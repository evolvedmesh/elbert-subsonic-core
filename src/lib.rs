//! The core of the Subsonic and OpenSubsonic API as Elbert serves it: request parameters,
//! authentication, endpoint dispatch, the catalog and its read model, and the response model
//! with its XML, JSON and JSONP encoders.
//!
//! This crate has no sockets, no files, no FFI, no globals and no runtime of its own, and
//! forbids `unsafe`. Who may log in, the library, the notes, the edits, audio, covers, lyrics
//! and artist info come from a [`Backend`]; what is left (the jukebox, podcasts, sonic
//! similarity, persistence of the extras) goes through [`Link`], which the embedding app
//! implements. HTTP is the business of the `elbert-subsonic-server` crate.
//!
//! The entry points are [`Server`] (one hosted library), [`api::dispatch`] (one call) and
//! [`auth::authenticate`] (who is asking).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod admin;
pub mod api;
pub mod auth;
pub mod backend;
#[cfg(test)]
mod backend_tests;
pub mod browse;
pub mod catalog;
pub mod edits;
pub mod respond;
pub mod state;
pub mod util;

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use api::{Answer, Params};
use auth::Who;
use backend::{Backend, CoverRef, DynBackend, Media, MediaRequest, TenantId, UserNotes};
use browse::View;
use catalog::Catalog;
use respond::ApiError;
use state::State;
use util::Bounded;

/// What one client is playing now, as `getNowPlaying` lists it.
pub struct NowPlaying {
    /// The song id.
    pub song: Arc<str>,
    /// The account that is playing it.
    pub user: String,
    /// The client's name, as it said in `c=`.
    pub client: String,
    /// When the play started, in milliseconds since the Unix epoch.
    pub since_ms: i64,
    /// When the entry was last refreshed, in milliseconds since the Unix epoch.
    pub updated_ms: i64,
    /// What `reportPlayback` said; a plain `scrobble` says only "playing it".
    pub state: Option<String>,
    /// Where the client says it is in the song, in milliseconds.
    pub position_ms: u64,
    /// The playback rate the client reports (1.0 is normal speed).
    pub rate: f64,
    /// Whether this play has been counted already.
    pub counted: bool,
}

/// A boxed, `Send` future: what the object-safe seams ([`Link`], [`backend::DynBackend`]) return.
pub type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A question the host puts to the app and waits for the answer to.
pub enum Ask<'a> {
    /// The jukebox is the device's own player, which only an on-device host has: it stays on
    /// this seam rather than in the [`Backend`].
    Jukebox {
        /// The `action` parameter of `jukeboxControl`.
        action: &'a str,
        /// The `index` parameter.
        index: i64,
        /// The `offset` parameter, in seconds.
        offset: u64,
        /// The `gain` parameter.
        gain: f64,
        /// The `id` parameters: the songs to add or set.
        ids: &'a [String],
    },
}

/// Something the app should hear about, with no answer expected.
pub enum Tell<'a> {
    /// A client was seen; the app decides how often to hear about the same one.
    Client {
        /// The client's address.
        ip: IpAddr,
        /// The name it gave in `c=`.
        name: &'a str,
    },
    /// A scan was asked for (`startScan`).
    Scan,
    /// A client changed the host's password.
    Login {
        /// The account that changed password.
        user: &'a str,
        /// The new password.
        password: &'a str,
    },
}

/// What the app embedding the server provides beyond the [`Backend`]: the questions only
/// it can answer, the calls whose work lives outside core, and the persistence of the
/// state core keeps for the extra call groups. Elbert implements it over its FFI jobs.
pub trait Link: Send + Sync + 'static {
    /// Hears about `event`; no answer is expected.
    fn tell(&self, event: Tell<'_>);

    /// `None` when the app has no answer, or doesn't give one in time.
    fn ask<'a>(&'a self, ask: Ask<'a>, timeout: Duration) -> Boxed<'a, Option<Vec<u8>>>;

    /// The server's state changed: write it a moment from now.
    fn save_soon(&self);

    /// The name of the host's own account (the one `Tell::Login` changes the password of).
    fn host_user(&self) -> String;

    /// A folder to keep downloaded podcast episodes in, if the app has one.
    fn data_dir(&self) -> Option<PathBuf>;

    /// The calls whose work lives outside core (podcasts and sonic similarity).
    fn call<'a>(
        &'a self,
        srv: &'a Arc<Server>,
        view: &'a View<'a>,
        call: &'a str,
        p: &'a Params,
    ) -> Boxed<'a, Answer>;
}

/// How many tenants' catalogs core keeps at once.
const CATALOGS_KEPT: usize = 64;

/// What one hosted library needs beside its backend: the link back to the app, the
/// state of the extra call groups, and what only this side keeps (now playing, caches).
pub struct Server {
    /// The seam back to the embedding app.
    pub link: Arc<dyn Link>,
    /// Who may log in and what the library holds.
    pub backend: Arc<dyn DynBackend>,
    /// The server's own name.
    pub name: RwLock<String>,
    /// Accounts and keys (read by Elbert's backend), shares, radio, chat, podcasts and
    /// sonic prints: what the extra call groups keep. The backend persists it.
    pub state: Arc<Mutex<State>>,
    /// Whether a scan is running, and when the last one finished (ms since the epoch).
    pub scan: Mutex<(bool, u64)>,
    /// Recently fetched lyrics by song id.
    pub lyrics: Mutex<Bounded<String, Option<Arc<str>>>>,
    /// Recently fetched artist and album info by artist and album id.
    pub info: Mutex<Bounded<String, serde_json::Value>>,
    /// What clients are playing now.
    pub now_playing: Mutex<Vec<NowPlaying>>,
    /// The catalog of each tenant, with the version it was fetched at.
    catalogs: Mutex<HashMap<TenantId, (u64, Arc<Catalog>)>>,
}

/// Milliseconds since the Unix epoch, now.
pub fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 16 hex digits nobody can guess.
pub fn random_id() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Server {
    /// A server for one hosted library: the [Link] back to the app, the [Backend] that answers
    /// /// for the library, the host's `name` and the persisted [State] of the extra call groups.
    pub fn new(
        link: Arc<dyn Link>,
        backend: Arc<impl Backend>,
        name: String,
        state: Arc<Mutex<State>>,
    ) -> Server {
        Server {
            link,
            backend,
            name: RwLock::new(name),
            state,
            scan: Mutex::new((false, 0)),
            lyrics: Mutex::new(Bounded::new(64)),
            info: Mutex::new(Bounded::new(256)),
            now_playing: Mutex::new(Vec::new()),
            catalogs: Mutex::new(HashMap::new()),
        }
    }

    /// The name the server calls itself in `getOpenSubsonicExtensions` and the landing page.
    pub fn name(&self) -> String {
        self.name.read().unwrap().clone()
    }

    /// The account named `name`, with what it may see worked out.
    pub async fn who(&self, name: &str) -> Result<Option<Who>, ApiError> {
        Ok(self.backend.who(name).await?)
    }

    /// The catalog `who` browses, from the backend unless this version is known already.
    pub async fn catalog(&self, who: &Who) -> Result<Arc<Catalog>, ApiError> {
        if let Some((version, catalog)) = self.catalogs.lock().unwrap().get(&who.tenant) {
            if *version == who.catalog_version {
                return Ok(catalog.clone());
            }
        }
        let catalog = self
            .backend
            .catalog(&who.tenant, who.catalog_version)
            .await?;
        let mut kept = self.catalogs.lock().unwrap();
        if kept.len() >= CATALOGS_KEPT && !kept.contains_key(&who.tenant) {
            kept.clear();
        }
        kept.insert(who.tenant.clone(), (who.catalog_version, catalog.clone()));
        Ok(catalog)
    }

    /// The audio of `req`, or a redirect to it.
    pub async fn media(&self, who: &Who, req: MediaRequest) -> Result<Media, ApiError> {
        Ok(self.backend.media(who, req).await?)
    }

    /// The picture `cover` names.
    pub async fn cover(
        &self,
        who: &Who,
        cover: &CoverRef,
        size: Option<u32>,
    ) -> Result<Media, ApiError> {
        Ok(self.backend.cover(who, cover, size).await?)
    }

    /// What `who` has noted about the library.
    pub async fn notes(&self, who: &Who) -> Result<Arc<UserNotes>, ApiError> {
        Ok(self.backend.notes(who).await?)
    }

    /// The name of the host's own account.
    pub fn username(&self) -> String {
        self.link.host_user()
    }

    /// A client changed the host's password: the app keeps it, and shows it.
    pub fn set_owner_password(&self, password: String) {
        let user = self.username();
        self.link.tell(Tell::Login {
            user: &user,
            password: &password,
        });
    }

    /// Tells the app a scan was asked for.
    pub fn tell_scan(&self) {
        self.link.tell(Tell::Scan);
    }

    /// Reports a client; the app keeps count of the ones it has seen.
    pub fn saw_client(&self, ip: IpAddr, client: &str) {
        self.link.tell(Tell::Client { ip, name: client });
    }

    /// Asks the app something and waits for the answer.
    pub async fn ask(&self, ask: Ask<'_>, timeout: Duration) -> Option<Vec<u8>> {
        self.link.ask(ask, timeout).await
    }

    /// Writes the extra state a moment from now, coalescing whatever else changes
    /// before then.
    pub fn save_soon(&self) {
        self.link.save_soon();
    }
}
