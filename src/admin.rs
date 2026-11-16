//! Accounts, shares, internet radio, chat and the jukebox.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::api::{Answer, Params};
use super::auth::{decode_password, Who};
use super::browse::View;
use super::catalog::Catalog;
use super::respond::{iso8601, ApiError, ERR_GENERIC};
use super::state::{Account, Chat, Radio, Share};
use super::{random_id, wall_ms, Ask, Server};

const MAX_CHAT: usize = 200;

// ---- Accounts ----------------------------------------------------------------

/// The `user` object of `getUser` for the account `name`.
pub fn user_json(name: &str, account: &Account, folder_count: usize) -> Value {
    let folders: Vec<u32> = if account.folders.is_empty() {
        (1..=folder_count as u32).collect()
    } else {
        account.folders.clone()
    };
    let mut value = json!({
        "username": name,
        "email": &account.email,
        "scrobblingEnabled": account.scrobbling,
        "adminRole": account.admin,
        "settingsRole": account.settings,
        "downloadRole": account.download,
        "uploadRole": account.upload,
        "playlistRole": account.playlist,
        "coverArtRole": account.cover_art,
        "commentRole": account.comment,
        "podcastRole": account.podcast,
        "streamRole": account.stream,
        "jukeboxRole": account.jukebox,
        "shareRole": account.share,
        "videoConversionRole": account.video_conversion,
        "folder": folders,
    });
    if account.max_bit_rate > 0 {
        value["maxBitRate"] = account.max_bit_rate.into();
    }
    value
}

/// `getUser`: one account (the caller's own, or any for an administrator).
pub async fn get_user(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let name = p.need("username")?;
    if name != who.name {
        who.admin("Reading another account")?;
    }
    let target = srv
        .who(name)
        .await?
        .ok_or_else(|| ApiError::not_found("User"))?;
    Ok(json!({ "user": user_json(name, &target.account, srv.catalog(who).await?.folders.len()) }))
}

/// `getUsers`: every account. Administrators only.
pub async fn get_users(srv: &Arc<Server>, who: &Who) -> Answer {
    who.admin("Listing accounts")?;
    let folders = srv.catalog(who).await?.folders.len();
    let mut names = vec![srv.username()];
    names.extend(srv.state.lock().unwrap().accounts.keys().cloned());
    let mut users: Vec<Value> = Vec::new();
    for name in &names {
        if let Some(found) = srv.who(name).await? {
            users.push(user_json(name, &found.account, folders));
        }
    }
    Ok(json!({ "users": { "user": users } }))
}

fn apply_roles(account: &mut Account, p: &Params) {
    let flags: [(&str, &mut bool); 13] = [
        ("adminRole", &mut account.admin),
        ("settingsRole", &mut account.settings),
        ("streamRole", &mut account.stream),
        ("jukeboxRole", &mut account.jukebox),
        ("downloadRole", &mut account.download),
        ("uploadRole", &mut account.upload),
        ("playlistRole", &mut account.playlist),
        ("coverArtRole", &mut account.cover_art),
        ("commentRole", &mut account.comment),
        ("podcastRole", &mut account.podcast),
        ("shareRole", &mut account.share),
        ("videoConversionRole", &mut account.video_conversion),
        ("scrobblingEnabled", &mut account.scrobbling),
    ];
    for (key, flag) in flags {
        if let Some(value) = p.truthy(key) {
            *flag = value;
        }
    }
    if let Some(email) = p.get("email") {
        account.email = email.to_string();
    }
    if let Some(rate) = p.int("maxBitRate") {
        account.max_bit_rate = rate.max(0) as u32;
    }
    let folders: Vec<u32> = p
        .all("musicFolderId")
        .iter()
        .filter_map(|f| f.parse().ok())
        .collect();
    if !folders.is_empty() {
        account.folders = folders;
    }
}

fn password_param(p: &Params, key: &str) -> Result<Option<String>, ApiError> {
    match p.get(key) {
        None => Ok(None),
        Some(given) => {
            let decoded = decode_password(given).filter(|d| !d.is_empty());
            decoded
                .map(Some)
                .ok_or_else(|| ApiError(ERR_GENERIC, "That is not a usable password".into()))
        }
    }
}

/// `createUser`. Administrators only.
pub fn create_user(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    who.admin("Creating accounts")?;
    let name = p.need("username")?.trim().to_string();
    let password = password_param(p, "password")?.ok_or_else(|| ApiError::missing("password"))?;
    p.need("email")?;
    if name.is_empty() || name == srv.username() {
        return Err(ApiError(ERR_GENERIC, "That username is taken".into()));
    }
    let mut account = Account {
        password,
        ..Default::default()
    };
    apply_roles(&mut account, p);
    {
        let mut state = srv.state.lock().unwrap();
        if state.accounts.contains_key(&name) {
            return Err(ApiError(ERR_GENERIC, "That username is taken".into()));
        }
        state.accounts.insert(name, account);
    }
    srv.save_soon();
    Ok(json!({}))
}

/// `updateUser`. Administrators only.
pub fn update_user(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    who.admin("Changing accounts")?;
    let name = p.need("username")?;
    let password = password_param(p, "password")?;
    if name == srv.username() {
        // The host's account is what the app and its QR code are built on: it
        // keeps every role and only its password can change.
        if let Some(password) = password {
            srv.set_owner_password(password);
        }
        return Ok(json!({}));
    }
    {
        let mut state = srv.state.lock().unwrap();
        let account = state
            .accounts
            .get_mut(name)
            .ok_or_else(|| ApiError::not_found("User"))?;
        apply_roles(account, p);
        if let Some(password) = password {
            account.password = password;
        }
    }
    srv.save_soon();
    Ok(json!({}))
}

/// `deleteUser`. Administrators only.
pub fn delete_user(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    who.admin("Removing accounts")?;
    let name = p.need("username")?;
    if name == srv.username() {
        return Err(ApiError::not_allowed("Removing the host's account"));
    }
    remove_account(srv, name).ok_or_else(|| ApiError::not_found("User"))?;
    Ok(json!({}))
}

/// Drops the account and everything that lets it in or was only its own.
pub fn remove_account(srv: &Arc<Server>, name: &str) -> Option<()> {
    {
        let mut state = srv.state.lock().unwrap();
        state.accounts.remove(name)?;
        state.users.remove(name);
        state.api_keys.retain(|_, key| key.user != name);
        state.playlists.retain(|playlist| playlist.owner != name);
    }
    srv.save_soon();
    Some(())
}

/// `changePassword`: the caller's own password, or any for an administrator.
pub fn change_password(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let name = p.need("username")?;
    if name == who.name {
        who.require(
            who.account.settings || who.account.admin,
            "Changing the password",
        )?;
    } else {
        who.admin("Changing another account's password")?;
    }
    let password = password_param(p, "password")?.ok_or_else(|| ApiError::missing("password"))?;
    if name == srv.username() {
        srv.set_owner_password(password);
        return Ok(json!({}));
    }
    {
        let mut state = srv.state.lock().unwrap();
        state
            .accounts
            .get_mut(name)
            .ok_or_else(|| ApiError::not_found("User"))?
            .password = password;
    }
    srv.save_soon();
    Ok(json!({}))
}

// ---- Shares ------------------------------------------------------------------

fn base_url(host: &str) -> String {
    format!("http://{host}")
}

fn share_json(view: &View, id: &str, share: &Share, base: &str) -> Value {
    let songs: Vec<Value> = share
        .songs
        .iter()
        .filter_map(|s| view.cat.song(s))
        .map(|s| view.song_json(s))
        .collect();
    let mut value = json!({
        "id": id,
        "url": format!("{base}/share/{id}"),
        "username": &share.user,
        "created": iso8601(share.created_ms),
        "visitCount": share.visits,
        "entry": songs,
    });
    if !share.description.is_empty() {
        value["description"] = share.description.clone().into();
    }
    if let Some(when) = share.expires_ms {
        value["expires"] = iso8601(when).into();
    }
    if let Some(when) = share.last_visited_ms {
        value["lastVisited"] = iso8601(when).into();
    }
    value
}

/// `getShares`: the shares the caller may see, with their public URLs under `host`.
pub fn get_shares(srv: &Arc<Server>, view: &View, host: &str) -> Answer {
    let shares: Vec<(String, Share)> = srv
        .state
        .lock()
        .unwrap()
        .shares
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let base = base_url(host);
    let list: Vec<Value> = shares
        .iter()
        .filter(|(_, share)| view.who.account.admin || share.user == view.who.name)
        .map(|(id, share)| share_json(view, id, share, &base))
        .collect();
    Ok(json!({ "shares": { "share": list } }))
}

/// `createShare`: shares songs, an album or a playlist under `host`.
pub fn create_share(srv: &Arc<Server>, view: &View, p: &Params, host: &str) -> Answer {
    view.who.require(view.who.account.share, "Sharing")?;
    let ids = p.all("id");
    if ids.is_empty() {
        return Err(ApiError::missing("id"));
    }
    let songs = expand_songs(view.cat, &ids);
    if songs.is_empty() {
        return Err(ApiError::not_found("Media"));
    }
    let share = Share {
        user: view.who.name.clone(),
        description: p.get("description").unwrap_or("").to_string(),
        created_ms: wall_ms(),
        expires_ms: p.int("expires").filter(|&e| e > 0),
        last_visited_ms: None,
        visits: 0,
        songs,
    };
    let id = format!("sh-{}", random_id());
    srv.state
        .lock()
        .unwrap()
        .shares
        .insert(id.clone(), share.clone());
    srv.save_soon();
    Ok(json!({ "shares": { "share": [share_json(view, &id, &share, &base_url(host))] } }))
}

fn expand_songs(cat: &Catalog, ids: &[&str]) -> Vec<String> {
    let mut songs = Vec::new();
    for id in ids {
        if let Some(song) = cat.song(id) {
            songs.push(song.id.to_string());
        } else if let Some(album) = cat.album(id) {
            songs.extend(
                album
                    .songs
                    .iter()
                    .map(|&i| cat.songs[i as usize].id.to_string()),
            );
        }
    }
    songs
}

fn own_share<'a>(
    state: &'a mut super::state::State,
    id: &str,
    who: &Who,
) -> Result<&'a mut Share, ApiError> {
    let share = state
        .shares
        .get_mut(id)
        .ok_or_else(|| ApiError::not_found("Share"))?;
    if !who.account.admin && share.user != who.name {
        return Err(ApiError::not_allowed("Changing someone else's share"));
    }
    Ok(share)
}

/// `updateShare`: changes a share's description or expiry.
pub fn update_share(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    who.require(who.account.share, "Sharing")?;
    {
        let mut state = srv.state.lock().unwrap();
        let share = own_share(&mut state, p.need("id")?, who)?;
        if let Some(description) = p.get("description") {
            share.description = description.to_string();
        }
        if let Some(expires) = p.int("expires") {
            share.expires_ms = (expires > 0).then_some(expires);
        }
    }
    srv.save_soon();
    Ok(json!({}))
}

/// `deleteShare`.
pub fn delete_share(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    who.require(who.account.share, "Sharing")?;
    let id = p.need("id")?;
    {
        let mut state = srv.state.lock().unwrap();
        own_share(&mut state, id, who)?;
        state.shares.remove(id);
    }
    srv.save_soon();
    Ok(json!({}))
}

// ---- Internet radio ----------------------------------------------------------

/// `getInternetRadioStations`.
pub fn radio_stations(srv: &Arc<Server>) -> Answer {
    let state = srv.state.lock().unwrap();
    let list: Vec<Value> = state
        .radio
        .iter()
        .map(|(id, station)| {
            let mut value =
                json!({ "id": id, "name": &station.name, "streamUrl": &station.stream_url });
            if let Some(home) = &station.home_page {
                value["homePageUrl"] = home.clone().into();
            }
            value
        })
        .collect();
    Ok(json!({ "internetRadioStations": { "internetRadioStation": list } }))
}

pub(crate) fn web_url(text: &str) -> Result<String, ApiError> {
    if text.starts_with("http://") || text.starts_with("https://") {
        Ok(text.to_string())
    } else {
        Err(ApiError(
            ERR_GENERIC,
            "A station's address must be an http(s) URL".into(),
        ))
    }
}

/// `createInternetRadioStation`. Administrators only.
pub fn create_radio(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    who.admin("Changing radio stations")?;
    let station = Radio {
        name: p.need("name")?.to_string(),
        stream_url: web_url(p.need("streamUrl")?)?,
        home_page: p.get("homepageUrl").map(web_url).transpose()?,
    };
    srv.state
        .lock()
        .unwrap()
        .radio
        .insert(format!("ra-{}", random_id()), station);
    srv.save_soon();
    Ok(json!({}))
}

/// `updateInternetRadioStation`. Administrators only.
pub fn update_radio(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    who.admin("Changing radio stations")?;
    let id = p.need("id")?;
    let stream_url = web_url(p.need("streamUrl")?)?;
    let name = p.need("name")?.to_string();
    let home_page = p.get("homepageUrl").map(web_url).transpose()?;
    {
        let mut state = srv.state.lock().unwrap();
        let station = state
            .radio
            .get_mut(id)
            .ok_or_else(|| ApiError::not_found("Station"))?;
        station.name = name;
        station.stream_url = stream_url;
        station.home_page = home_page;
    }
    srv.save_soon();
    Ok(json!({}))
}

/// `deleteInternetRadioStation`. Administrators only.
pub fn delete_radio(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    who.admin("Changing radio stations")?;
    srv.state
        .lock()
        .unwrap()
        .radio
        .remove(p.need("id")?)
        .ok_or_else(|| ApiError::not_found("Station"))?;
    srv.save_soon();
    Ok(json!({}))
}

// ---- Chat --------------------------------------------------------------------

/// `getChatMessages`.
pub fn chat_messages(srv: &Arc<Server>, p: &Params) -> Answer {
    let since = p.int("since").unwrap_or(0);
    let state = srv.state.lock().unwrap();
    let list: Vec<Value> = state
        .chat
        .iter()
        .filter(|m| m.time_ms > since)
        .map(|m| json!({ "username": &m.user, "time": m.time_ms, "message": &m.message }))
        .collect();
    Ok(json!({ "chatMessages": { "chatMessage": list } }))
}

/// `addChatMessage`.
pub fn add_chat(srv: &Arc<Server>, p: &Params, who: &Who) -> Answer {
    let message = p.need("message")?.to_string();
    {
        let mut state = srv.state.lock().unwrap();
        state.chat.push(Chat {
            user: who.name.clone(),
            time_ms: wall_ms(),
            message,
        });
        let extra = state.chat.len().saturating_sub(MAX_CHAT);
        state.chat.drain(..extra);
    }
    srv.save_soon();
    Ok(json!({}))
}

// ---- Jukebox -----------------------------------------------------------------

/// This device's player, as Dart reports it: `{currentIndex, playing, gain,
/// position, ids}`.
pub async fn jukebox(srv: &Arc<Server>, view: &View<'_>, p: &Params, who: &Who) -> Answer {
    who.require(who.account.jukebox, "The jukebox")?;
    let action = p.need("action")?;
    if ![
        "get", "status", "set", "start", "stop", "skip", "add", "clear", "remove", "shuffle",
        "setGain",
    ]
    .contains(&action)
    {
        return Err(ApiError(
            ERR_GENERIC,
            format!("Unknown jukebox action: {action}"),
        ));
    }
    let ids: Vec<String> = p
        .all("id")
        .into_iter()
        .filter(|id| view.cat.song(id).is_some())
        .map(str::to_string)
        .collect();
    let index = p.int("index").unwrap_or(-1);
    let offset = p.int("offset").unwrap_or(0).max(0) as u64;
    let gain = p
        .get("gain")
        .and_then(|g| g.parse::<f64>().ok())
        .unwrap_or(-1.0);
    let answer = srv
        .ask(
            Ask::Jukebox {
                action,
                index,
                offset,
                gain,
                ids: &ids,
            },
            Duration::from_secs(10),
        )
        .await
        .ok_or_else(|| ApiError(ERR_GENERIC, "The player did not answer".into()))?;
    let status: Value = serde_json::from_slice(&answer)
        .map_err(|_| ApiError(ERR_GENERIC, "The player can't be used right now".into()))?;
    // The API has always called it `gain`, which is what clients read; the
    // OpenSubsonic schema requires `volume`. Both, so each is satisfied.
    let gain = status["gain"].as_f64().unwrap_or(1.0);
    let mut out = json!({
        "currentIndex": status["currentIndex"].as_i64().unwrap_or(-1),
        "playing": status["playing"].as_bool().unwrap_or(false),
        "gain": gain,
        "volume": gain,
        "position": status["position"].as_i64().unwrap_or(0),
    });
    if action == "get" {
        let entries: Vec<Value> = status["ids"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| view.cat.song(id.as_str()?))
                    .map(|s| view.song_json(s))
                    .collect()
            })
            .unwrap_or_default();
        out["entry"] = entries.into();
        return Ok(json!({ "jukeboxPlaylist": out }));
    }
    Ok(json!({ "jukeboxStatus": out }))
}
