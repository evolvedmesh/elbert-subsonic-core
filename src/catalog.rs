//! The library as a Subsonic client browses it: songs grouped into albums and
//! artists, built once from what Dart hands over and then only read.
//!
//! A rebuild replaces the whole thing behind an `Arc`, so a request in flight
//! keeps the catalog it started with. Ids are stable across rebuilds and
//! restarts: a song keeps the library's id, and an album or artist id is a
//! hash of its name, so a client's cache survives both.

// The fields are plain data named after what they hold; the types and methods are documented.
#![allow(missing_docs)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha1::{Digest, Sha1};

/// One track, as the catalog holds it. The `*_at` fields index into [`Catalog`]'s vectors.
pub struct Song {
    pub id: Arc<str>,
    pub path: PathBuf,
    pub title: Arc<str>,
    pub artist: Arc<str>,
    pub album: Arc<str>,
    pub album_artist: Arc<str>,
    pub genre: Arc<str>,
    pub cover: Option<Arc<str>>,
    pub isrc: Option<Box<str>>,
    pub duration_ms: u64,
    pub track: u32,
    pub disc: u32,
    pub year: u32,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub size: u64,
    pub added_ms: i64,
    pub album_at: u32,
    pub artist_at: u32,
    pub folder: u32,
    /// Title, artist, album and album artist, folded, for searching.
    pub search: Box<str>,
    pub fold_title: Box<str>,
    pub fold_artist: Box<str>,
}

/// One album: the songs that share an album name and album artist.
pub struct Album {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub artist: Arc<str>,
    pub artist_at: u32,
    pub songs: Vec<u32>,
    pub year: u32,
    pub genre: Arc<str>,
    pub cover: Option<Arc<str>>,
    pub duration_ms: u64,
    pub added_ms: i64,
    /// Which music folders hold it: bit `n` for folder `n + 1`.
    pub folders: u64,
    pub sort: Box<str>,
    pub search: Box<str>,
}

/// One album artist.
pub struct Artist {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub albums: Vec<u32>,
    pub cover: Option<Arc<str>>,
    pub folders: u64,
    pub sort: Box<str>,
    pub search: Box<str>,
}

/// One genre, with how many songs and albums carry it.
pub struct Genre {
    pub name: Arc<str>,
    pub songs: u32,
    pub albums: u32,
}

/// The read model of one library: songs, albums, artists and genres, indexed by id.
/// Built once per library version with [`Catalog::build`] and shared.
#[derive(Default)]
pub struct Catalog {
    pub songs: Vec<Song>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub genres: Vec<Genre>,
    pub folders: Vec<String>,
    /// Cover id → what Dart calls that cover (a path, a URL, a MediaStore ref).
    pub covers: HashMap<Arc<str>, Arc<str>>,
    /// When this catalog was built, for `ifModifiedSince`.
    pub built_ms: i64,
    song_at: HashMap<Arc<str>, u32>,
    album_at: HashMap<Arc<str>, u32>,
    artist_at: HashMap<Arc<str>, u32>,
}

/// What Subsonic clients strip from the front of a name when sorting.
pub const IGNORED_ARTICLES: &str = "The El La Los Las Le Les Os As O A";

impl Catalog {
    /// The song with this id.
    pub fn song(&self, id: &str) -> Option<&Song> {
        self.song_at.get(id).map(|&i| &self.songs[i as usize])
    }

    /// The album with this id.
    pub fn album(&self, id: &str) -> Option<&Album> {
        self.album_at.get(id).map(|&i| &self.albums[i as usize])
    }

    /// The artist with this id.
    pub fn artist(&self, id: &str) -> Option<&Artist> {
        self.artist_at.get(id).map(|&i| &self.artists[i as usize])
    }

    /// The cover of the album `song` is on, falling back to the song's own cover.
    pub fn album_cover_of(&self, song: &Song) -> Option<Arc<str>> {
        song.cover
            .clone()
            .or_else(|| self.albums[song.album_at as usize].cover.clone())
    }

    /// Groups songs into albums and artists. `raw` may be in any order.
    pub fn build(mut folders: Vec<String>, mut raw: Vec<Raw>, built_ms: i64) -> Catalog {
        // A stable order, because clients page through search results to sync.
        let mut sort_keys: HashMap<Arc<str>, Box<str>> = HashMap::new();
        for s in &raw {
            for name in [&s.album_artist, &s.album] {
                sort_keys
                    .entry(name.clone())
                    .or_insert_with(|| sort_key(name));
            }
        }
        raw.sort_by(|a, b| {
            (
                &sort_keys[&a.album_artist],
                &sort_keys[&a.album],
                a.disc,
                a.track,
                &a.title,
                &a.id,
            )
                .cmp(&(
                    &sort_keys[&b.album_artist],
                    &sort_keys[&b.album],
                    b.disc,
                    b.track,
                    &b.title,
                    &b.id,
                ))
        });

        let roots: Vec<String> = folders.iter().map(|f| path_key(f)).collect();
        let mut elsewhere = false;

        let mut cat = Catalog {
            built_ms,
            ..Default::default()
        };
        let mut album_index: HashMap<(Arc<str>, Arc<str>), u32> = HashMap::new();
        let mut artist_index: HashMap<Arc<str>, u32> = HashMap::new();
        let mut cover_ids: HashMap<String, Arc<str>> = HashMap::new();
        let mut genre_index: HashMap<Arc<str>, (u32, HashSet<u32>)> = HashMap::new();

        for s in raw {
            let artist_at = *artist_index
                .entry(s.album_artist.clone())
                .or_insert_with(|| {
                    let at = cat.artists.len() as u32;
                    cat.artists.push(Artist {
                        id: hashed_id("ar-", &[&s.album_artist]),
                        name: s.album_artist.clone(),
                        albums: Vec::new(),
                        cover: None,
                        folders: 0,
                        sort: sort_keys[&s.album_artist].clone(),
                        search: fold(&s.album_artist).into(),
                    });
                    at
                });
            let album_at = *album_index
                .entry((s.album.clone(), s.album_artist.clone()))
                .or_insert_with(|| {
                    let at = cat.albums.len() as u32;
                    cat.albums.push(Album {
                        id: hashed_id("al-", &[&s.album, &s.album_artist]),
                        name: s.album.clone(),
                        artist: s.album_artist.clone(),
                        artist_at,
                        songs: Vec::new(),
                        year: 0,
                        genre: Arc::from(""),
                        cover: None,
                        duration_ms: 0,
                        added_ms: s.added_ms,
                        folders: 0,
                        sort: sort_keys[&s.album].clone(),
                        search: fold(&format!("{} {}", s.album, s.album_artist)).into(),
                    });
                    cat.artists[artist_at as usize].albums.push(at);
                    at
                });

            let cover = s.cover.as_deref().filter(|c| !c.is_empty()).map(|c| {
                cover_ids
                    .entry(c.to_string())
                    .or_insert_with(|| {
                        let id = hashed_id("cv-", &[c]);
                        cat.covers.insert(id.clone(), Arc::from(c));
                        id
                    })
                    .clone()
            });

            let key = path_key(&s.path);
            let folder = match roots.iter().position(|r| key.starts_with(r.as_str())) {
                Some(i) => i as u32,
                None => {
                    elsewhere = true;
                    roots.len() as u32
                }
            };

            let at = cat.songs.len() as u32;
            let album = &mut cat.albums[album_at as usize];
            album.songs.push(at);
            album.duration_ms += s.duration_ms;
            album.added_ms = album.added_ms.max(s.added_ms);
            album.year = album.year.max(s.year);
            album.folders |= 1 << folder.min(63);
            if album.genre.is_empty() {
                album.genre = s.genre.clone();
            }
            if album.cover.is_none() {
                album.cover = cover.clone();
            }
            let artist = &mut cat.artists[artist_at as usize];
            artist.folders |= 1 << folder.min(63);
            if artist.cover.is_none() {
                artist.cover = cover.clone();
            }
            if !s.genre.is_empty() {
                let g = genre_index.entry(s.genre.clone()).or_default();
                g.0 += 1;
                g.1.insert(album_at);
            }

            let fold_title = fold(&s.title);
            let fold_artist = fold(&s.artist);
            let search = if s.artist == s.album_artist {
                format!("{fold_title} {fold_artist} {}", fold(&s.album))
            } else {
                format!(
                    "{fold_title} {fold_artist} {} {}",
                    fold(&s.album),
                    fold(&s.album_artist)
                )
            };
            let id: Arc<str> = Arc::from(s.id);
            cat.song_at.insert(id.clone(), at);
            cat.songs.push(Song {
                id,
                path: PathBuf::from(s.path),
                title: s.title,
                artist: s.artist,
                album: s.album,
                album_artist: s.album_artist,
                genre: s.genre,
                cover,
                isrc: s.isrc.filter(|i| !i.is_empty()).map(String::into_boxed_str),
                duration_ms: s.duration_ms,
                track: s.track,
                disc: s.disc,
                year: s.year,
                bitrate: s.bitrate,
                sample_rate: s.sample_rate,
                size: s.size,
                added_ms: s.added_ms,
                album_at,
                artist_at,
                folder,
                search: search.into(),
                fold_title: fold_title.into(),
                fold_artist: fold_artist.into(),
            });
        }

        if elsewhere {
            folders.push(String::new());
        }
        cat.folders = folders;
        for (i, a) in cat.artists.iter().enumerate() {
            cat.artist_at.insert(a.id.clone(), i as u32);
        }
        for (i, a) in cat.albums.iter().enumerate() {
            cat.album_at.insert(a.id.clone(), i as u32);
        }
        cat.genres = genre_index
            .into_iter()
            .map(|(name, (songs, albums))| Genre {
                name,
                songs,
                albums: albums.len() as u32,
            })
            .collect();
        cat.genres.sort_by_key(|g| g.name.to_lowercase());
        cat
    }
}

/// One song as the app describes it, before grouping.
pub struct Raw {
    pub id: String,
    pub path: String,
    pub title: Arc<str>,
    pub artist: Arc<str>,
    pub album: Arc<str>,
    pub album_artist: Arc<str>,
    pub genre: Arc<str>,
    pub cover: Option<String>,
    pub isrc: Option<String>,
    pub duration_ms: u64,
    pub track: u32,
    pub disc: u32,
    pub year: u32,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub size: u64,
    pub added_ms: i64,
}

/// One `Arc` per distinct string: a library repeats its artists and albums.
#[derive(Default)]
pub struct Pool(HashSet<Arc<str>>);

impl Pool {
    /// The one shared `Arc` for `s` (trimmed).
    pub fn get(&mut self, s: &str) -> Arc<str> {
        let s = s.trim();
        if let Some(found) = self.0.get(s) {
            return found.clone();
        }
        let new: Arc<str> = Arc::from(s);
        self.0.insert(new.clone());
        new
    }
}

fn hashed_id(prefix: &str, parts: &[&str]) -> Arc<str> {
    let mut hash = Sha1::new();
    for part in parts {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let mut out = String::with_capacity(prefix.len() + 16);
    out.push_str(prefix);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    Arc::from(out)
}

/// `path` for comparing against a music folder: a folder only contains what's
/// under a separator, and Windows paths ignore case.
fn path_key(path: &str) -> String {
    let mut key = if cfg!(windows) {
        path.replace('\\', "/").to_lowercase()
    } else {
        path.to_string()
    };
    if !key.ends_with('/') && std::path::Path::new(path).extension().is_none() {
        key.push('/');
    }
    key
}

/// What a name sorts by: folded, without a leading article.
pub fn sort_key(name: &str) -> Box<str> {
    let folded = fold(name);
    for article in IGNORED_ARTICLES.split(' ') {
        let lower = article.to_lowercase();
        if let Some(rest) = folded.strip_prefix(&lower) {
            if let Some(rest) = rest.strip_prefix(' ') {
                if !rest.is_empty() {
                    return rest.into();
                }
            }
        }
    }
    folded.into()
}

/// The letter an artist is filed under.
pub fn index_letter(sort: &str) -> String {
    match sort.chars().find(|c| c.is_alphanumeric()) {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
        _ => "#".to_string(),
    }
}

/// Lower case, without the accents a person wouldn't type in a search.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
            continue;
        }
        for lower in c.to_lowercase() {
            match strip_accent(lower) {
                Some(plain) => out.push_str(plain),
                None => out.push(lower),
            }
        }
    }
    out
}

fn strip_accent(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'þ' => "th",
        _ => return None,
    })
}

/// The path a client is shown: made of tags, so it never says where the music
/// really is on this computer.
pub fn virtual_path(song: &Song) -> String {
    fn clean(s: &str) -> String {
        s.replace(['/', '\\'], "_")
    }
    let ext = suffix_of(&song.path);
    let number = if song.track > 0 {
        format!("{:02} - ", song.track)
    } else {
        String::new()
    };
    format!(
        "{}/{}/{number}{}.{ext}",
        clean(&song.album_artist),
        clean(&song.album),
        clean(&song.title)
    )
}

/// The lower-case file extension of `path`, without the dot.
pub fn suffix_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}
