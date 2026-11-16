//! The REST API's request model and endpoint dispatch: everything under `/rest/`, by GET
//! or a form POST, answered in XML, JSON or JSONP (see `respond`).
//!
//! Subsonic API 1.16.1 with every OpenSubsonic endpoint and extension. Video
//! exists as an empty library (Elbert has none), so its calls answer "none" or
//! "not found", as they would on any server without videos.

use std::sync::Arc;

use serde_json::{json, Value};

use super::auth::Who;
use super::backend::Capabilities;
use super::browse::View;
use super::respond::{ApiError, ERR_GENERIC};
use super::util::percent_decode;
use super::{admin, edits, Server};

/// What a call answers: the response payload, or the API error to send.
pub type Answer = Result<Value, ApiError>;

/// The query and, for a form POST, the body.
pub struct Params(pub Vec<(String, String)>);

impl Params {
    /// Splits a query string or a form body into decoded key and value pairs.
    pub fn parse(text: &str) -> Vec<(String, String)> {
        text.split('&')
            .filter(|kv| !kv.is_empty())
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (decode(k), decode(v))
            })
            .collect()
    }

    /// The first value of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Every value of `key`, for the parameters a client repeats (`id=1&id=2`).
    pub fn all(&self, key: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// The first value of `key`, or the "required parameter is missing" error.
    pub fn need(&self, key: &str) -> Result<&str, ApiError> {
        self.get(key).ok_or_else(|| ApiError::missing(key))
    }

    /// The first value of `key` as an integer.
    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.trim().parse().ok())
    }

    /// `key` as a page size: `default` when absent, at most `max`.
    pub fn count(&self, key: &str, default: usize, max: usize) -> usize {
        self.int(key)
            .map(|v| v.max(0) as usize)
            .unwrap_or(default)
            .min(max)
    }

    /// `key` as a page offset, 0 when absent.
    pub fn offset(&self, key: &str) -> usize {
        self.int(key).map(|v| v.max(0) as usize).unwrap_or(0)
    }

    /// `key` as a boolean (`true` or `false`)
    pub fn truthy(&self, key: &str) -> Option<bool> {
        self.get(key)
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
    }
}

/// A form value: `%XX` and `+`, unlike a path segment.
fn decode(text: &str) -> String {
    percent_decode(&text.replace('+', " "))
}

/// The OpenSubsonic extensions this host offers, by the call groups it has.
pub fn extensions(caps: &Capabilities) -> Value {
    let list: Vec<Value> = [
        ("apiKeyAuthentication", true),
        ("formPost", true),
        ("getPodcastEpisode", caps.podcasts),
        ("indexBasedQueue", true),
        ("playbackReport", true),
        ("songLyrics", true),
        ("sonicSimilarity", caps.sonic),
        ("topSongsByArtistId", true),
        ("transcodeOffset", caps.transcoding),
        ("transcoding", caps.transcoding),
    ]
    .iter()
    .filter(|(_, offered)| *offered)
    .map(|(name, _)| json!({ "name": name, "versions": [1] }))
    .collect();
    json!({ "openSubsonicExtensions": list })
}

/// `host` is what the client reached the server as, for links the answer carries.
pub async fn dispatch(srv: &Arc<Server>, call: &str, p: &Params, who: &Who, host: &str) -> Answer {
    if !srv.backend.capabilities().allows(call) {
        return Err(ApiError(ERR_GENERIC, format!("No such call: {call}")));
    }
    // The system calls read neither the library nor the notes.
    let (cat, notes) = match call {
        "ping" | "getLicense" | "tokenInfo" => Default::default(),
        _ => (srv.catalog(who).await?, srv.notes(who).await?),
    };
    let view = View {
        srv,
        cat: &cat,
        who,
        notes: &notes,
    };
    match call {
        // System
        "ping" => Ok(json!({})),
        "getLicense" => Ok(json!({ "license": { "valid": true } })),
        "tokenInfo" => Ok(json!({ "tokenInfo": { "username": who.name } })),
        // Browsing
        "getMusicFolders" => view.music_folders(),
        "getIndexes" => view.indexes(p),
        "getArtists" => view.artists(p),
        "getMusicDirectory" => view.directory(p),
        "getGenres" => view.genres(),
        "getArtist" => view.artist(p),
        "getAlbum" => view.album(p),
        "getSong" => view.song(p),
        "getArtistInfo" => view.artist_info(p, "artistInfo").await,
        "getArtistInfo2" => view.artist_info(p, "artistInfo2").await,
        "getAlbumInfo" | "getAlbumInfo2" => view.album_info(p).await,
        "getSimilarSongs" => view.similar(p, "similarSongs"),
        "getSimilarSongs2" => view.similar(p, "similarSongs2"),
        "getTopSongs" => view.top_songs(p),
        "getVideos" => Ok(json!({ "videos": { "video": [] } })),
        "getVideoInfo" => Err(ApiError::not_found("Video")),
        // Lists
        "getAlbumList" => view.album_list(p, "albumList"),
        "getAlbumList2" => view.album_list(p, "albumList2"),
        "getRandomSongs" => view.random_songs(p),
        "getSongsByGenre" => view.songs_by_genre(p),
        "getNowPlaying" => view.now_playing(),
        "getStarred" => view.starred(p, "starred"),
        "getStarred2" => view.starred(p, "starred2"),
        // Search
        "search" => view.search_old(p),
        "search2" => view.search(p, "searchResult2"),
        "search3" => view.search(p, "searchResult3"),
        // Playlists
        "getPlaylists" => view.playlists(p),
        "getPlaylist" => view.playlist(p),
        "createPlaylist" => edits::create_playlist(srv, p, who).await,
        "updatePlaylist" => edits::update_playlist(srv, p, who).await,
        "deletePlaylist" => edits::delete_playlist(srv, p, who).await,
        // Lyrics
        "getLyrics" => edits::get_lyrics(srv, &cat, p, who).await,
        "getLyricsBySongId" => edits::get_lyrics_by_song(srv, &cat, p, who).await,
        // Annotation
        "star" => edits::star(srv, &cat, p, who, true).await,
        "unstar" => edits::star(srv, &cat, p, who, false).await,
        "setRating" => edits::set_rating(srv, &cat, p, who).await,
        "scrobble" => edits::scrobble(srv, &cat, p, who).await,
        "reportPlayback" => edits::report_playback(srv, &cat, p, who).await,
        // Bookmarks and the play queue
        "getBookmarks" => view.bookmarks(),
        "createBookmark" => edits::create_bookmark(srv, &cat, p, who).await,
        "deleteBookmark" => edits::delete_bookmark(srv, p, who).await,
        "getPlayQueue" => view.play_queue(),
        "getPlayQueueByIndex" => view.play_queue_by_index(),
        "savePlayQueue" => edits::save_play_queue(srv, p, who).await,
        "savePlayQueueByIndex" => edits::save_play_queue_by_index(srv, p, who).await,
        // Sonic similarity
        "getSonicSimilarTracks" | "findSonicPath" => srv.link.call(srv, &view, call, p).await,
        // Accounts
        "getUser" => admin::get_user(srv, p, who).await,
        "getUsers" => admin::get_users(srv, who).await,
        "createUser" => admin::create_user(srv, p, who),
        "updateUser" => admin::update_user(srv, p, who),
        "deleteUser" => admin::delete_user(srv, p, who),
        "changePassword" => admin::change_password(srv, p, who),
        // Scanning
        "getScanStatus" => view.scan_status(),
        "startScan" => {
            who.admin("Scanning")?;
            srv.tell_scan();
            *srv.scan.lock().unwrap() = (true, cat.songs.len() as u64);
            view.scan_status()
        }
        // Sharing
        "getShares" => admin::get_shares(srv, &view, host),
        "createShare" => admin::create_share(srv, &view, p, host),
        "updateShare" => admin::update_share(srv, p, who),
        "deleteShare" => admin::delete_share(srv, p, who),
        // Internet radio
        "getInternetRadioStations" => admin::radio_stations(srv),
        "createInternetRadioStation" => admin::create_radio(srv, p, who),
        "updateInternetRadioStation" => admin::update_radio(srv, p, who),
        "deleteInternetRadioStation" => admin::delete_radio(srv, p, who),
        // Chat
        "getChatMessages" => admin::chat_messages(srv, p),
        "addChatMessage" => admin::add_chat(srv, p, who),
        // Podcasts
        "getPodcasts"
        | "getNewestPodcasts"
        | "getPodcastEpisode"
        | "refreshPodcasts"
        | "createPodcastChannel" => srv.link.call(srv, &view, call, p).await,
        "deletePodcastChannel" | "deletePodcastEpisode" | "downloadPodcastEpisode" => {
            srv.link.call(srv, &view, call, p).await
        }
        // The jukebox is this device's own player.
        "jukeboxControl" => admin::jukebox(srv, &view, p, who).await,
        _ => Err(ApiError(ERR_GENERIC, format!("No such call: {call}"))),
    }
}
