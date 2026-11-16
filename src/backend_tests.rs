//! Core against a tiny in-memory [`Backend`]: no Dart, no files, no Elbert state. These
//! prove the protocol works with whatever the host provides. [`Memory::managed`] is the
//! managed host's shape: audio behind redirects, managed capabilities, no app, no data dir.

use std::sync::{Arc, Mutex};

use md5::{Digest, Md5};
use serde_json::Value;
use sha2::Sha256;
use zeroize::Zeroizing;

use super::api::Answer;
use super::api::{dispatch, Params};
use super::auth::{authenticate, Who};
use super::backend::{
    ArtistInfo, Backend, BackendError, Capabilities, CoverRef, Edit, EditOutcome, ItemKind,
    Lockout, Lyrics, Media, MediaRequest, Peer, RadioStation, TenantId, UserNotes,
};
use super::browse::View;
use super::catalog::{Catalog, Pool, Raw};
use super::state::{Account, State};
use super::{Ask, Boxed, Link, Server, Tell};

/// One user, `amy`, with one library and one app password.
pub(crate) struct Memory {
    caps: Capabilities,
    catalog: Arc<Catalog>,
    pub(crate) notes: Mutex<UserNotes>,
    failed: Mutex<Vec<Option<String>>>,
}

pub(crate) fn who_amy() -> Who {
    Who {
        name: "amy".into(),
        owner: false,
        account: Account {
            password: "sesame".into(),
            stream: true,
            ..Default::default()
        },
        mask: u64::MAX,
        tenant: TenantId(Arc::from("t1")),
        catalog_version: 7,
    }
}

impl Memory {
    pub(crate) fn new() -> Memory {
        Memory::with(CAPABILITIES)
    }

    /// The managed host's capabilities.
    pub(crate) fn managed() -> Memory {
        Memory::with(Capabilities::managed())
    }

    fn with(caps: Capabilities) -> Memory {
        let mut pool = Pool::default();
        let raw: Vec<Raw> = [("s1", "First", "Alpha"), ("s2", "Second", "Beta")]
            .iter()
            .map(|(id, title, artist)| Raw {
                id: id.to_string(),
                path: format!("/{id}.mp3"),
                title: pool.get(title),
                artist: pool.get(artist),
                album: pool.get("Album"),
                album_artist: pool.get(artist),
                genre: pool.get("Rock"),
                cover: None,
                isrc: None,
                duration_ms: 3000,
                track: 1,
                disc: 1,
                year: 2020,
                bitrate: 128,
                sample_rate: 44_100,
                size: 1000,
                added_ms: 1_700_000_000_000,
            })
            .collect();
        Memory {
            caps,
            catalog: Arc::new(Catalog::build(vec!["/".into()], raw, 0)),
            notes: Mutex::new(UserNotes::default()),
            failed: Mutex::new(Vec::new()),
        }
    }
}

static CAPABILITIES: Capabilities = Capabilities {
    accounts: false,
    shares: false,
    radio: false,
    chat: false,
    podcasts: false,
    sonic: false,
    jukebox: false,
    scan: false,
    transcoding: false,
    managed: false,
};

impl Backend for Memory {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    async fn passwords(&self, user: &str) -> Result<Vec<Zeroizing<String>>, BackendError> {
        Ok(if user == "amy" {
            vec![Zeroizing::new("sesame".to_string())]
        } else {
            vec![]
        })
    }

    async fn api_key(&self, key_sha256: [u8; 32]) -> Result<Option<Who>, BackendError> {
        let known: [u8; 32] = Sha256::digest(b"elb_secret").into();
        Ok((key_sha256 == known).then(who_amy))
    }

    async fn who(&self, user: &str) -> Result<Option<Who>, BackendError> {
        Ok((user == "amy").then(who_amy))
    }

    async fn auth_failed(&self, user: Option<&str>, _peer: &Peer) -> Result<Lockout, BackendError> {
        let mut failed = self.failed.lock().unwrap();
        failed.push(user.map(str::to_string));
        Ok(if failed.len() > 3 {
            Lockout::Locked
        } else {
            Lockout::Open
        })
    }

    async fn catalog(&self, tenant: &TenantId, version: u64) -> Result<Arc<Catalog>, BackendError> {
        assert_eq!((&*tenant.0, version), ("t1", 7));
        Ok(self.catalog.clone())
    }

    async fn notes(&self, _who: &Who) -> Result<Arc<UserNotes>, BackendError> {
        Ok(Arc::new(self.notes.lock().unwrap().clone()))
    }

    async fn apply(&self, _who: &Who, edit: Edit) -> Result<EditOutcome, BackendError> {
        match edit {
            Edit::Star {
                kind: ItemKind::Song,
                id,
                on: true,
            } => {
                self.notes
                    .lock()
                    .unwrap()
                    .song_stars
                    .insert(id, 1_700_000_000_000);
                Ok(EditOutcome::Done)
            }
            Edit::RadioCreate {
                name,
                stream_url,
                home_page,
            } => {
                let mut notes = self.notes.lock().unwrap();
                let id = format!("ra-{}", notes.radio.len() + 1);
                notes.radio.push(RadioStation {
                    id: id.clone(),
                    name,
                    stream_url,
                    home_page,
                });
                Ok(EditOutcome::Created(id))
            }
            Edit::RadioUpdate {
                id,
                name,
                stream_url,
                home_page,
            } => {
                let mut notes = self.notes.lock().unwrap();
                let station = notes.radio.iter_mut().find(|s| s.id == id).unwrap();
                (station.name, station.stream_url, station.home_page) =
                    (name, stream_url, home_page);
                Ok(EditOutcome::Done)
            }
            Edit::RadioDelete { id } => {
                self.notes.lock().unwrap().radio.retain(|s| s.id != id);
                Ok(EditOutcome::Done)
            }
            other => Err(BackendError::Failed(format!(
                "not supported here: {other:?}"
            ))),
        }
    }

    /// A managed host: audio is somewhere else, with a link that expires.
    async fn media(&self, _who: &Who, req: MediaRequest) -> Result<Media, BackendError> {
        Ok(match req.id.as_str() {
            "s1" | "s2" => Media::Redirect {
                url: format!("https://objects.example/u/t1/{}?sig=abc", req.id),
                ttl_secs: 120,
            },
            _ => Media::NotFound,
        })
    }

    async fn cover(
        &self,
        _who: &Who,
        cover: &CoverRef,
        _size: Option<u32>,
    ) -> Result<Media, BackendError> {
        Ok(Media::Redirect {
            url: format!("https://objects.example/covers/{}", cover.0),
            ttl_secs: 120,
        })
    }

    async fn lyrics(&self, _who: &Who, song: &str) -> Result<Option<Lyrics>, BackendError> {
        Ok((song == "s1").then(|| Lyrics {
            text: "[00:01.00]one\n[00:02.00]two".into(),
        }))
    }

    async fn artist_info(
        &self,
        _who: &Who,
        artist: &str,
        _album: Option<&str>,
    ) -> Result<Option<ArtistInfo>, BackendError> {
        Ok((artist == "Alpha")
            .then(|| ArtistInfo(serde_json::json!({ "biography": "From the backend" }))))
    }
}

/// The app side: nothing to say, nothing to ask.
pub(crate) struct NoApp;

impl Link for NoApp {
    fn tell(&self, _event: Tell<'_>) {}

    fn ask<'a>(
        &'a self,
        _ask: Ask<'a>,
        _timeout: std::time::Duration,
    ) -> Boxed<'a, Option<Vec<u8>>> {
        Box::pin(async { None })
    }

    fn save_soon(&self) {}

    fn host_user(&self) -> String {
        String::new()
    }

    fn data_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    fn call<'a>(
        &'a self,
        _srv: &'a Arc<Server>,
        _view: &'a View<'a>,
        _call: &'a str,
        _p: &'a Params,
    ) -> Boxed<'a, Answer> {
        Box::pin(async { Err(super::respond::ApiError(0, "none".into())) })
    }
}

fn server() -> (Arc<Server>, Arc<Memory>) {
    server_of(Memory::new())
}

/// A server over `memory`.
pub(crate) fn server_of(memory: Memory) -> (Arc<Server>, Arc<Memory>) {
    let memory = Arc::new(memory);
    let srv = Server::new(
        Arc::new(NoApp),
        memory.clone(),
        "Test".into(),
        Arc::new(Mutex::new(State::default())),
    );
    (Arc::new(srv), memory)
}

pub(crate) fn params(pairs: &[(&str, &str)]) -> Params {
    Params(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

pub(crate) fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn checks_credentials_against_the_backends_secrets() {
    let (srv, memory) = server();
    let peer = Peer::default();
    let token = {
        let mut hash = Md5::new();
        hash.update(b"sesame");
        hash.update(b"salt");
        hash.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let login = |extra: &[(&str, &str)]| {
        let mut all = vec![("v", "1.16.1"), ("c", "test")];
        all.extend_from_slice(extra);
        run(authenticate(&srv, &params(&all), &peer))
    };
    assert_eq!(
        login(&[("u", "amy"), ("t", &token), ("s", "salt")])
            .unwrap()
            .name,
        "amy"
    );
    assert_eq!(
        login(&[("u", "amy"), ("p", "enc:736573616d65")])
            .unwrap()
            .name,
        "amy"
    );
    assert_eq!(login(&[("apiKey", "elb_secret")]).unwrap().name, "amy");
    assert_eq!(login(&[("u", "amy"), ("p", "wrong")]).err().unwrap().0, 40);
    assert_eq!(
        login(&[("u", "nobody"), ("p", "sesame")]).err().unwrap().0,
        40
    );
    assert_eq!(login(&[("apiKey", "elb_other")]).err().unwrap().0, 44);
    // Every refusal was reported, and the fourth locks the account.
    assert_eq!(memory.failed.lock().unwrap().len(), 3);
    assert_eq!(
        login(&[("u", "amy"), ("p", "wrong")]).err().unwrap().1,
        "Too many failed attempts"
    );
}

#[test]
fn answers_a_listing_from_the_backends_catalog() {
    let (srv, _) = server();
    let who = who_amy();
    let answer = run(dispatch(
        &srv,
        "search3",
        &params(&[("query", "")]),
        &who,
        "localhost",
    ))
    .unwrap();
    let songs = answer["searchResult3"]["song"].as_array().unwrap();
    let titles: Vec<&str> = songs.iter().map(|s| s["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["First", "Second"]);
    // Groups the host doesn't have are not there.
    let refused = run(dispatch(
        &srv,
        "getPodcasts",
        &params(&[]),
        &who,
        "localhost",
    ))
    .err()
    .unwrap();
    assert_eq!(refused.1, "No such call: getPodcasts");
}

#[test]
fn a_star_goes_to_the_backend_and_shows_in_the_next_read() {
    let (srv, memory) = server();
    let who = who_amy();
    run(dispatch(
        &srv,
        "star",
        &params(&[("id", "s2")]),
        &who,
        "localhost",
    ))
    .unwrap();
    assert!(memory.notes.lock().unwrap().song_stars.contains_key("s2"));
    let starred = run(dispatch(
        &srv,
        "getStarred2",
        &params(&[]),
        &who,
        "localhost",
    ))
    .unwrap();
    let ids: Vec<&Value> = starred["starred2"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["id"])
        .collect();
    assert_eq!(ids, ["s2"]);
    // An edit the backend refuses reaches the client as an error.
    let refused = run(dispatch(
        &srv,
        "setRating",
        &params(&[("id", "s1"), ("rating", "3")]),
        &Who {
            account: Account {
                comment: true,
                ..who.account.clone()
            },
            ..who
        },
        "localhost",
    ));
    assert_eq!(refused.err().unwrap().0, 0);
}

#[test]
fn a_redirect_is_a_302_and_is_never_cached() {
    let (srv, _) = server();
    let who = who_amy();
    let media = run(srv.media(
        &who,
        MediaRequest {
            id: "s1".into(),
            download: false,
        },
    ))
    .unwrap();
    let head = media.redirect_head().unwrap();
    assert_eq!(head.status, 302);
    assert!(head.headers.contains(&(
        "Location",
        "https://objects.example/u/t1/s1?sig=abc".to_string()
    )));
    assert!(head
        .headers
        .contains(&("Cache-Control", "no-store".to_string())));
    // Covers redirect the same way; only a redirect has such a head.
    let cover = run(srv.cover(&who, &CoverRef("cv-1".into()), Some(160))).unwrap();
    assert_eq!(cover.redirect_head().unwrap().status, 302);
    assert!(Media::NotFound.redirect_head().is_none());
    // What the backend does not have is not found, as a Subsonic error.
    let missing = run(srv.media(
        &who,
        MediaRequest {
            id: "nope".into(),
            download: true,
        },
    ))
    .unwrap();
    assert!(matches!(missing, Media::NotFound));
}

#[test]
fn lyrics_and_artist_info_come_from_the_backend() {
    let (srv, _) = server();
    let who = who_amy();
    let lyrics = run(dispatch(
        &srv,
        "getLyricsBySongId",
        &params(&[("id", "s1")]),
        &who,
        "localhost",
    ))
    .unwrap();
    let lines = lyrics["lyricsList"]["structuredLyrics"][0]["line"]
        .as_array()
        .unwrap();
    assert_eq!((lines.len(), lines[0]["start"].as_u64()), (2, Some(1000)));
    let none = run(dispatch(
        &srv,
        "getLyricsBySongId",
        &params(&[("id", "s2")]),
        &who,
        "localhost",
    ))
    .unwrap();
    assert!(none["lyricsList"]["structuredLyrics"]
        .as_array()
        .unwrap()
        .is_empty());
    let artists = run(dispatch(
        &srv,
        "getArtists",
        &params(&[]),
        &who,
        "localhost",
    ))
    .unwrap();
    let alpha = artists["artists"]["index"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["artist"].as_array().unwrap())
        .find(|a| a["name"] == "Alpha")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let info = run(dispatch(
        &srv,
        "getArtistInfo2",
        &params(&[("id", &alpha)]),
        &who,
        "localhost",
    ))
    .unwrap();
    assert_eq!(info["artistInfo2"]["biography"], "From the backend");
}
