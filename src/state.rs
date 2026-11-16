//! What only the server keeps, in `subsonic_host.json`: other accounts and API
//! keys, each account's stars, ratings, plays, bookmarks and queue (the host's
//! own song stars and ratings are the app's favourites, kept by Dart), and
//! shares, radio stations, chat and podcasts.
//!
//! Plays reported by clients stay here rather than in the listening history,
//! which counts what played in this app. Writes are coalesced.

// The fields are plain data named after what they hold; the types and methods are documented.
#![allow(missing_docs)]

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// Everything the server keeps beside the library, serialised as one JSON document.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub accounts: BTreeMap<String, Account>,
    pub api_keys: BTreeMap<String, ApiKey>,
    pub users: HashMap<String, UserData>,
    /// Playlists of accounts other than the host's (the host's are the app's).
    pub playlists: Vec<StoredPlaylist>,
    /// What the API can say about one of the app's playlists that the app
    /// doesn't keep.
    pub playlist_notes: HashMap<String, PlaylistNote>,
    pub shares: BTreeMap<String, Share>,
    pub radio: BTreeMap<String, Radio>,
    pub chat: Vec<Chat>,
    pub podcasts: Vec<Channel>,
    pub sonic: HashMap<String, SonicPrint>,
}

impl State {
    /// What `user` has noted, if anything.
    pub fn data(&self, user: &str) -> Option<&UserData> {
        self.users.get(user)
    }

    /// What `user` has noted, created empty when there is none.
    pub fn data_mut(&mut self, user: &str) -> &mut UserData {
        self.users.entry(user.to_string()).or_default()
    }
}

/// One account of the server: its password, rights and limits.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub password: String,
    pub email: String,
    pub admin: bool,
    pub settings: bool,
    pub stream: bool,
    pub jukebox: bool,
    pub download: bool,
    pub upload: bool,
    pub playlist: bool,
    pub cover_art: bool,
    pub comment: bool,
    pub podcast: bool,
    pub share: bool,
    pub video_conversion: bool,
    pub scrobbling: bool,
    /// Kbit/s; 0 is no limit.
    pub max_bit_rate: u32,
    /// 1-based music folder ids this account may see; empty is all of them.
    pub folders: Vec<u32>,
}

impl Default for Account {
    /// The API's defaults for `createUser`.
    fn default() -> Self {
        Account {
            password: String::new(),
            email: String::new(),
            admin: false,
            settings: true,
            stream: true,
            jukebox: false,
            download: false,
            upload: false,
            playlist: false,
            cover_art: false,
            comment: false,
            podcast: false,
            share: false,
            video_conversion: false,
            scrobbling: true,
            max_bit_rate: 0,
            folders: Vec::new(),
        }
    }
}

impl Account {
    /// The host's own account: everything this server can do.
    pub fn owner(password: String) -> Account {
        Account {
            password,
            admin: true,
            settings: true,
            stream: true,
            jukebox: true,
            download: true,
            playlist: true,
            cover_art: true,
            comment: true,
            podcast: true,
            share: true,
            ..Default::default()
        }
    }
}

/// An API key's owner and label. The map key is the key's SHA-256.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiKey {
    pub user: String,
    pub label: String,
    pub created_ms: i64,
}

/// One account's stars, ratings, plays, bookmarks and saved queue.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserData {
    pub song_stars: HashMap<String, i64>,
    pub album_stars: HashMap<String, i64>,
    pub artist_stars: HashMap<String, i64>,
    pub song_ratings: HashMap<String, u8>,
    pub album_ratings: HashMap<String, u8>,
    pub artist_ratings: HashMap<String, u8>,
    /// Song id → (count, last played).
    pub plays: HashMap<String, (u64, i64)>,
    pub bookmarks: HashMap<String, Bookmark>,
    pub queue: Option<Queue>,
}

/// A bookmark on a song.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    pub position_ms: u64,
    pub comment: String,
    pub created_ms: i64,
    pub changed_ms: i64,
}

/// A saved play queue.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub ids: Vec<String>,
    pub current: Option<usize>,
    pub position_ms: u64,
    pub changed_ms: i64,
    pub changed_by: String,
}

/// A playlist of an account other than the host's.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredPlaylist {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub public: bool,
    pub comment: String,
    pub created_ms: i64,
    pub changed_ms: i64,
    pub songs: Vec<String>,
}

/// What the API says about one of the app's playlists that the app doesn't keep.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistNote {
    pub public: bool,
    pub comment: String,
}

/// A public share of songs.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Share {
    pub user: String,
    pub description: String,
    pub created_ms: i64,
    pub expires_ms: Option<i64>,
    pub last_visited_ms: Option<i64>,
    pub visits: u64,
    pub songs: Vec<String>,
}

/// An internet radio station.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Radio {
    pub name: String,
    pub stream_url: String,
    pub home_page: Option<String>,
}

/// One chat message.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Chat {
    pub user: String,
    pub time_ms: i64,
    pub message: String,
}

/// A podcast channel with its episodes.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub id: String,
    pub url: String,
    pub title: String,
    pub description: String,
    pub image_url: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub refreshed_ms: i64,
    pub episodes: Vec<Episode>,
}

/// One podcast episode.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Episode {
    pub id: String,
    pub guid: String,
    pub title: String,
    pub description: String,
    pub published_ms: Option<i64>,
    pub url: String,
    pub mime: String,
    pub size: u64,
    pub duration_s: u64,
    pub status: String,
    pub file: Option<String>,
    pub error: Option<String>,
}

/// What a song sounds like, for sonic similarity; the file's length tells a
/// stale one from a current one.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SonicPrint {
    pub len: u64,
    pub features: Vec<f32>,
}
