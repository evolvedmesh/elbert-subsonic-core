//! The managed host's answers (ADR-0017 section 8): golden responses in XML and JSON for
//! each call group that differs from the on-device host, and the rules behind them.
//!
//! The goldens are in `tests/golden/managed-json.txt` and `managed-xml.txt`, one
//! `name<TAB>response` line per case. After a deliberate change, run the tests with
//! `UPDATE_GOLDEN=1`, then read the diff.

use std::sync::Arc;

use serde_json::Value;

use super::api::{dispatch, extensions, Answer, Params};
use super::auth::Who;
use super::backend::Capabilities;
use super::backend_tests::{params, run, server_of, who_amy, Memory};
use super::respond::{failed, ok, Format};
use super::state::Account;
use super::{NowPlaying, Server};

fn managed() -> (Arc<Server>, Arc<Memory>) {
    server_of(Memory::managed())
}

/// Amy as the backend might wrongly describe her: with every right. Core must not trust it.
fn who_overgrown() -> Who {
    let amy = who_amy();
    Who {
        account: Account {
            admin: true,
            jukebox: true,
            share: true,
            podcast: true,
            comment: true,
            ..amy.account.clone()
        },
        ..amy
    }
}

fn call(srv: &Arc<Server>, name: &str, pairs: &[(&str, &str)]) -> Answer {
    let mut all = vec![("c", "test")];
    all.extend_from_slice(pairs);
    run(dispatch(
        srv,
        name,
        &params(&all),
        &who_overgrown(),
        "localhost",
    ))
}

fn code(answer: Answer) -> u32 {
    answer.unwrap_err().0
}

/// The same answer as a golden line body in each format, the version made constant.
fn render(answer: &Answer) -> (String, String) {
    let both = |format: &Format| {
        let rendered = match answer {
            Ok(payload) => ok(format, payload.clone()),
            Err(error) => failed(format, error),
        };
        rendered
            .body
            .replace(env!("CARGO_PKG_VERSION"), "@VERSION@")
    };
    (both(&Format::Json), both(&Format::Xml))
}

fn check_golden(file: &str, actual: String) {
    let path = format!("{}/tests/golden/{file}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        actual, expected,
        "{file} differs; UPDATE_GOLDEN=1 rewrites it"
    );
}

#[test]
fn managed_answers_match_their_goldens_in_json_and_xml() {
    let (srv, _) = managed();
    // Each step is one call; the radio and now-playing steps depend on those before them.
    type Step<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);
    let steps: &[Step] = &[
        (
            "decision",
            "getTranscodeDecision",
            &[("mediaId", "s1"), ("mediaType", "song")],
        ),
        (
            "decision-unknown",
            "getTranscodeDecision",
            &[("mediaId", "x"), ("mediaType", "song")],
        ),
        (
            "transcode-stream",
            "getTranscodeStream",
            &[("mediaId", "s1")],
        ),
        ("hls", "hls.m3u8", &[("id", "s1")]),
        ("user", "getUser", &[("username", "amy")]),
        ("user-other", "getUser", &[("username", "bob")]),
        ("users", "getUsers", &[]),
        ("create-user", "createUser", &[("username", "bob")]),
        ("update-user", "updateUser", &[("username", "amy")]),
        ("delete-user", "deleteUser", &[("username", "amy")]),
        (
            "change-password",
            "changePassword",
            &[("username", "amy"), ("password", "x")],
        ),
        ("scan-status", "getScanStatus", &[]),
        ("start-scan", "startScan", &[]),
        ("shares", "getShares", &[]),
        ("create-share", "createShare", &[("id", "s1")]),
        ("update-share", "updateShare", &[("id", "sh-1")]),
        ("delete-share", "deleteShare", &[("id", "sh-1")]),
        ("chat", "getChatMessages", &[]),
        ("add-chat", "addChatMessage", &[("message", "hi")]),
        ("podcasts", "getPodcasts", &[]),
        ("newest-podcasts", "getNewestPodcasts", &[]),
        ("podcast-episode", "getPodcastEpisode", &[("id", "e1")]),
        (
            "create-podcast",
            "createPodcastChannel",
            &[("url", "https://feed.example/rss")],
        ),
        ("refresh-podcasts", "refreshPodcasts", &[]),
        ("jukebox", "jukeboxControl", &[("action", "get")]),
        ("videos", "getVideos", &[]),
        ("video-info", "getVideoInfo", &[("id", "v1")]),
        ("sonic-similar", "getSonicSimilarTracks", &[("id", "s1")]),
        (
            "sonic-path",
            "findSonicPath",
            &[("startSongId", "s1"), ("endSongId", "s2")],
        ),
        ("radio-empty", "getInternetRadioStations", &[]),
        (
            "radio-create",
            "createInternetRadioStation",
            &[
                ("name", "Jazz"),
                ("streamUrl", "https://radio.example/jazz"),
            ],
        ),
        (
            "radio-create-2",
            "createInternetRadioStation",
            &[
                ("name", "News"),
                ("streamUrl", "http://radio.example/news"),
                ("homepageUrl", "https://radio.example"),
            ],
        ),
        ("radio-list", "getInternetRadioStations", &[]),
        (
            "radio-update",
            "updateInternetRadioStation",
            &[
                ("id", "ra-1"),
                ("name", "Jazz FM"),
                ("streamUrl", "https://radio.example/jazz2"),
            ],
        ),
        (
            "radio-delete",
            "deleteInternetRadioStation",
            &[("id", "ra-2")],
        ),
        ("radio-list-after", "getInternetRadioStations", &[]),
        ("now-playing-empty", "getNowPlaying", &[]),
        (
            "scrobble-now",
            "scrobble",
            &[("id", "s1"), ("submission", "false")],
        ),
        ("now-playing", "getNowPlaying", &[]),
    ];
    let (mut json_lines, mut xml_lines) = (String::new(), String::new());
    for (name, call_name, pairs) in steps {
        let (json, xml) = render(&call(&srv, call_name, pairs));
        json_lines += &format!("{name}\t{json}\n");
        xml_lines += &format!("{name}\t{xml}\n");
    }
    let (json, xml) = {
        let list = extensions(&Capabilities::managed());
        render(&Ok(list))
    };
    json_lines += &format!("extensions\t{json}\n");
    xml_lines += &format!("extensions\t{xml}\n");
    check_golden("managed-json.txt", json_lines);
    check_golden("managed-xml.txt", xml_lines);
}

#[test]
fn managed_advertises_exactly_its_six_extensions() {
    let names = |caps: &Capabilities| -> Vec<String> {
        extensions(caps)["openSubsonicExtensions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        names(&Capabilities::managed()),
        [
            "apiKeyAuthentication",
            "formPost",
            "indexBasedQueue",
            "playbackReport",
            "songLyrics",
            "topSongsByArtistId"
        ]
    );
    // The on-device host is untouched.
    assert_eq!(names(&Capabilities::all()).len(), 10);
    assert!(!Capabilities::all().managed);
}

#[test]
fn the_roles_are_fixed_whatever_the_backend_says() {
    let (srv, _) = managed();
    let user = call(&srv, "getUser", &[("username", "amy")]).unwrap()["user"].clone();
    for (role, on) in [
        ("adminRole", false),
        ("settingsRole", false),
        ("jukeboxRole", false),
        ("shareRole", false),
        ("podcastRole", false),
        ("commentRole", false),
        ("streamRole", true),
        ("downloadRole", true),
        ("playlistRole", true),
    ] {
        assert_eq!(user[role], on, "{role}");
    }
    // Only the caller is there to be read.
    assert_eq!(code(call(&srv, "getUser", &[("username", "bob")])), 50);
    let users = call(&srv, "getUsers", &[]).unwrap();
    assert_eq!(users["users"]["user"].as_array().unwrap().len(), 1);
}

#[test]
fn writes_of_unavailable_groups_are_refused_with_error_50() {
    let (srv, _) = managed();
    for name in [
        "createUser",
        "updateUser",
        "deleteUser",
        "changePassword",
        "createShare",
        "updateShare",
        "deleteShare",
        "addChatMessage",
        "refreshPodcasts",
        "createPodcastChannel",
        "deletePodcastChannel",
        "deletePodcastEpisode",
        "downloadPodcastEpisode",
        "jukeboxControl",
    ] {
        assert_eq!(code(call(&srv, name, &[("id", "x")])), 50, "{name}");
    }
    for name in ["getTranscodeStream", "hls.m3u8", "hls"] {
        assert_eq!(code(call(&srv, name, &[])), 0, "{name}");
    }
}

#[test]
fn a_scan_never_runs_and_counts_the_items() {
    let (srv, _) = managed();
    for name in ["getScanStatus", "startScan"] {
        let status = &call(&srv, name, &[]).unwrap()["scanStatus"];
        assert_eq!(
            (status["scanning"].as_bool(), status["count"].as_u64()),
            (Some(false), Some(2))
        );
    }
    assert!(!srv.scan.lock().unwrap().0);
}

#[test]
fn rating_needs_no_comment_role_on_a_managed_host() {
    let (srv, _) = managed();
    // The test backend refuses a rating itself (error 0); the role check (error 50) let it by.
    let amy = who_amy();
    assert!(!amy.account.comment);
    let answer = run(dispatch(
        &srv,
        "setRating",
        &params(&[("id", "s1"), ("rating", "3")]),
        &amy,
        "localhost",
    ));
    assert_eq!(code(answer), 0);
}

#[test]
fn radio_stations_are_the_accounts_own_and_validated() {
    let (srv, memory) = managed();
    let create = |pairs: &[(&str, &str)]| call(&srv, "createInternetRadioStation", pairs);
    assert_eq!(
        code(create(&[
            ("name", "x"),
            ("streamUrl", "ftp://radio.example/x")
        ])),
        0
    );
    assert_eq!(
        code(create(&[
            ("name", " "),
            ("streamUrl", "https://radio.example/x")
        ])),
        0
    );
    assert_eq!(
        code(create(&[("streamUrl", "https://radio.example/x")])),
        10
    );
    assert_eq!(memory.notes.lock().unwrap().radio.len(), 0);
    assert_eq!(
        code(call(&srv, "deleteInternetRadioStation", &[("id", "nope")])),
        70
    );
    let update = [
        ("id", "nope"),
        ("name", "a"),
        ("streamUrl", "https://radio.example/x"),
    ];
    assert_eq!(code(call(&srv, "updateInternetRadioStation", &update)), 70);
}

#[test]
fn now_playing_shows_only_the_callers_own_plays() {
    let (srv, _) = managed();
    let playing = |user: &str| NowPlaying {
        song: Arc::from("s2"),
        user: user.into(),
        client: "other".into(),
        since_ms: super::wall_ms(),
        updated_ms: super::wall_ms(),
        state: None,
        position_ms: 0,
        rate: 1.0,
        counted: true,
    };
    srv.now_playing
        .lock()
        .unwrap()
        .extend([playing("bob"), playing("amy")]);
    let list = call(&srv, "getNowPlaying", &[]).unwrap();
    let users: Vec<&Value> = list["nowPlaying"]["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["username"])
        .collect();
    assert_eq!(users, ["amy"]);
}

#[test]
fn the_on_device_host_still_has_no_such_call_for_what_it_lacks() {
    let (srv, _) = server_of(Memory::new());
    let who = who_amy();
    for name in [
        "getTranscodeDecision",
        "getUsers",
        "getScanStatus",
        "jukeboxControl",
    ] {
        let refused = run(dispatch(&srv, name, &Params(vec![]), &who, "localhost"))
            .err()
            .unwrap();
        assert_eq!(refused.1, format!("No such call: {name}"));
    }
}
