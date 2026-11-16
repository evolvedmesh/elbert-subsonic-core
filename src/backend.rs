//! What the protocol needs from whoever hosts it: who may log in, the library each
//! account sees, what each account has noted about it, and where their edits go.
//!
//! Elbert's backend keeps the library in memory (Dart pushes it) and sends the owner's
//! edits back to Dart; the managed host's answers each of these from the service. Core
//! holds no storage of its own: persistence and file layout are the backend's business.
//! Audio, covers, lyrics and artist info come from here as well: a backend answers with
//! a file, a redirect or bytes, and the host decides how to send it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use zeroize::Zeroizing;

use super::auth::Who;
use super::catalog::Catalog;
use super::respond::{ApiError, ERR_GENERIC};
use super::state::{Bookmark, Queue};
use super::Boxed;

/// Whose library it is: the account id on the managed host, a constant on Elbert.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TenantId(pub Arc<str>);

/// Which call groups exist on a host. They drive dispatch (a call of an absent group is
/// "no such call") and the OpenSubsonic extension list.
#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    /// `getUser(s)`, `createUser`, `updateUser`, `deleteUser`, `changePassword`.
    pub accounts: bool,
    /// `getShares`, `createShare`, `updateShare`, `deleteShare`.
    pub shares: bool,
    /// The internet radio calls.
    pub radio: bool,
    /// `getChatMessages`, `addChatMessage`.
    pub chat: bool,
    /// Podcast calls, and the `getPodcastEpisode` extension.
    pub podcasts: bool,
    /// `getSonicSimilarTracks`, `findSonicPath`, and the `sonicSimilarity` extension.
    pub sonic: bool,
    /// `jukeboxControl`.
    pub jukebox: bool,
    /// `startScan`, `getScanStatus`.
    pub scan: bool,
    /// The `transcoding` and `transcodeOffset` extensions.
    pub transcoding: bool,
    /// The managed, multi-tenant host ([`Capabilities::managed`]): core answers the account,
    /// scan, share, chat, podcast, sonic, jukebox, radio, transcoding and `hls` calls as
    /// ADR-0017 section 8 says, whatever the flags above say. The flags still decide which
    /// OpenSubsonic extensions are advertised.
    pub managed: bool,
}

impl Capabilities {
    /// Everything: what the on-device host offers.
    pub const fn all() -> Self {
        Capabilities {
            accounts: true,
            shares: true,
            radio: true,
            chat: true,
            podcasts: true,
            sonic: true,
            jukebox: true,
            scan: true,
            transcoding: true,
            managed: false,
        }
    }

    /// What the managed host offers: its calls are answered by core as described in
    /// ADR-0017 section 8 (accounts, scan status, shares, chat, podcasts, sonic, jukebox,
    /// radio per account, no transcoding). The advertised extensions are exactly
    /// `apiKeyAuthentication`, `formPost`, `indexBasedQueue`, `playbackReport`,
    /// `songLyrics` and `topSongsByArtistId`.
    pub const fn managed() -> Self {
        Capabilities {
            accounts: true,
            shares: true,
            radio: true,
            chat: true,
            podcasts: false,
            sonic: false,
            jukebox: false,
            scan: true,
            transcoding: false,
            managed: true,
        }
    }

    /// Whether `call` belongs to a group this host has. Calls outside every optional
    /// group are always allowed.
    pub fn allows(&self, call: &str) -> bool {
        match call {
            "getUser" | "getUsers" | "createUser" | "updateUser" | "deleteUser"
            | "changePassword" => self.accounts,
            "getShares" | "createShare" | "updateShare" | "deleteShare" => self.shares,
            "getInternetRadioStations"
            | "createInternetRadioStation"
            | "updateInternetRadioStation"
            | "deleteInternetRadioStation" => self.radio,
            "getChatMessages" | "addChatMessage" => self.chat,
            "getPodcasts"
            | "getNewestPodcasts"
            | "getPodcastEpisode"
            | "refreshPodcasts"
            | "createPodcastChannel" => self.podcasts,
            "deletePodcastChannel" | "deletePodcastEpisode" | "downloadPodcastEpisode" => {
                self.podcasts
            }
            "getSonicSimilarTracks" | "findSonicPath" => self.sonic,
            "jukeboxControl" => self.jukebox,
            "getTranscodeDecision" | "getTranscodeStream" => self.transcoding,
            "startScan" | "getScanStatus" => self.scan,
            _ => true,
        }
    }
}

/// Where a request came from, for the backend's lockout counters.
#[derive(Clone, Copy, Debug, Default)]
pub struct Peer {
    /// The client's address, when the transport knows it.
    pub ip: Option<IpAddr>,
}

/// What the backend says about an account after a failed attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lockout {
    /// Attempts are still allowed.
    Open,
    /// Too many failures: core answers without saying whether the credentials were right.
    Locked,
}

/// Why a [`Backend`] could not do what core asked.
#[derive(Debug)]
pub enum BackendError {
    /// There is no such thing, or the account may not know about it.
    NotFound(String),
    /// The account may not do it.
    NotAllowed(String),
    /// The backend could not do it, or did not answer in time.
    Failed(String),
}

impl From<BackendError> for ApiError {
    fn from(e: BackendError) -> Self {
        match e {
            BackendError::NotFound(what) => ApiError::not_found(&what),
            BackendError::NotAllowed(what) => ApiError::not_allowed(&what),
            BackendError::Failed(message) => ApiError(ERR_GENERIC, message),
        }
    }
}

/// What a backend says a cover is. Only the backend that gave it out reads it: Elbert's
/// is a path, a URL or a platform media-library reference; a managed host's is its own key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CoverRef(pub String);

/// A request for the audio of one item.
#[derive(Clone, Debug)]
pub struct MediaRequest {
    /// A song id, or the id of anything else the host streams (a downloaded podcast episode).
    pub id: String,
    /// `download` rather than `stream`.
    pub download: bool,
}

/// What a backend can send a client for a [`MediaRequest`] or a cover.
#[derive(Clone, Debug)]
pub enum Media {
    /// A local file: the host serves it, with `Range`.
    File {
        /// Where the file is.
        path: PathBuf,
        /// Its MIME type.
        mime: String,
    },
    /// Bytes the backend has in memory (a cover).
    Bytes {
        /// The bytes.
        data: Arc<[u8]>,
        /// Their MIME type.
        mime: String,
    },
    /// Somewhere else: the host answers `302` and the client follows. Managed mode.
    Redirect {
        /// Where to go: a bearer secret, never logged.
        url: String,
        /// How long the URL stays valid, in seconds.
        ttl_secs: u32,
    },
    /// A file the host must decode and encode (the `transcode` feature of the server crate;
    /// without it the source is sent as it is).
    Transcode {
        /// The file to decode.
        source: PathBuf,
        /// What to encode it to.
        format: TranscodeFormat,
        /// Where in the song to start, in milliseconds.
        offset_ms: u64,
    },
    /// There is nothing to send (a song without a cover, say).
    NotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// The encodings a [`Media::Transcode`] can ask for.
pub enum TranscodeFormat {
    /// MP3.
    Mp3,
    /// Opus in an Ogg container.
    Opus,
    /// WAV (16-bit PCM).
    Wav,
}

/// How a host answers a [`Media::Redirect`], for any transport: the status and headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    /// The HTTP status.
    pub status: u16,
    /// The headers to send.
    pub headers: Vec<(&'static str, String)>,
}

impl Media {
    /// `302 Found` to the URL, never cached: the URL is a bearer secret that expires.
    /// `None` for what isn't a redirect.
    pub fn redirect_head(&self) -> Option<Head> {
        match self {
            Media::Redirect { url, .. } => Some(Head {
                status: 302,
                headers: vec![
                    ("Location", url.clone()),
                    ("Cache-Control", "no-store".into()),
                ],
            }),
            _ => None,
        }
    }
}

/// The lyrics of a song: LRC (`[mm:ss.xx]text` lines) or plain text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lyrics {
    /// The text itself.
    pub text: String,
}

/// What is known about an artist, or about an album: the fields of OpenSubsonic's
/// `artistInfo2` and `albumInfo` (biography, MusicBrainz id, Last.fm URL, image URLs),
/// as a JSON object.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtistInfo(pub serde_json::Value);

/// A playlist as the backend holds it, whoever's it is. Core works out what an account
/// may see and change.
#[derive(Clone, Debug, Default)]
pub struct PlaylistInfo {
    /// The playlist id.
    pub id: String,
    /// The playlist's name.
    pub name: String,
    /// The account that owns it.
    pub owner: String,
    /// A free-text comment.
    pub comment: String,
    /// Whether other accounts may see it.
    pub public: bool,
    /// One no client may edit: a plugin's mirror, or one holding tracks the server
    /// doesn't serve.
    pub readonly: bool,
    /// One of the host's own (the app's): only the host account may change it.
    pub app: bool,
    /// When it was created, in milliseconds since the Unix epoch.
    pub created_ms: i64,
    /// When it last changed, in milliseconds since the Unix epoch.
    pub changed_ms: i64,
    /// The song ids, in order.
    pub songs: Vec<String>,
}

/// One internet radio station of an account. Only managed hosts keep them per account (the
/// on-device host keeps its stations in [`State`](super::state::State)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadioStation {
    /// The station id.
    pub id: String,
    /// The station's name.
    pub name: String,
    /// The address of the stream: an `http` or `https` URL.
    pub stream_url: String,
    /// The station's home page, if it has one: an `http` or `https` URL.
    pub home_page: Option<String>,
}

/// What one account has noted about the library: stars, ratings, plays, bookmarks, the
/// saved queue, and every playlist there is (core applies who may see which).
#[derive(Clone, Debug, Default)]
pub struct UserNotes {
    /// Song id → when it was starred (ms since the epoch).
    pub song_stars: HashMap<String, i64>,
    /// Album id → when it was starred.
    pub album_stars: HashMap<String, i64>,
    /// Artist id → when it was starred.
    pub artist_stars: HashMap<String, i64>,
    /// Song id → rating (1 to 5).
    pub song_ratings: HashMap<String, u8>,
    /// Album id → rating (1 to 5).
    pub album_ratings: HashMap<String, u8>,
    /// Artist id → rating (1 to 5).
    pub artist_ratings: HashMap<String, u8>,
    /// Song id → (count, last played).
    pub plays: HashMap<String, (u64, i64)>,
    /// Song id → bookmark.
    pub bookmarks: HashMap<String, Bookmark>,
    /// The saved play queue, if any.
    pub queue: Option<Queue>,
    /// Every playlist, whoever's it is.
    pub playlists: Vec<PlaylistInfo>,
    /// The account's internet radio stations (managed hosts only).
    pub radio: Vec<RadioStation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// What a star or a rating is on.
pub enum ItemKind {
    /// A song.
    Song,
    /// An album.
    Album,
    /// An artist.
    Artist,
}

/// A change a client asked for, to the notes of the account that asked.
#[derive(Clone, Debug)]
pub enum Edit {
    /// Star or unstar an item.
    Star {
        /// What is starred.
        kind: ItemKind,
        /// Its id.
        id: String,
        /// Star it (`true`) or unstar it.
        on: bool,
    },
    /// 0 clears it.
    Rating {
        /// What is rated.
        kind: ItemKind,
        /// Its id.
        id: String,
        /// The rating, 1 to 5.
        rating: u8,
    },
    /// A finished play.
    Play {
        /// The song id.
        id: String,
        /// When it was played, in milliseconds since the Unix epoch.
        when_ms: i64,
    },
    /// Set a bookmark on a song.
    Bookmark {
        /// The song id.
        id: String,
        /// The position, in milliseconds.
        position_ms: u64,
        /// A note the client keeps with it.
        comment: String,
    },
    /// Delete the bookmark on a song.
    DeleteBookmark {
        /// The song id.
        id: String,
    },
    /// `None` clears the saved queue.
    SaveQueue(Option<Queue>),
    /// Create a playlist.
    PlaylistCreate {
        /// The new playlist's name.
        name: String,
        /// Its song ids.
        songs: Vec<String>,
    },
    /// What is `None` stays as it was.
    PlaylistUpdate {
        /// The playlist id.
        id: String,
        /// A new name.
        name: Option<String>,
        /// The new song ids.
        songs: Option<Vec<String>>,
        /// A new comment.
        comment: Option<String>,
        /// A new visibility.
        public: Option<bool>,
    },
    /// Delete a playlist.
    PlaylistDelete {
        /// The playlist id.
        id: String,
    },
    /// Add an internet radio station to the account (managed hosts only).
    RadioCreate {
        /// The station's name.
        name: String,
        /// The address of the stream.
        stream_url: String,
        /// The station's home page.
        home_page: Option<String>,
    },
    /// Replace a station's fields (managed hosts only).
    RadioUpdate {
        /// The station id.
        id: String,
        /// The station's name.
        name: String,
        /// The address of the stream.
        stream_url: String,
        /// The station's home page.
        home_page: Option<String>,
    },
    /// Delete a station (managed hosts only).
    RadioDelete {
        /// The station id.
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// What a [`Backend`] did with an [`Edit`].
pub enum EditOutcome {
    /// It was done.
    Done,
    /// The id of the playlist a `PlaylistCreate` made.
    Created(String),
}

/// Implemented by Elbert (an in-memory catalog fed by Dart) and by `subsonic-ffi` (asks to
/// the service). `Backend` is the `Send` variant; `LocalBackend` is the same trait for
/// backends that aren't `Send`.
#[trait_variant::make(Backend: Send)]
pub trait LocalBackend: Sync + 'static {
    /// Which call groups exist on this host.
    fn capabilities(&self) -> &Capabilities;

    /// Every active secret of `user` (app passwords). Core does the `md5(password + salt)`,
    /// plain and `enc:` comparisons in constant time. Empty when there is no such user,
    /// or while the account is locked out.
    async fn passwords(&self, user: &str) -> Result<Vec<Zeroizing<String>>, BackendError>;
    /// The identity for an API key, looked up by its SHA-256: the key itself never leaves core.
    async fn api_key(&self, key_sha256: [u8; 32]) -> Result<Option<Who>, BackendError>;
    /// The identity of `user`: roles, folder mask, tenant and catalog version.
    async fn who(&self, user: &str) -> Result<Option<Who>, BackendError>;
    /// Called after every failed attempt, so the host can count and lock out.
    async fn auth_failed(&self, user: Option<&str>, peer: &Peer) -> Result<Lockout, BackendError>;

    /// The tenant's catalog at `version` (`Who::catalog_version`). Core keeps it until the
    /// version changes.
    async fn catalog(&self, tenant: &TenantId, version: u64) -> Result<Arc<Catalog>, BackendError>;
    /// What `who` has noted. A snapshot: an edit shows in the next one.
    async fn notes(&self, who: &Who) -> Result<Arc<UserNotes>, BackendError>;
    /// Applies an edit; once it answers, the next `notes` shows it.
    async fn apply(&self, who: &Who, edit: Edit) -> Result<EditOutcome, BackendError>;

    /// The audio of `req`: a file (Elbert), or a redirect (managed). The backend checks the
    /// item is `who`'s to hear.
    async fn media(&self, who: &Who, req: MediaRequest) -> Result<Media, BackendError>;
    /// The picture `cover` names, at about `size` pixels when it can.
    async fn cover(
        &self,
        who: &Who,
        cover: &CoverRef,
        size: Option<u32>,
    ) -> Result<Media, BackendError>;
    /// The lyrics of `song`, if there are any.
    async fn lyrics(&self, who: &Who, song: &str) -> Result<Option<Lyrics>, BackendError>;
    /// What is known about `artist`, or about `artist`'s `album` when one is named.
    async fn artist_info(
        &self,
        who: &Who,
        artist: &str,
        album: Option<&str>,
    ) -> Result<Option<ArtistInfo>, BackendError>;
}

/// `Backend` made object-safe, so a `Server` can hold any of them. Implemented for every
/// `Backend`.
pub trait DynBackend: Send + Sync + 'static {
    /// Which call groups this backend has.
    fn capabilities(&self) -> &Capabilities;
    /// The app passwords of `user`, for the token (salted MD5) login. Empty when there are none.
    fn passwords<'a>(
        &'a self,
        user: &'a str,
    ) -> Boxed<'a, Result<Vec<Zeroizing<String>>, BackendError>>;
    /// The account an API key belongs to, found by the key's SHA-256.
    fn api_key<'a>(&'a self, key_sha256: [u8; 32]) -> Boxed<'a, Result<Option<Who>, BackendError>>;
    /// The account named `user`, with what it may see worked out.
    fn who<'a>(&'a self, user: &'a str) -> Boxed<'a, Result<Option<Who>, BackendError>>;
    /// Records a failed login attempt and says whether the account (or the peer) is now locked out.
    fn auth_failed<'a>(
        &'a self,
        user: Option<&'a str>,
        peer: &'a Peer,
    ) -> Boxed<'a, Result<Lockout, BackendError>>;
    /// The catalog of `tenant` at `version`.
    fn catalog<'a>(
        &'a self,
        tenant: &'a TenantId,
        version: u64,
    ) -> Boxed<'a, Result<Arc<Catalog>, BackendError>>;
    /// What `who` has noted about the library.
    fn notes<'a>(&'a self, who: &'a Who) -> Boxed<'a, Result<Arc<UserNotes>, BackendError>>;
    /// Applies `edit` to the notes of `who`.
    fn apply<'a>(
        &'a self,
        who: &'a Who,
        edit: Edit,
    ) -> Boxed<'a, Result<EditOutcome, BackendError>>;
    /// The audio of `req`, or where to get it.
    fn media<'a>(
        &'a self,
        who: &'a Who,
        req: MediaRequest,
    ) -> Boxed<'a, Result<Media, BackendError>>;
    /// The picture `cover` names, at `size` pixels if the backend can scale.
    fn cover<'a>(
        &'a self,
        who: &'a Who,
        cover: &'a CoverRef,
        size: Option<u32>,
    ) -> Boxed<'a, Result<Media, BackendError>>;
    /// The lyrics of `song`.
    fn lyrics<'a>(
        &'a self,
        who: &'a Who,
        song: &'a str,
    ) -> Boxed<'a, Result<Option<Lyrics>, BackendError>>;
    /// What is known about `artist`, or about the album `album` of it.
    fn artist_info<'a>(
        &'a self,
        who: &'a Who,
        artist: &'a str,
        album: Option<&'a str>,
    ) -> Boxed<'a, Result<Option<ArtistInfo>, BackendError>>;
}

impl<B: Backend> DynBackend for B {
    fn capabilities(&self) -> &Capabilities {
        Backend::capabilities(self)
    }

    fn passwords<'a>(
        &'a self,
        user: &'a str,
    ) -> Boxed<'a, Result<Vec<Zeroizing<String>>, BackendError>> {
        Box::pin(Backend::passwords(self, user))
    }

    fn api_key<'a>(&'a self, key_sha256: [u8; 32]) -> Boxed<'a, Result<Option<Who>, BackendError>> {
        Box::pin(Backend::api_key(self, key_sha256))
    }

    fn who<'a>(&'a self, user: &'a str) -> Boxed<'a, Result<Option<Who>, BackendError>> {
        Box::pin(Backend::who(self, user))
    }

    fn auth_failed<'a>(
        &'a self,
        user: Option<&'a str>,
        peer: &'a Peer,
    ) -> Boxed<'a, Result<Lockout, BackendError>> {
        Box::pin(Backend::auth_failed(self, user, peer))
    }

    fn catalog<'a>(
        &'a self,
        tenant: &'a TenantId,
        version: u64,
    ) -> Boxed<'a, Result<Arc<Catalog>, BackendError>> {
        Box::pin(Backend::catalog(self, tenant, version))
    }

    fn notes<'a>(&'a self, who: &'a Who) -> Boxed<'a, Result<Arc<UserNotes>, BackendError>> {
        Box::pin(Backend::notes(self, who))
    }

    fn apply<'a>(
        &'a self,
        who: &'a Who,
        edit: Edit,
    ) -> Boxed<'a, Result<EditOutcome, BackendError>> {
        Box::pin(Backend::apply(self, who, edit))
    }

    fn media<'a>(
        &'a self,
        who: &'a Who,
        req: MediaRequest,
    ) -> Boxed<'a, Result<Media, BackendError>> {
        Box::pin(Backend::media(self, who, req))
    }

    fn cover<'a>(
        &'a self,
        who: &'a Who,
        cover: &'a CoverRef,
        size: Option<u32>,
    ) -> Boxed<'a, Result<Media, BackendError>> {
        Box::pin(Backend::cover(self, who, cover, size))
    }

    fn lyrics<'a>(
        &'a self,
        who: &'a Who,
        song: &'a str,
    ) -> Boxed<'a, Result<Option<Lyrics>, BackendError>> {
        Box::pin(Backend::lyrics(self, who, song))
    }

    fn artist_info<'a>(
        &'a self,
        who: &'a Who,
        artist: &'a str,
        album: Option<&'a str>,
    ) -> Boxed<'a, Result<Option<ArtistInfo>, BackendError>> {
        Box::pin(Backend::artist_info(self, who, artist, album))
    }
}
