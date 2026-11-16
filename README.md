# elbert-subsonic-core

The transport-free core of the [Subsonic](https://www.subsonic.org/pages/api.jsp) and
[OpenSubsonic](https://opensubsonic.netlify.app/) API as [Elbert](https://github.com/evolvedmesh/elbert)
serves it. It holds the protocol and nothing else: request parameters, authentication, endpoint
dispatch, the catalog and its read model, and the XML, JSON and JSONP response encoders.

It has no sockets, no files, no FFI, no globals and no runtime of its own, and it is
`#![forbid(unsafe_code)]`. Who may log in, the library, each account's notes (stars, ratings,
bookmarks, playlists), audio, covers, lyrics and artist info come from a `Backend` you implement.
HTTP is the job of [`elbert-subsonic-server`](https://github.com/evolvedmesh/elbert-subsonic-server).

Licensed under the [MIT licence](LICENSE).

## Using it before 0.1.0 is on crates.io

Until `0.1.0` is published to crates.io, pin a git revision (a commit hash, never a branch):

```toml
[dependencies]
elbert-subsonic-core = { git = "https://github.com/evolvedmesh/elbert-subsonic-core", rev = "<commit>" }
```

## Public API

| Module | What it is |
|---|---|
| `backend` | The `Backend` trait and the types it exchanges: `Capabilities`, `Media`, `MediaRequest`, `CoverRef`, `TenantId`, `Peer`, `Lockout`, `Edit`, `EditOutcome`, `UserNotes`, `PlaylistInfo`, `Lyrics`, `ArtistInfo`, `BackendError`. |
| `api` | `dispatch` (one call: name, `Params`, `Who`, host) and `extensions` (the OpenSubsonic extension list). |
| `auth` | `authenticate` (token, password or API key: exactly one) and `Who` (the account that is asking and its rights). |
| `catalog` | `Catalog`, the read model of one library (songs, albums, artists, genres), built with `Catalog::build` from `Raw` rows. |
| `browse` | `View`, the browse, search and list calls over a catalog. |
| `edits` | The calls that change an account's notes: stars, ratings, bookmarks, playlists, play queue, lyrics. |
| `admin` | The account, share, radio and chat calls. |
| `respond` | `ApiError`, the error codes, `Format` and the encoders (`ok`, `failed`). |
| `state` | The persisted state of the extra call groups (accounts, API keys, shares, radio, chat, podcasts). |
| `util` | Range parsing, MIME helpers and a bounded cache. |

The crate root has `Server` (one hosted library: a `Link`, a `Backend`, the persisted `State`),
`Link` (the seam back to the embedding app) and the `Ask`, `Tell` and `NowPlaying` types.

### The `Backend` trait

`Backend` is declared with `trait_variant`: `async fn` methods whose futures are `Send`. One
implementation answers for every tenant. It provides:

- `capabilities` — which optional call groups exist (a call of an absent group is "no such call");
- `passwords`, `api_key`, `who`, `auth_failed` — authentication and lock-out;
- `catalog` — the tenant's library at a version (core keeps it until the version changes);
- `notes` and `apply` — what an account has noted, and the edits it makes;
- `media`, `cover` — what to send for audio and pictures;
- `lyrics`, `artist_info` — optional extras.

Core never stores anything itself: persistence is the backend's business.

### `Media`

What a backend answers for audio or a cover: `File` (a local file, the host serves it with `Range`),
`Bytes` (in memory, e.g. a cover), `Redirect` (a presigned URL the client follows with a `302`; the
URL is a bearer secret and must never be logged or cached) or `Transcode` (a file to decode and
encode to MP3, Opus or WAV, which needs `elbert-subsonic-server`'s `transcode` feature), or
`NotFound`.

### `Capabilities`

A struct of booleans, one per optional call group: `accounts`, `shares`, `radio`, `chat`,
`podcasts`, `sonic`, `jukebox`, `scan` and `transcoding`. They decide which calls dispatch accepts
and which OpenSubsonic extensions are advertised.

## Development

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check
```

The toolchain is pinned in `rust-toolchain.toml`; the minimum supported Rust version of the crate
is 1.87. CI (`.github/workflows/ci.yml`) runs the four commands above. Pushing a `v*` tag runs
`release.yml`: the same checks, then `cargo publish` with the repository secret
`CARGO_REGISTRY_TOKEN`.

The tests drive the protocol through a small in-memory `Backend` (`src/backend_tests.rs`). The
golden responses of the full host stay in the Elbert repository, which is the consumer of this
crate.
