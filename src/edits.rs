//! What a client changes: playlists, stars, ratings, plays, bookmarks and the
//! saved queue. Core checks who may change what; the [`Backend`](super::backend::Backend)
//! keeps it (Elbert's sends the host's own to Dart, which owns them).

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{json, Value};

use super::api::{Answer, Params};
use super::auth::Who;
use super::backend::{Edit, EditOutcome, ItemKind, PlaylistInfo, UserNotes};
use super::browse::View;
use super::catalog::{fold, Catalog, Song};
use super::respond::{ApiError, ERR_GENERIC};
use super::state::Queue;
use super::{wall_ms, NowPlaying, Server};

/// A play counts once half the track or this long has gone by (the rule the
/// app's own statistics and scrobblers use).
const PLAY_AFTER_MS: u64 = 4 * 60 * 1000;

fn known_ids(cat: &Catalog, ids: Vec<&str>) -> Vec<String> {
    ids.into_iter()
        .filter(|id| cat.song(id).is_some())
        .map(str::to_string)
        .collect()
}

async fn apply(srv: &Server, who: &Who, edit: Edit) -> Result<EditOutcome, ApiError> {
    Ok(srv.backend.apply(who, edit).await?)
}

/// The playlist `id`, if `who` may change it.
fn target<'a>(notes: &'a UserNotes, id: &str, who: &Who) -> Result<&'a PlaylistInfo, ApiError> {
    let found = notes
        .playlists
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| ApiError::not_found("Playlist"))?;
    if found.app {
        // In the app's library; only the host may change it.
        if !who.owner || found.readonly {
            return Err(ApiError::not_allowed("Changing this playlist"));
        }
    } else if found.owner != who.name && !who.account.admin {
        return Err(ApiError::not_allowed("Changing someone else's playlist"));
    }
    Ok(found)
}

/// `createPlaylist`.
pub async fn create_playlist(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let cat = srv.catalog(who).await?;
    let songs = known_ids(&cat, p.all("songId"));
    let id = if let Some(existing) = p.get("playlistId") {
        let name = p.get("name").map(str::to_string);
        let notes = srv.notes(who).await?;
        target(&notes, existing, who)?;
        apply(
            srv,
            who,
            Edit::PlaylistUpdate {
                id: existing.to_string(),
                name,
                songs: Some(songs),
                comment: None,
                public: None,
            },
        )
        .await?;
        existing.to_string()
    } else {
        let name = p.need("name")?.trim().to_string();
        if name.is_empty() {
            return Err(ApiError::missing("name"));
        }
        match apply(srv, who, Edit::PlaylistCreate { name, songs }).await? {
            EditOutcome::Created(id) => id,
            EditOutcome::Done => {
                return Err(ApiError(
                    ERR_GENERIC,
                    "Could not create the playlist".into(),
                ))
            }
        }
    };
    let notes = srv.notes(who).await?;
    View {
        srv,
        cat: &cat,
        who,
        notes: &notes,
    }
    .playlist_by_id(&id)
}

/// `updatePlaylist`.
pub async fn update_playlist(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let cat = srv.catalog(who).await?;
    let id = p.need("playlistId")?;
    let notes = srv.notes(who).await?;
    let current = target(&notes, id, who)?.songs.clone();
    let remove: HashSet<usize> = p
        .all("songIndexToRemove")
        .iter()
        .filter_map(|i| i.parse().ok())
        .collect();
    let add = known_ids(&cat, p.all("songIdToAdd"));
    let changed_songs = !remove.is_empty() || !add.is_empty();
    // Indexes count what the client was shown, which leaves out songs gone
    // from the library.
    let mut songs: Vec<String> = current
        .into_iter()
        .filter(|s| cat.song(s).is_some())
        .enumerate()
        .filter(|(i, _)| !remove.contains(i))
        .map(|(_, s)| s)
        .collect();
    songs.extend(add);
    let edit = Edit::PlaylistUpdate {
        id: id.to_string(),
        name: p.get("name").map(str::to_string),
        songs: changed_songs.then_some(songs),
        comment: p.get("comment").map(str::to_string),
        public: p.truthy("public"),
    };
    apply(srv, who, edit).await?;
    Ok(json!({}))
}

/// `deletePlaylist`.
pub async fn delete_playlist(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let id = p.need("id")?;
    let notes = srv.notes(who).await?;
    target(&notes, id, who)?;
    apply(srv, who, Edit::PlaylistDelete { id: id.to_string() }).await?;
    Ok(json!({}))
}

/// `star` and `unstar` (`on` says which).
pub async fn star(srv: &Arc<Server>, cat: &Catalog, p: &Params, who: &Who, on: bool) -> Answer {
    for id in p.all("id") {
        let kind = if cat.song(id).is_some() {
            ItemKind::Song
        } else if cat.album(id).is_some() {
            ItemKind::Album
        } else if cat.artist(id).is_some() {
            ItemKind::Artist
        } else {
            return Err(ApiError::not_found("Item"));
        };
        apply(
            srv,
            who,
            Edit::Star {
                kind,
                id: id.to_string(),
                on,
            },
        )
        .await?;
    }
    for id in p.all("albumId") {
        apply(
            srv,
            who,
            Edit::Star {
                kind: ItemKind::Album,
                id: id.to_string(),
                on,
            },
        )
        .await?;
    }
    for id in p.all("artistId") {
        apply(
            srv,
            who,
            Edit::Star {
                kind: ItemKind::Artist,
                id: id.to_string(),
                on,
            },
        )
        .await?;
    }
    Ok(json!({}))
}

/// `setRating`.
pub async fn set_rating(srv: &Arc<Server>, cat: &Catalog, p: &Params, who: &Who) -> Answer {
    who.require(who.account.comment, "Rating")?;
    let id = p.need("id")?;
    let rating = p.int("rating").ok_or_else(|| ApiError::missing("rating"))?;
    if !(0..=5).contains(&rating) {
        return Err(ApiError(ERR_GENERIC, "A rating is from 0 to 5".into()));
    }
    let kind = if cat.song(id).is_some() {
        ItemKind::Song
    } else if cat.album(id).is_some() {
        ItemKind::Album
    } else if cat.artist(id).is_some() {
        ItemKind::Artist
    } else {
        return Err(ApiError::not_found("Item"));
    };
    apply(
        srv,
        who,
        Edit::Rating {
            kind,
            id: id.to_string(),
            rating: rating as u8,
        },
    )
    .await?;
    Ok(json!({}))
}

async fn count_play(srv: &Arc<Server>, who: &Who, id: &str, when: i64) -> Result<(), ApiError> {
    apply(
        srv,
        who,
        Edit::Play {
            id: id.to_string(),
            when_ms: when,
        },
    )
    .await?;
    Ok(())
}

/// A finished play counts on the server; `submission=false` is "now playing".
pub async fn scrobble(srv: &Arc<Server>, cat: &Catalog, p: &Params, who: &Who) -> Answer {
    let ids = p.all("id");
    if ids.is_empty() {
        return Err(ApiError::missing("id"));
    }
    let times = p.all("time");
    let submission = p.truthy("submission").unwrap_or(true);
    let client = p.get("c").unwrap_or("").to_string();
    let now = wall_ms();
    if submission {
        for (i, id) in ids.iter().enumerate() {
            if cat.song(id).is_none() {
                return Err(ApiError::not_found("Song"));
            }
            if who.account.scrobbling {
                count_play(
                    srv,
                    who,
                    id,
                    times.get(i).and_then(|t| t.parse().ok()).unwrap_or(now),
                )
                .await?;
            }
        }
        srv.now_playing
            .lock()
            .unwrap()
            .retain(|e| e.user != who.name || e.client != client);
    } else if let Some(id) = ids.iter().find(|id| cat.song(id).is_some()) {
        let mut playing = srv.now_playing.lock().unwrap();
        playing.retain(|e| e.user != who.name || e.client != client);
        playing.push(NowPlaying {
            song: Arc::from(*id),
            user: who.name.clone(),
            client,
            since_ms: now,
            updated_ms: now,
            state: None,
            position_ms: 0,
            rate: 1.0,
            counted: true,
        });
    }
    Ok(json!({}))
}

/// `playbackReport`: the client says where it is, and the play counts once it
/// is far enough in.
pub async fn report_playback(srv: &Arc<Server>, cat: &Catalog, p: &Params, who: &Who) -> Answer {
    let id = p.need("mediaId")?;
    let kind = p.need("mediaType")?;
    let position = p
        .int("positionMs")
        .ok_or_else(|| ApiError::missing("positionMs"))?
        .max(0) as u64;
    let state = p.need("state")?;
    if !["starting", "playing", "paused", "stopped"].contains(&state) {
        return Err(ApiError(
            ERR_GENERIC,
            format!("Unknown playback state: {state}"),
        ));
    }
    let song = match kind {
        "song" => Some(cat.song(id).ok_or_else(|| ApiError::not_found("Song"))?),
        "podcast" => None,
        other => {
            return Err(ApiError(
                ERR_GENERIC,
                format!("Unknown media type: {other}"),
            ))
        }
    };
    let rate = p
        .get("playbackRate")
        .and_then(|r| r.parse().ok())
        .unwrap_or(1.0);
    let client = p.get("c").unwrap_or("").to_string();
    let ignore = p.truthy("ignoreScrobble").unwrap_or(false) || !who.account.scrobbling;
    let now = wall_ms();
    let mut count = false;
    {
        let mut playing = srv.now_playing.lock().unwrap();
        let at = playing
            .iter()
            .position(|e| e.user == who.name && e.client == client);
        let same = at.is_some_and(|i| &*playing[i].song == id) && state != "starting";
        let mut counted = ignore || song.is_none();
        let mut since = now;
        if same {
            let was = &playing[at.unwrap()];
            counted = counted || was.counted;
            since = was.since_ms;
        }
        if let Some(i) = at {
            playing.remove(i);
        }
        if let Some(song) = song {
            let needed = (song.duration_ms / 2).clamp(1, PLAY_AFTER_MS);
            if !counted && position >= needed {
                count = true;
                counted = true;
            }
        }
        if state != "stopped" {
            playing.push(NowPlaying {
                song: Arc::from(id),
                user: who.name.clone(),
                client,
                since_ms: since,
                updated_ms: now,
                state: Some(state.to_string()),
                position_ms: position,
                rate,
                counted,
            });
        }
    }
    if count {
        count_play(srv, who, id, now).await?;
    }
    Ok(json!({}))
}

/// `createBookmark`.
pub async fn create_bookmark(srv: &Arc<Server>, cat: &Catalog, p: &Params, who: &Who) -> Answer {
    let id = p.need("id")?;
    if cat.song(id).is_none() {
        return Err(ApiError::not_found("Song"));
    }
    let position_ms = p
        .int("position")
        .ok_or_else(|| ApiError::missing("position"))?
        .max(0) as u64;
    let comment = p.get("comment").unwrap_or("").to_string();
    apply(
        srv,
        who,
        Edit::Bookmark {
            id: id.to_string(),
            position_ms,
            comment,
        },
    )
    .await?;
    Ok(json!({}))
}

/// `deleteBookmark`.
pub async fn delete_bookmark(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let id = p.need("id")?;
    apply(srv, who, Edit::DeleteBookmark { id: id.to_string() }).await?;
    Ok(json!({}))
}

async fn store_queue(
    srv: &Arc<Server>,
    who: &Who,
    p: &Params,
    ids: Vec<String>,
    current: Option<usize>,
) -> Result<(), ApiError> {
    let queue = (!ids.is_empty()).then(|| Queue {
        ids,
        current,
        position_ms: p.int("position").unwrap_or(0).max(0) as u64,
        changed_ms: wall_ms(),
        changed_by: p.get("c").unwrap_or("").to_string(),
    });
    apply(srv, who, Edit::SaveQueue(queue)).await?;
    Ok(())
}

/// `savePlayQueue`.
pub async fn save_play_queue(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let ids: Vec<String> = p.all("id").into_iter().map(str::to_string).collect();
    let current = p
        .get("current")
        .and_then(|c| ids.iter().position(|id| id == c));
    if !ids.is_empty() && p.get("current").is_some() && current.is_none() {
        return Err(ApiError(
            ERR_GENERIC,
            "The current song isn't in the queue".into(),
        ));
    }
    store_queue(srv, who, p, ids, current).await?;
    Ok(json!({}))
}

/// `savePlayQueueByIndex`.
pub async fn save_play_queue_by_index(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let ids: Vec<String> = p.all("id").into_iter().map(str::to_string).collect();
    let current = match (ids.is_empty(), p.int("currentIndex")) {
        (true, _) => None,
        (false, None) => return Err(ApiError::missing("currentIndex")),
        (false, Some(i)) if i < 0 || i as usize >= ids.len() => {
            return Err(ApiError(
                ERR_GENERIC,
                "currentIndex is outside the queue".into(),
            ))
        }
        (false, Some(i)) => Some(i as usize),
    };
    store_queue(srv, who, p, ids, current).await?;
    Ok(json!({}))
}

// ---- Lyrics ------------------------------------------------------------------

/// The lyrics for `song` as the app has them: LRC or plain text.
async fn lyrics_of(srv: &Arc<Server>, who: &Who, song: &Song) -> Option<Arc<str>> {
    let key = song.id.to_string();
    if let Some(known) = srv.lyrics.lock().unwrap().get(&key).cloned() {
        return known;
    }
    let found = srv.backend.lyrics(who, &song.id).await.ok().flatten();
    let text: Option<Arc<str>> = found
        .filter(|l| !l.text.is_empty())
        .map(|l| Arc::from(l.text.as_str()));
    srv.lyrics.lock().unwrap().insert(key, text.clone());
    text
}

/// `[mm:ss.xx]text` lines, as (milliseconds, text); empty when the lyrics
/// aren't timed. Word timings (`<mm:ss.xx>`) are dropped: the API has no field
/// for them.
pub(super) fn parse_lrc(text: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut times = Vec::new();
        while let Some(tail) = rest.strip_prefix('[') {
            let Some(end) = tail.find(']') else { break };
            let Some(ms) = lrc_time(&tail[..end]) else {
                break;
            };
            times.push(ms);
            rest = tail[end + 1..].trim_start();
        }
        if times.is_empty() {
            continue;
        }
        let words = strip_word_times(rest);
        for ms in times {
            out.push((ms, words.clone()));
        }
    }
    out.sort_by_key(|(ms, _)| *ms);
    out
}

fn lrc_time(tag: &str) -> Option<u64> {
    let (min, sec) = tag.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let sec: f64 = sec.trim().replace(':', ".").parse().ok()?;
    Some(min * 60_000 + (sec * 1000.0).round() as u64)
}

fn strip_word_times(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        if lrc_time(&rest[start + 1..start + end]).is_none() {
            out.push_str(&rest[..start + end + 1]);
        } else {
            out.push_str(&rest[..start]);
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn plain_lyrics(text: &str) -> String {
    let timed = parse_lrc(text);
    if timed.is_empty() {
        return text.to_string();
    }
    timed
        .into_iter()
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `getLyrics`.
pub async fn get_lyrics(srv: &Arc<Server>, cat: &Catalog, p: &Params, who: &Who) -> Answer {
    let artist = p.get("artist").map(fold).filter(|a| !a.is_empty());
    let title = p.get("title").map(fold).filter(|t| !t.is_empty());
    let found = cat.songs.iter().find(|s| {
        who.mask & (1 << s.folder.min(63)) != 0
            && title.as_deref().is_some_and(|t| *s.fold_title == *t)
            && artist.as_deref().is_none_or(|a| *s.fold_artist == *a)
    });
    let Some(song) = found else {
        return Ok(json!({ "lyrics": { "value": "" } }));
    };
    let text = lyrics_of(srv, who, song).await;
    Ok(json!({ "lyrics": {
        "artist": &*song.artist,
        "title": &*song.title,
        "value": text.as_deref().map(plain_lyrics).unwrap_or_default(),
    }}))
}

/// `getLyricsBySongId`.
pub async fn get_lyrics_by_song(srv: &Arc<Server>, cat: &Catalog, p: &Params, who: &Who) -> Answer {
    let song = cat
        .song(p.need("id")?)
        .filter(|s| who.mask & (1 << s.folder.min(63)) != 0)
        .ok_or_else(|| ApiError::not_found("Song"))?;
    let Some(text) = lyrics_of(srv, who, song).await else {
        return Ok(json!({ "lyricsList": { "structuredLyrics": [] } }));
    };
    let timed = parse_lrc(&text);
    let synced = !timed.is_empty();
    let lines: Vec<Value> = if synced {
        timed
            .into_iter()
            .map(|(start, value)| json!({ "start": start, "value": value }))
            .collect()
    } else {
        text.lines()
            .map(|value| json!({ "value": value }))
            .collect()
    };
    Ok(json!({ "lyricsList": { "structuredLyrics": [{
        "displayArtist": &*song.artist,
        "displayTitle": &*song.title,
        "lang": "und",
        "synced": synced,
        "line": lines,
    }]}}))
}
