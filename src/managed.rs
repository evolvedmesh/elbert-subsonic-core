//! The call groups of a managed host (`Capabilities::managed`), as ADR-0017 section 8 has
//! them: the managed library is the caller's own, so there is no account administration, no
//! server-side player, no shares, chat or podcasts, and no transcoding. Calls that cannot
//! work answer error 50 (or 0 where there is nothing to be authorised for), lists answer
//! empty, and internet radio is kept per account through the [`Backend`](super::Backend).
//!
//! Everything here is reached only through [`api::dispatch`](super::api::dispatch) on a host
//! whose capabilities say `managed`; the on-device host never gets here.

use std::sync::Arc;

use serde_json::{json, Value};

use super::admin::{user_json, web_url};
use super::api::{Answer, Params};
use super::auth::Who;
use super::backend::{Edit, UserNotes};
use super::browse::View;
use super::catalog::{suffix_of, Song};
use super::respond::{ApiError, ERR_GENERIC, ERR_NOT_ALLOWED};
use super::state::Account;
use super::Server;

/// The most stations an account may keep.
const MAX_STATIONS: usize = 100;
/// The longest station name, in bytes.
const MAX_NAME: usize = 200;
/// The longest station URL, in bytes.
const MAX_URL: usize = 2048;

/// The answer to `call` if a managed host answers it differently from the on-device host,
/// `None` for every other call (they go the ordinary way).
pub(crate) async fn answer(srv: &Arc<Server>, call: &str, p: &Params, who: &Who) -> Option<Answer> {
    Some(match call {
        // Transcoding is off: the client plays the original.
        "getTranscodeDecision" => decision(srv, p, who).await,
        "getTranscodeStream" | "hls.m3u8" | "hls" => {
            Err(ApiError(ERR_GENERIC, "Not supported on this server".into()))
        }
        // The caller is the only account; credentials are managed in Elbert Web.
        "getUser" => user(srv, p, who).await,
        "getUsers" => users(srv, who).await,
        "createUser" | "updateUser" | "deleteUser" | "changePassword" => {
            Err(refused("Accounts and passwords are managed in Elbert Web"))
        }
        // Uploads update the library; there is nothing to scan.
        "getScanStatus" | "startScan" => scan_status(srv, who).await,
        // Public share links would bypass the friends-only sharing model.
        "getShares" => Ok(json!({ "shares": { "share": [] } })),
        "createShare" | "updateShare" | "deleteShare" => Err(refused("Sharing is not available")),
        "getChatMessages" => Ok(json!({ "chatMessages": { "chatMessage": [] } })),
        "addChatMessage" => Err(refused("Chat is not available")),
        "getPodcasts" => Ok(json!({ "podcasts": { "channel": [] } })),
        "getNewestPodcasts" => Ok(json!({ "newestPodcasts": { "episode": [] } })),
        "getPodcastEpisode" => Err(ApiError::not_found("Podcast episode")),
        "refreshPodcasts"
        | "createPodcastChannel"
        | "deletePodcastChannel"
        | "deletePodcastEpisode"
        | "downloadPodcastEpisode" => Err(refused("Podcasts are not available")),
        "jukeboxControl" => Err(refused("There is no server-side player")),
        "getSonicSimilarTracks" | "findSonicPath" => Ok(json!({ "sonicMatch": [] })),
        // Stations are kept per account, by the backend.
        "getInternetRadioStations" => radio_stations(srv, who).await,
        "createInternetRadioStation" => create_radio(srv, p, who).await,
        "updateInternetRadioStation" => update_radio(srv, p, who).await,
        "deleteInternetRadioStation" => delete_radio(srv, p, who).await,
        _ => return None,
    })
}

fn refused(what: &str) -> ApiError {
    ApiError(ERR_NOT_ALLOWED, what.into())
}

/// The roles every managed account has, whatever the backend says: stream, download and
/// playlist, and no administration.
fn fixed_account() -> Account {
    Account {
        settings: false,
        stream: true,
        download: true,
        playlist: true,
        scrobbling: true,
        ..Default::default()
    }
}

async fn user(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    if p.need("username")? != who.name {
        return Err(refused("Only your own account can be read"));
    }
    let folders = srv.catalog(who).await?.folders.len();
    Ok(json!({ "user": user_json(&who.name, &fixed_account(), folders) }))
}

async fn users(srv: &Arc<Server>, who: &Who) -> Answer {
    let folders = srv.catalog(who).await?.folders.len();
    Ok(json!({ "users": { "user": [user_json(&who.name, &fixed_account(), folders)] } }))
}

async fn scan_status(srv: &Arc<Server>, who: &Who) -> Answer {
    let count = srv.catalog(who).await?.songs.len();
    Ok(json!({ "scanStatus": { "scanning": false, "count": count } }))
}

/// The container and codec of a song, in the words of the OpenSubsonic transcoding extension.
fn source_stream(song: &Song) -> Value {
    let suffix = suffix_of(&song.path);
    let (container, codec) = match suffix.as_str() {
        "m4a" | "m4b" | "aac" | "mp4" => ("mp4", "aac"),
        "alac" => ("mp4", "alac"),
        "ogg" | "oga" => ("ogg", "vorbis"),
        "wav" | "aif" | "aiff" => (suffix.as_str(), "pcm"),
        other => (other, other),
    };
    let mut value = json!({ "protocol": "http", "container": container, "codec": codec });
    if song.bitrate > 0 {
        value["audioBitrate"] = (song.bitrate * 1000).into();
    }
    if song.sample_rate > 0 {
        value["audioSamplerate"] = song.sample_rate.into();
    }
    value
}

/// `getTranscodeDecision`: always direct play, whatever profiles the client sent.
async fn decision(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let id = p.need("mediaId")?;
    if p.need("mediaType")? != "song" {
        return Err(ApiError::not_found("Media"));
    }
    let cat = srv.catalog(who).await?;
    let notes = Arc::<UserNotes>::default();
    let view = View {
        srv,
        cat: &cat,
        who,
        notes: &notes,
    };
    let song = view
        .visible_song(id)
        .ok_or_else(|| ApiError::not_found("Song"))?;
    Ok(json!({ "transcodeDecision": {
        "canDirectPlay": true,
        "canTranscode": false,
        "sourceStream": source_stream(song),
    } }))
}

fn station_json(id: &str, name: &str, stream_url: &str, home_page: Option<&str>) -> Value {
    let mut value = json!({ "id": id, "name": name, "streamUrl": stream_url });
    if let Some(home) = home_page {
        value["homePageUrl"] = home.into();
    }
    value
}

async fn radio_stations(srv: &Arc<Server>, who: &Who) -> Answer {
    let notes = srv.notes(who).await?;
    let list: Vec<Value> = notes
        .radio
        .iter()
        .map(|s| station_json(&s.id, &s.name, &s.stream_url, s.home_page.as_deref()))
        .collect();
    Ok(json!({ "internetRadioStations": { "internetRadioStation": list } }))
}

/// The station fields of a create or update: a bounded name and `http(s)` URLs.
fn station_fields(p: &Params) -> Result<(String, String, Option<String>), ApiError> {
    let name = p.need("name")?.trim().to_string();
    let stream_url = web_url(p.need("streamUrl")?)?;
    let home_page = p.get("homepageUrl").map(web_url).transpose()?;
    let too_long = |s: &str| s.len() > MAX_URL;
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(ApiError(
            ERR_GENERIC,
            "That station name is not usable".into(),
        ));
    }
    if too_long(&stream_url) || home_page.as_deref().is_some_and(too_long) {
        return Err(ApiError(ERR_GENERIC, "That address is too long".into()));
    }
    Ok((name, stream_url, home_page))
}

async fn create_radio(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let (name, stream_url, home_page) = station_fields(p)?;
    if srv.notes(who).await?.radio.len() >= MAX_STATIONS {
        return Err(ApiError(ERR_GENERIC, "There are too many stations".into()));
    }
    srv.backend
        .apply(
            who,
            Edit::RadioCreate {
                name,
                stream_url,
                home_page,
            },
        )
        .await?;
    Ok(json!({}))
}

async fn update_radio(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let id = p.need("id")?;
    let (name, stream_url, home_page) = station_fields(p)?;
    if !srv.notes(who).await?.radio.iter().any(|s| s.id == id) {
        return Err(ApiError::not_found("Station"));
    }
    srv.backend
        .apply(
            who,
            Edit::RadioUpdate {
                id: id.to_string(),
                name,
                stream_url,
                home_page,
            },
        )
        .await?;
    Ok(json!({}))
}

async fn delete_radio(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let id = p.need("id")?;
    if !srv.notes(who).await?.radio.iter().any(|s| s.id == id) {
        return Err(ApiError::not_found("Station"));
    }
    srv.backend
        .apply(who, Edit::RadioDelete { id: id.to_string() })
        .await?;
    Ok(json!({}))
}
