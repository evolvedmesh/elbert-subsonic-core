//! Reading the library: everything a client browses, lists and searches, as
//! the account asking may see it.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::api::{Answer, Params};
use super::auth::Who;
use super::backend::{PlaylistInfo, UserNotes};
use super::catalog::{
    fold, index_letter, suffix_of, virtual_path, Album, Artist, Catalog, Song, IGNORED_ARTICLES,
};
use super::respond::{iso8601, ApiError, ERR_GENERIC};
use super::util::audio_mime;
use super::{wall_ms, Server};

/// A "now playing" entry nobody has replaced this long ago is over.
const NOW_PLAYING_MS: i64 = 15 * 60 * 1000;

fn page<T>(items: Vec<T>, offset: usize, size: usize) -> Vec<T> {
    items.into_iter().skip(offset).take(size).collect()
}

/// A small generator seeded from the system: shuffling needs nothing better.
pub struct Shuffler(u64);

impl Default for Shuffler {
    fn default() -> Self {
        Self::new()
    }
}

impl Shuffler {
    /// A generator seeded from the system.
    pub fn new() -> Self {
        let mut seed = [0u8; 8];
        let _ = getrandom::fill(&mut seed);
        Shuffler(u64::from_ne_bytes(seed) | 1)
    }

    /// A number below `n`, or 0 when `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    /// The first `take` of `items`, shuffled, without shuffling the rest.
    pub fn sample<T>(&mut self, items: &mut [T], take: usize) {
        let n = items.len();
        for i in 0..take.min(n) {
            let j = i + self.below(n - i);
            items.swap(i, j);
        }
    }
}

/// One request's reading of the library.
pub struct View<'a> {
    /// The server.
    pub srv: &'a Arc<Server>,
    /// The catalog this view browses.
    pub cat: &'a Catalog,
    /// The account that is asking.
    pub who: &'a Who,
    /// What the account has noted: stars, ratings, plays, bookmarks, queue, playlists.
    pub notes: &'a UserNotes,
}

impl View<'_> {
    /// The folders to look in: what the account may see, narrowed by
    /// `musicFolderId` when given.
    fn mask(&self, p: &Params) -> u64 {
        match p.int("musicFolderId") {
            Some(f) if f >= 1 => self.who.mask & (1 << (f as u32 - 1).min(63)),
            _ => self.who.mask,
        }
    }

    /// Whether the account's music folders include `song`'s.
    pub fn song_visible(&self, song: &Song) -> bool {
        self.who.mask & (1 << song.folder.min(63)) != 0
    }

    /// A song this account may see, by id.
    pub fn visible_song(&self, id: &str) -> Option<&Song> {
        self.cat.song(id).filter(|s| self.song_visible(s))
    }

    fn visible_album(&self, id: &str) -> Option<&Album> {
        self.cat
            .album(id)
            .filter(|a| a.folders & self.who.mask != 0)
    }

    fn visible_artist(&self, id: &str) -> Option<&Artist> {
        self.cat
            .artist(id)
            .filter(|a| a.folders & self.who.mask != 0)
    }

    fn plays_of(&self, id: &str) -> (u64, i64) {
        self.notes.plays.get(id).copied().unwrap_or((0, 0))
    }

    fn song_star(&self, id: &str) -> Option<i64> {
        self.notes.song_stars.get(id).copied()
    }

    fn song_rating(&self, id: &str) -> Option<u8> {
        self.notes.song_ratings.get(id).copied()
    }

    fn album_plays(&self, album: &Album) -> (u64, i64) {
        album.songs.iter().fold((0, 0), |acc, &i| {
            let (n, last) = self.plays_of(&self.cat.songs[i as usize].id);
            (acc.0 + n, acc.1.max(last))
        })
    }

    /// `song` as a `child` object.
    pub fn song_json(&self, song: &Song) -> Value {
        let mut m = Map::new();
        let artist_entry = &self.cat.artists[song.artist_at as usize];
        m.insert("id".into(), (*song.id).into());
        m.insert(
            "parent".into(),
            (*self.cat.albums[song.album_at as usize].id).into(),
        );
        m.insert("isDir".into(), false.into());
        m.insert("title".into(), (*song.title).into());
        m.insert("album".into(), (*song.album).into());
        m.insert("artist".into(), (*song.artist).into());
        m.insert("albumArtist".into(), (*song.album_artist).into());
        m.insert("displayArtist".into(), (*song.artist).into());
        m.insert("displayAlbumArtist".into(), (*song.album_artist).into());
        m.insert(
            "albumId".into(),
            (*self.cat.albums[song.album_at as usize].id).into(),
        );
        m.insert("artistId".into(), (*artist_entry.id).into());
        m.insert(
            "artists".into(),
            json!([{ "id": &*artist_entry.id, "name": &*song.artist }]),
        );
        m.insert(
            "albumArtists".into(),
            json!([{ "id": &*artist_entry.id, "name": &*artist_entry.name }]),
        );
        m.insert("type".into(), "music".into());
        m.insert("mediaType".into(), "song".into());
        m.insert("isVideo".into(), false.into());
        m.insert("duration".into(), (song.duration_ms / 1000).into());
        m.insert("path".into(), virtual_path(song).into());
        m.insert("suffix".into(), suffix_of(&song.path).into());
        m.insert("contentType".into(), audio_mime(&song.path).into());
        m.insert("created".into(), iso8601(song.added_ms).into());
        if song.track > 0 {
            m.insert("track".into(), song.track.into());
        }
        if song.disc > 0 {
            m.insert("discNumber".into(), song.disc.into());
        }
        if song.year > 0 {
            m.insert("year".into(), song.year.into());
        }
        if song.bitrate > 0 {
            m.insert("bitRate".into(), song.bitrate.into());
        }
        if song.sample_rate > 0 {
            m.insert("samplingRate".into(), song.sample_rate.into());
        }
        if song.size > 0 {
            m.insert("size".into(), song.size.into());
        }
        if !song.genre.is_empty() {
            m.insert("genre".into(), (*song.genre).into());
            m.insert("genres".into(), json!([{ "name": &*song.genre }]));
        }
        if let Some(isrc) = &song.isrc {
            m.insert("isrc".into(), json!([&**isrc]));
        }
        if let Some(cover) = self.cat.album_cover_of(song) {
            m.insert("coverArt".into(), (*cover).into());
        }
        if let Some(when) = self.song_star(&song.id) {
            m.insert("starred".into(), iso8601(when).into());
        }
        if let Some(rating) = self.song_rating(&song.id) {
            m.insert("userRating".into(), rating.into());
        }
        let (plays, last) = self.plays_of(&song.id);
        if plays > 0 {
            m.insert("playCount".into(), plays.into());
        }
        if last > 0 {
            m.insert("played".into(), iso8601(last).into());
        }
        Value::Object(m)
    }

    /// `songs` as `child` objects, the ones the account may see.
    pub fn songs_json<'s>(&self, songs: impl IntoIterator<Item = &'s Song>) -> Vec<Value> {
        songs.into_iter().map(|s| self.song_json(s)).collect()
    }

    /// An album both ways the API describes one (folder and tag), which
    /// clients of either kind read without noticing the other's fields.
    fn album_json(&self, album: &Album) -> Value {
        let mut m = Map::new();
        let artist = &self.cat.artists[album.artist_at as usize];
        m.insert("id".into(), (*album.id).into());
        m.insert("parent".into(), (*artist.id).into());
        m.insert("isDir".into(), true.into());
        m.insert("name".into(), (*album.name).into());
        m.insert("title".into(), (*album.name).into());
        m.insert("album".into(), (*album.name).into());
        m.insert("artist".into(), (*album.artist).into());
        m.insert("displayArtist".into(), (*album.artist).into());
        m.insert("artistId".into(), (*artist.id).into());
        m.insert(
            "artists".into(),
            json!([{ "id": &*artist.id, "name": &*album.artist }]),
        );
        m.insert("songCount".into(), album.songs.len().into());
        m.insert("duration".into(), (album.duration_ms / 1000).into());
        m.insert("created".into(), iso8601(album.added_ms).into());
        m.insert("mediaType".into(), "album".into());
        m.insert("sortName".into(), (*album.sort).into());
        if album.year > 0 {
            m.insert("year".into(), album.year.into());
        }
        if !album.genre.is_empty() {
            m.insert("genre".into(), (*album.genre).into());
            m.insert("genres".into(), json!([{ "name": &*album.genre }]));
        }
        if let Some(cover) = &album.cover {
            m.insert("coverArt".into(), (**cover).into());
        }
        if let Some(&when) = self.notes.album_stars.get(&*album.id) {
            m.insert("starred".into(), iso8601(when).into());
        }
        if let Some(&rating) = self.notes.album_ratings.get(&*album.id) {
            m.insert("userRating".into(), rating.into());
        }
        let (plays, last) = self.album_plays(album);
        if plays > 0 {
            m.insert("playCount".into(), plays.into());
        }
        if last > 0 {
            m.insert("played".into(), iso8601(last).into());
        }
        Value::Object(m)
    }

    fn artist_json(&self, artist: &Artist) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), (*artist.id).into());
        m.insert("name".into(), (*artist.name).into());
        m.insert("albumCount".into(), artist.albums.len().into());
        m.insert("sortName".into(), (*artist.sort).into());
        if let Some(cover) = &artist.cover {
            m.insert("coverArt".into(), (**cover).into());
        }
        if let Some(&when) = self.notes.artist_stars.get(&*artist.id) {
            m.insert("starred".into(), iso8601(when).into());
        }
        if let Some(&rating) = self.notes.artist_ratings.get(&*artist.id) {
            m.insert("userRating".into(), rating.into());
        }
        Value::Object(m)
    }

    /// `getMusicFolders`.
    pub fn music_folders(&self) -> Answer {
        let folders: Vec<Value> = self
            .cat
            .folders
            .iter()
            .enumerate()
            .filter(|(i, _)| self.who.mask & (1 << (*i).min(63)) != 0)
            .map(|(i, path)| {
                let name = if path.is_empty() {
                    "Elsewhere".to_string()
                } else {
                    let leaf = std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned());
                    leaf.unwrap_or_else(|| path.clone())
                };
                json!({ "id": i + 1, "name": name })
            })
            .collect();
        Ok(json!({ "musicFolders": { "musicFolder": folders } }))
    }

    /// Artists by first letter, in order, for `getIndexes` and `getArtists`.
    fn index_groups(&self, mask: u64) -> Vec<Value> {
        let mut artists: Vec<&Artist> = self
            .cat
            .artists
            .iter()
            .filter(|a| a.folders & mask != 0)
            .collect();
        artists.sort_by(|a, b| a.sort.cmp(&b.sort));
        let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
        for artist in artists {
            let letter = index_letter(&artist.sort);
            if groups.last().is_none_or(|(l, _)| *l != letter) {
                groups.push((letter, Vec::new()));
            }
            groups.last_mut().unwrap().1.push(self.artist_json(artist));
        }
        groups
            .into_iter()
            .map(|(name, artist)| json!({ "name": name, "artist": artist }))
            .collect()
    }

    /// `getIndexes`.
    pub fn indexes(&self, p: &Params) -> Answer {
        let modified = self.cat.built_ms;
        if p.int("ifModifiedSince")
            .is_some_and(|since| since >= modified)
        {
            return Ok(
                json!({ "indexes": { "lastModified": modified, "ignoredArticles": IGNORED_ARTICLES } }),
            );
        }
        Ok(json!({ "indexes": {
            "lastModified": modified,
            "ignoredArticles": IGNORED_ARTICLES,
            "index": self.index_groups(self.mask(p)),
        }}))
    }

    /// `getArtists`.
    pub fn artists(&self, p: &Params) -> Answer {
        Ok(json!({ "artists": {
            "ignoredArticles": IGNORED_ARTICLES,
            "index": self.index_groups(self.mask(p)),
        }}))
    }

    /// `getMusicDirectory`.
    pub fn directory(&self, p: &Params) -> Answer {
        let id = p.need("id")?;
        if let Some(album) = self.visible_album(id) {
            let artist = &self.cat.artists[album.artist_at as usize];
            let songs = album
                .songs
                .iter()
                .map(|&i| &self.cat.songs[i as usize])
                .filter(|s| self.song_visible(s));
            return Ok(json!({ "directory": {
                "id": &*album.id,
                "parent": &*artist.id,
                "name": &*album.name,
                "child": self.songs_json(songs),
            }}));
        }
        if let Some(artist) = self.visible_artist(id) {
            let albums: Vec<Value> = artist
                .albums
                .iter()
                .map(|&i| &self.cat.albums[i as usize])
                .filter(|a| a.folders & self.who.mask != 0)
                .map(|a| self.album_json(a))
                .collect();
            return Ok(
                json!({ "directory": { "id": &*artist.id, "name": &*artist.name, "child": albums } }),
            );
        }
        Err(ApiError::not_found("Directory"))
    }

    /// `getGenres`.
    pub fn genres(&self) -> Answer {
        let genres: Vec<Value> = self
            .cat
            .genres
            .iter()
            .map(|g| json!({ "value": &*g.name, "songCount": g.songs, "albumCount": g.albums }))
            .collect();
        Ok(json!({ "genres": { "genre": genres } }))
    }

    /// `getArtist`.
    pub fn artist(&self, p: &Params) -> Answer {
        let artist = self
            .visible_artist(p.need("id")?)
            .ok_or_else(|| ApiError::not_found("Artist"))?;
        let mut value = self.artist_json(artist);
        let albums: Vec<Value> = artist
            .albums
            .iter()
            .map(|&i| &self.cat.albums[i as usize])
            .filter(|a| a.folders & self.who.mask != 0)
            .map(|a| self.album_json(a))
            .collect();
        value["album"] = albums.into();
        Ok(json!({ "artist": value }))
    }

    /// `getAlbum`.
    pub fn album(&self, p: &Params) -> Answer {
        let album = self
            .visible_album(p.need("id")?)
            .ok_or_else(|| ApiError::not_found("Album"))?;
        let mut value = self.album_json(album);
        let songs = album
            .songs
            .iter()
            .map(|&i| &self.cat.songs[i as usize])
            .filter(|s| self.song_visible(s));
        value["song"] = self.songs_json(songs).into();
        Ok(json!({ "album": value }))
    }

    /// `getSong`.
    pub fn song(&self, p: &Params) -> Answer {
        let song = self
            .visible_song(p.need("id")?)
            .ok_or_else(|| ApiError::not_found("Song"))?;
        Ok(json!({ "song": self.song_json(song) }))
    }

    /// Who else shares a genre comes from the library. The rest is the app's to
    /// look up (Last.fm, when the host has signed in): cached, and absent
    /// rather than an error when it can't.
    pub async fn artist_info(&self, p: &Params, key: &str) -> Answer {
        let artist = self
            .visible_artist(p.need("id")?)
            .ok_or_else(|| ApiError::not_found("Artist"))?;
        let count = p.count("count", 20, 100);
        let genres: HashSet<&str> = artist
            .albums
            .iter()
            .map(|&i| &*self.cat.albums[i as usize].genre)
            .filter(|g| !g.is_empty())
            .collect();
        let similar: Vec<Value> = self
            .cat
            .artists
            .iter()
            .filter(|other| other.id != artist.id && other.folders & self.who.mask != 0)
            .filter(|other| {
                other
                    .albums
                    .iter()
                    .any(|&i| genres.contains(&*self.cat.albums[i as usize].genre))
            })
            .take(count)
            .map(|other| self.artist_json(other))
            .collect();
        let mut info = self.looked_up(0, &artist.name, "").await;
        info["similarArtist"] = similar.into();
        let mut out = Map::new();
        out.insert(key.into(), info);
        Ok(Value::Object(out))
    }

    /// `getAlbumInfo` and `getAlbumInfo2`.
    pub async fn album_info(&self, p: &Params) -> Answer {
        let id = p.need("id")?;
        let album = self
            .visible_album(id)
            .or_else(|| {
                self.visible_song(id)
                    .map(|s| &self.cat.albums[s.album_at as usize])
            })
            .ok_or_else(|| ApiError::not_found("Album"))?;
        Ok(json!({ "albumInfo": self.looked_up(1, &album.artist, &album.name).await }))
    }

    /// What Dart found about an artist (`kind` 0) or album (1), as JSON.
    async fn looked_up(&self, kind: u8, artist: &str, album: &str) -> Value {
        let key = format!("{}\0{kind}\0{artist}\0{album}", self.who.tenant.0);
        if let Some(known) = self.srv.info.lock().unwrap().get(&key).cloned() {
            return known;
        }
        let found = self
            .srv
            .backend
            .artist_info(self.who, artist, (kind == 1).then_some(album))
            .await
            .ok()
            .flatten();
        let value: Value = found
            .map(|i| i.0)
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        // Nothing yet may be something once the host has looked it up.
        if value.as_object().is_some_and(|o| !o.is_empty()) {
            self.srv.info.lock().unwrap().insert(key, value.clone());
        }
        value
    }

    /// The same artist first, then the same genre, shuffled.
    pub fn similar(&self, p: &Params, key: &str) -> Answer {
        let id = p.need("id")?;
        let count = p.count("count", 50, 500);
        let (artist_at, genre, skip): (Option<u32>, &str, Option<&str>) =
            if let Some(song) = self.visible_song(id) {
                (Some(song.artist_at), &song.genre, Some(&song.id))
            } else if let Some(album) = self.visible_album(id) {
                (Some(album.artist_at), &album.genre, None)
            } else if let Some(artist) = self.visible_artist(id) {
                let at = self
                    .cat
                    .artists
                    .iter()
                    .position(|a| a.id == artist.id)
                    .map(|i| i as u32);
                let genre = artist
                    .albums
                    .first()
                    .map(|&i| &*self.cat.albums[i as usize].genre)
                    .unwrap_or("");
                (at, genre, None)
            } else {
                return Err(ApiError::not_found("Song"));
            };
        let mut rng = Shuffler::new();
        let mut same: Vec<&Song> = Vec::new();
        let mut near: Vec<&Song> = Vec::new();
        for song in &self.cat.songs {
            if Some(&*song.id) == skip || !self.song_visible(song) {
                continue;
            }
            if Some(song.artist_at) == artist_at {
                same.push(song);
            } else if !genre.is_empty() && &*song.genre == genre {
                near.push(song);
            }
        }
        rng.sample(&mut same, count / 2);
        same.truncate(count / 2);
        let want = count - same.len();
        rng.sample(&mut near, want);
        near.truncate(want);
        same.extend(near);
        let len = same.len();
        rng.sample(&mut same, len);
        let mut out = Map::new();
        out.insert(key.into(), json!({ "song": self.songs_json(same) }));
        Ok(Value::Object(out))
    }

    /// `id` (the `topSongsByArtistId` extension) wins over `artist`.
    pub fn top_songs(&self, p: &Params) -> Answer {
        let count = p.count("count", 50, 500);
        let name = match p.get("id") {
            Some(id) => fold(
                &self
                    .visible_artist(id)
                    .ok_or_else(|| ApiError::not_found("Artist"))?
                    .name,
            ),
            None => fold(p.need("artist")?),
        };
        let songs: Vec<&Song> = self
            .cat
            .songs
            .iter()
            .filter(|s| {
                self.song_visible(s) && (*s.fold_artist == *name || fold(&s.album_artist) == name)
            })
            .collect();
        let mut ranked: Vec<(u64, u8, usize)> = songs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    self.plays_of(&s.id).0,
                    self.song_rating(&s.id).unwrap_or(0),
                    i,
                )
            })
            .collect();
        ranked.sort_by_key(|&(plays, rating, i)| (Reverse(plays), Reverse(rating), i));
        let picked: Vec<&Song> = ranked
            .into_iter()
            .take(count)
            .map(|(_, _, i)| songs[i])
            .collect();
        Ok(json!({ "topSongs": { "song": self.songs_json(picked) } }))
    }

    /// `getAlbumList` and `getAlbumList2`; `key` is the name of the response element.
    pub fn album_list(&self, p: &Params, key: &str) -> Answer {
        let kind = p.need("type")?;
        let size = p.count("size", 10, 500);
        let offset = p.offset("offset");
        let mask = self.mask(p);
        let mut albums: Vec<&Album> = self
            .cat
            .albums
            .iter()
            .filter(|a| a.folders & mask != 0)
            .collect();
        match kind {
            "random" => {
                Shuffler::new().sample(&mut albums, size);
                albums.truncate(size);
            }
            "newest" => albums.sort_by_key(|a| Reverse(a.added_ms)),
            "alphabeticalByName" => albums.sort_by(|a, b| a.sort.cmp(&b.sort)),
            "alphabeticalByArtist" => albums.sort_by(|a, b| {
                let (x, y) = (
                    &self.cat.artists[a.artist_at as usize].sort,
                    &self.cat.artists[b.artist_at as usize].sort,
                );
                x.cmp(y)
                    .then_with(|| a.year.cmp(&b.year))
                    .then_with(|| a.sort.cmp(&b.sort))
            }),
            "frequent" => {
                let mut ranked: Vec<(u64, &Album)> = albums
                    .into_iter()
                    .map(|a| (self.album_plays(a).0, a))
                    .filter(|(n, _)| *n > 0)
                    .collect();
                ranked.sort_by_key(|(n, _)| Reverse(*n));
                albums = ranked.into_iter().map(|(_, a)| a).collect();
            }
            "recent" => {
                let mut ranked: Vec<(i64, &Album)> = albums
                    .into_iter()
                    .map(|a| (self.album_plays(a).1, a))
                    .filter(|(t, _)| *t > 0)
                    .collect();
                ranked.sort_by_key(|(t, _)| Reverse(*t));
                albums = ranked.into_iter().map(|(_, a)| a).collect();
            }
            "highest" => {
                let mut ranked: Vec<(u32, &Album)> = albums
                    .into_iter()
                    .map(|a| (self.album_rating(a), a))
                    .filter(|(r, _)| *r > 0)
                    .collect();
                ranked.sort_by_key(|(r, _)| Reverse(*r));
                albums = ranked.into_iter().map(|(_, a)| a).collect();
            }
            "starred" => {
                albums.retain(|a| self.notes.album_stars.contains_key(&*a.id));
                albums.sort_by(|a, b| a.sort.cmp(&b.sort));
            }
            "byYear" => {
                let from =
                    p.int("fromYear")
                        .ok_or_else(|| ApiError::missing("fromYear"))? as u32;
                let to = p.int("toYear").ok_or_else(|| ApiError::missing("toYear"))? as u32;
                let (low, high) = (from.min(to), from.max(to));
                albums.retain(|a| a.year >= low && a.year <= high);
                if from > to {
                    albums.sort_by_key(|a| Reverse(a.year));
                } else {
                    albums.sort_by_key(|a| a.year);
                }
            }
            "byGenre" => {
                let genre = p.need("genre")?;
                albums.retain(|a| &*a.genre == genre);
                albums.sort_by(|a, b| a.sort.cmp(&b.sort));
            }
            other => return Err(ApiError(ERR_GENERIC, format!("Unknown list type: {other}"))),
        }
        let offset = if kind == "random" { 0 } else { offset };
        let picked: Vec<Value> = page(albums, offset, size)
            .into_iter()
            .map(|a| self.album_json(a))
            .collect();
        let mut out = Map::new();
        out.insert(key.into(), json!({ "album": picked }));
        Ok(Value::Object(out))
    }

    /// Its own rating, else its songs' average, in hundredths for sorting.
    fn album_rating(&self, album: &Album) -> u32 {
        let own = self.notes.album_ratings.get(&*album.id).copied();
        if let Some(own) = own {
            return own as u32 * 100;
        }
        let rated: Vec<u32> = album
            .songs
            .iter()
            .filter_map(|&i| {
                self.song_rating(&self.cat.songs[i as usize].id)
                    .map(|r| r as u32)
            })
            .collect();
        if rated.is_empty() {
            0
        } else {
            rated.iter().sum::<u32>() * 100 / rated.len() as u32
        }
    }

    /// `getRandomSongs`.
    pub fn random_songs(&self, p: &Params) -> Answer {
        let size = p.count("size", 10, 500);
        let mask = self.mask(p);
        let genre = p.get("genre");
        let from = p.int("fromYear").unwrap_or(0) as u32;
        let to = p.int("toYear").map(|y| y as u32).unwrap_or(u32::MAX);
        let mut songs: Vec<&Song> = self
            .cat
            .songs
            .iter()
            .filter(|s| mask & (1 << s.folder.min(63)) != 0)
            .filter(|s| genre.is_none_or(|g| &*s.genre == g))
            .filter(|s| (from == 0 && to == u32::MAX) || (s.year >= from && s.year <= to))
            .collect();
        Shuffler::new().sample(&mut songs, size);
        songs.truncate(size);
        Ok(json!({ "randomSongs": { "song": self.songs_json(songs) } }))
    }

    /// `getSongsByGenre`.
    pub fn songs_by_genre(&self, p: &Params) -> Answer {
        let genre = p.need("genre")?;
        let mask = self.mask(p);
        let songs: Vec<&Song> = self
            .cat
            .songs
            .iter()
            .filter(|s| &*s.genre == genre && mask & (1 << s.folder.min(63)) != 0)
            .skip(p.offset("offset"))
            .take(p.count("count", 10, 500))
            .collect();
        Ok(json!({ "songsByGenre": { "song": self.songs_json(songs) } }))
    }

    /// `getNowPlaying`.
    pub fn now_playing(&self) -> Answer {
        let now = wall_ms();
        let mut playing = self.srv.now_playing.lock().unwrap();
        playing.retain(|e| now - e.updated_ms < NOW_PLAYING_MS);
        // A managed host serves many accounts from one server: each sees only its own plays.
        let own_only = self.srv.backend.capabilities().managed;
        let entries: Vec<Value> = playing
            .iter()
            .filter(|e| !own_only || e.user == self.who.name)
            .filter_map(|e| {
                let mut value = self.song_json(self.visible_song(&e.song)?);
                value["username"] = e.user.clone().into();
                value["minutesAgo"] = ((now - e.since_ms) / 60_000).into();
                value["playerId"] = 0.into();
                value["playerName"] = e.client.clone().into();
                if let Some(state) = &e.state {
                    // The position moves on between reports, at the client's speed.
                    let moved = if state == "playing" {
                        (now - e.updated_ms).max(0) as f64 * e.rate
                    } else {
                        0.0
                    };
                    value["state"] = state.clone().into();
                    value["positionMs"] = (e.position_ms + moved as u64).into();
                    value["playbackRate"] = e.rate.into();
                }
                Some(value)
            })
            .collect();
        Ok(json!({ "nowPlaying": { "entry": entries } }))
    }

    /// `getStarred` and `getStarred2`; `key` is the name of the response element.
    pub fn starred(&self, p: &Params, key: &str) -> Answer {
        let mask = self.mask(p);
        let songs: Vec<&Song> = self
            .cat
            .songs
            .iter()
            .filter(|s| mask & (1 << s.folder.min(63)) != 0 && self.song_star(&s.id).is_some())
            .collect();
        let albums: Vec<&Album> = self
            .cat
            .albums
            .iter()
            .filter(|a| a.folders & mask != 0 && self.notes.album_stars.contains_key(&*a.id))
            .collect();
        let artists: Vec<&Artist> = self
            .cat
            .artists
            .iter()
            .filter(|a| a.folders & mask != 0 && self.notes.artist_stars.contains_key(&*a.id))
            .collect();
        let mut out = Map::new();
        out.insert(
            key.into(),
            json!({
                "artist": artists.into_iter().map(|a| self.artist_json(a)).collect::<Vec<_>>(),
                "album": albums.into_iter().map(|a| self.album_json(a)).collect::<Vec<_>>(),
                "song": self.songs_json(songs),
            }),
        );
        Ok(Value::Object(out))
    }

    /// `search2`/`search3`. An empty query lists everything, in a stable
    /// order, which is how several clients sync a whole library by paging.
    pub fn search(&self, p: &Params, key: &str) -> Answer {
        let query = p.get("query").unwrap_or("");
        let words = search_words(query);
        let mask = self.mask(p);
        let artists = rank(
            self.cat.artists.iter().filter(|a| a.folders & mask != 0),
            &words,
            |a| (&a.search, &a.search),
        );
        let albums = rank(
            self.cat.albums.iter().filter(|a| a.folders & mask != 0),
            &words,
            |a| (&a.search, &a.search),
        );
        let songs = rank(
            self.cat
                .songs
                .iter()
                .filter(|s| mask & (1 << s.folder.min(63)) != 0),
            &words,
            |s| (&s.search, &s.fold_title),
        );
        let artists = page(
            artists,
            p.offset("artistOffset"),
            p.count("artistCount", 20, 500),
        );
        let albums = page(
            albums,
            p.offset("albumOffset"),
            p.count("albumCount", 20, 500),
        );
        let songs = page(songs, p.offset("songOffset"), p.count("songCount", 20, 500));
        let mut out = Map::new();
        out.insert(
            key.into(),
            json!({
                "artist": artists.into_iter().map(|a| self.artist_json(a)).collect::<Vec<_>>(),
                "album": albums.into_iter().map(|a| self.album_json(a)).collect::<Vec<_>>(),
                "song": self.songs_json(songs),
            }),
        );
        Ok(Value::Object(out))
    }

    /// The original search: `artist`, `album`, `title` or `any`.
    pub fn search_old(&self, p: &Params) -> Answer {
        let word = |key: &str| p.get(key).map(fold).filter(|w| !w.is_empty());
        let (artist, album, title, any) =
            (word("artist"), word("album"), word("title"), word("any"));
        let count = p.count("count", 20, 500);
        let offset = p.offset("offset");
        let matches: Vec<&Song> = self
            .cat
            .songs
            .iter()
            .filter(|s| {
                self.song_visible(s)
                    && artist.as_deref().is_none_or(|w| s.fold_artist.contains(w))
                    && album.as_deref().is_none_or(|w| fold(&s.album).contains(w))
                    && title.as_deref().is_none_or(|w| s.fold_title.contains(w))
                    && any.as_deref().is_none_or(|w| s.search.contains(w))
            })
            .collect();
        let total = matches.len();
        let picked = page(matches, offset, count);
        Ok(
            json!({ "searchResult": { "offset": offset, "totalHits": total, "match": self.songs_json(picked) } }),
        )
    }

    /// Every playlist this account can see: the app's (the host's own) when
    /// it is the host or they are public, and those kept elsewhere when they are
    /// its own, public, or it is an administrator.
    fn visible_playlists(&self) -> Vec<Listed<'_>> {
        let mut out = Vec::new();
        for playlist in &self.notes.playlists {
            let readonly = if playlist.app {
                if !self.who.owner && !playlist.public {
                    continue;
                }
                playlist.readonly || !self.who.owner
            } else {
                if playlist.owner != self.who.name && !playlist.public && !self.who.account.admin {
                    continue;
                }
                playlist.owner != self.who.name && !self.who.account.admin
            };
            out.push(Listed {
                info: playlist,
                readonly,
            });
        }
        out
    }

    fn playlist_json(&self, listed: &Listed, with_songs: bool) -> Value {
        let playlist = listed.info;
        let songs: Vec<&Song> = playlist
            .songs
            .iter()
            .filter_map(|s| self.visible_song(s))
            .collect();
        let duration: u64 = songs.iter().map(|s| s.duration_ms / 1000).sum();
        let mut value = json!({
            "id": &playlist.id,
            "name": &playlist.name,
            "owner": &playlist.owner,
            "public": playlist.public,
            "readonly": listed.readonly,
            "songCount": songs.len(),
            "duration": duration,
            "created": iso8601(playlist.created_ms),
            "changed": iso8601(playlist.changed_ms),
        });
        if !playlist.comment.is_empty() {
            value["comment"] = playlist.comment.clone().into();
        }
        if let Some(cover) = songs.iter().find_map(|s| self.cat.album_cover_of(s)) {
            value["coverArt"] = (*cover).into();
        }
        if with_songs {
            value["entry"] = self.songs_json(songs).into();
        }
        value
    }

    /// `getPlaylists`.
    pub fn playlists(&self, p: &Params) -> Answer {
        if let Some(name) = p.get("username") {
            if name != self.who.name {
                self.who.admin("Reading another account's playlists")?;
            }
        }
        let wanted = p.get("username");
        let list: Vec<Value> = self
            .visible_playlists()
            .iter()
            .filter(|l| wanted.is_none_or(|w| l.info.owner == w))
            .map(|l| self.playlist_json(l, false))
            .collect();
        Ok(json!({ "playlists": { "playlist": list } }))
    }

    /// The cover of the playlist `id`: the cover of its first song.
    pub fn playlist_cover(&self, id: &str) -> Option<Arc<str>> {
        let all = self.visible_playlists();
        let found = all.iter().find(|l| l.info.id == id)?;
        found
            .info
            .songs
            .iter()
            .filter_map(|s| self.visible_song(s))
            .find_map(|s| self.cat.album_cover_of(s))
    }

    /// `getPlaylist`.
    pub fn playlist(&self, p: &Params) -> Answer {
        self.playlist_by_id(p.need("id")?)
    }

    /// The playlist `id`, as `getPlaylist` answers.
    pub fn playlist_by_id(&self, id: &str) -> Answer {
        let all = self.visible_playlists();
        let found = all
            .iter()
            .find(|l| l.info.id == id)
            .ok_or_else(|| ApiError::not_found("Playlist"))?;
        Ok(json!({ "playlist": self.playlist_json(found, true) }))
    }

    /// `getBookmarks`.
    pub fn bookmarks(&self) -> Answer {
        let saved = &self.notes.bookmarks;
        let list: Vec<Value> = saved
            .iter()
            .filter_map(|(id, b)| {
                Some(json!({
                    "position": b.position_ms,
                    "username": &self.who.name,
                    "comment": &b.comment,
                    "created": iso8601(b.created_ms),
                    "changed": iso8601(b.changed_ms),
                    "entry": self.song_json(self.visible_song(id)?),
                }))
            })
            .collect();
        Ok(json!({ "bookmarks": { "bookmark": list } }))
    }

    fn saved_queue(&self) -> Option<&super::state::Queue> {
        self.notes.queue.as_ref()
    }

    /// `getPlayQueue`.
    pub fn play_queue(&self) -> Answer {
        let Some(queue) = self.saved_queue() else {
            return Ok(json!({}));
        };
        let entries: Vec<Value> = queue
            .ids
            .iter()
            .filter_map(|id| self.visible_song(id))
            .map(|s| self.song_json(s))
            .collect();
        let mut value = json!({
            "position": queue.position_ms,
            "username": &self.who.name,
            "changed": iso8601(queue.changed_ms),
            "changedBy": queue.changed_by,
            "entry": entries,
        });
        if let Some(current) = queue.current.and_then(|i| queue.ids.get(i)) {
            value["current"] = current.clone().into();
        }
        Ok(json!({ "playQueue": value }))
    }

    /// `getPlayQueueByIndex`.
    pub fn play_queue_by_index(&self) -> Answer {
        let Some(queue) = self.saved_queue() else {
            return Ok(json!({}));
        };
        let entries: Vec<Value> = queue
            .ids
            .iter()
            .filter_map(|id| self.visible_song(id))
            .map(|s| self.song_json(s))
            .collect();
        let mut value = json!({
            "position": queue.position_ms,
            "username": &self.who.name,
            "changed": iso8601(queue.changed_ms),
            "changedBy": queue.changed_by,
            "entry": entries,
        });
        if let Some(current) = queue.current {
            value["currentIndex"] = current.into();
        }
        Ok(json!({ "playQueueByIndex": value }))
    }

    /// `getScanStatus`.
    pub fn scan_status(&self) -> Answer {
        let (scanning, count) = *self.srv.scan.lock().unwrap();
        Ok(json!({ "scanStatus": { "scanning": scanning, "count": count } }))
    }
}

/// A playlist as an account sees it: what the backend holds, and whether this account
/// may not change it.
struct Listed<'a> {
    info: &'a PlaylistInfo,
    readonly: bool,
}

fn search_words(query: &str) -> Vec<String> {
    let cleaned = query.trim().trim_matches('"').trim_end_matches('*');
    fold(cleaned)
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// What matches every word, those whose name starts with the query first.
/// Stable, so paging through the results is too.
fn rank<'a, T>(
    items: impl Iterator<Item = &'a T>,
    words: &[String],
    text: impl Fn(&'a T) -> (&'a str, &'a str),
) -> Vec<&'a T>
where
    T: 'a,
{
    if words.is_empty() {
        return items.collect();
    }
    let first = words.join(" ");
    let mut hits: Vec<(bool, &T)> = items
        .filter_map(|item| {
            let (haystack, name) = text(item);
            words
                .iter()
                .all(|w| haystack.contains(w.as_str()))
                .then(|| (!name.starts_with(&first), item))
        })
        .collect();
    hits.sort_by_key(|(later, _)| *later);
    hits.into_iter().map(|(_, item)| item).collect()
}
