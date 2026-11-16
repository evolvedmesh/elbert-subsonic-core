//! Who is asking, and what they may do.

use md5::{Digest, Md5};
use sha2::Sha256;
use zeroize::Zeroizing;

use super::api::Params;
use super::backend::{DynBackend, Lockout, Peer, TenantId};
use super::respond::{
    ApiError, ERR_AUTH_CONFLICT, ERR_BAD_API_KEY, ERR_CLIENT_OLD, ERR_CREDENTIALS, ERR_SERVER_OLD,
    MINOR_VERSION,
};
use super::state::Account;
use super::util::ct_eq;
use super::Server;

/// Who is asking, with what they may do.
pub struct Who {
    /// The account name.
    pub name: String,
    /// The host's own account: its song stars and ratings are the app's.
    pub owner: bool,
    /// The account's rights and password.
    pub account: Account,
    /// Music folders it may see: bit `n` for folder id `n + 1`.
    pub mask: u64,
    /// Whose library it browses.
    pub tenant: TenantId,
    /// The version of that library now; core fetches the catalog again when it changes.
    pub catalog_version: u64,
}

impl Who {
    /// Fails with "not authorized" unless `allowed`; `what` says what was refused.
    pub fn require(&self, allowed: bool, what: &str) -> Result<(), ApiError> {
        if allowed {
            Ok(())
        } else {
            Err(ApiError::not_allowed(what))
        }
    }

    /// Fails with "not authorized" unless the account is an administrator.
    pub fn admin(&self, what: &str) -> Result<(), ApiError> {
        self.require(self.account.admin, what)
    }
}

/// `v`: same major version, and a minor version no newer than this server's.
fn check_version(version: &str) -> Result<(), ApiError> {
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    if major < 1 {
        return Err(ApiError(ERR_CLIENT_OLD, "Client must upgrade".into()));
    }
    if major > 1 || minor > MINOR_VERSION {
        return Err(ApiError(ERR_SERVER_OLD, "Server must upgrade".into()));
    }
    Ok(())
}

/// A refusal, after telling the backend about the failed attempt.
async fn refusal(
    backend: &dyn DynBackend,
    user: Option<&str>,
    peer: &Peer,
    error: ApiError,
) -> ApiError {
    match backend.auth_failed(user, peer).await {
        Ok(Lockout::Locked) => ApiError(ERR_CREDENTIALS, "Too many failed attempts".into()),
        _ => error,
    }
}

/// Token, password (plain or `enc:` hex), or API key: exactly one of them. A wrong
/// credential is reported to the backend (`auth_failed`), which counts and locks out.
pub async fn authenticate<'a>(
    srv: &Server,
    params: &'a Params,
    peer: &Peer,
) -> Result<Who, ApiError> {
    check_version(params.need("v")?)?;
    params.need("c")?;
    let conflict = || {
        ApiError(
            ERR_AUTH_CONFLICT,
            "Multiple conflicting authentication mechanisms provided".into(),
        )
    };
    let (token, salt, password) = (params.get("t"), params.get("s"), params.get("p"));
    let backend = &srv.backend;
    let refuse =
        |user: Option<&'a str>, error: ApiError| refusal(backend.as_ref(), user, peer, error);
    let bad = || ApiError(ERR_CREDENTIALS, "Wrong username or password".into());

    if let Some(key) = params.get("apiKey") {
        if params.get("u").is_some() || token.is_some() || salt.is_some() || password.is_some() {
            return Err(conflict());
        }
        let digest: [u8; 32] = Sha256::digest(key.as_bytes()).into();
        return match backend.api_key(digest).await? {
            Some(who) => Ok(who),
            None => Err(refuse(None, ApiError(ERR_BAD_API_KEY, "Invalid API key".into())).await),
        };
    }
    if password.is_some() && (token.is_some() || salt.is_some()) {
        return Err(conflict());
    }

    let user = params.need("u")?;
    let secrets = backend.passwords(user).await?;
    if secrets.is_empty() || secrets.iter().all(|s| s.is_empty()) {
        return Err(refuse(Some(user), bad()).await);
    }
    // Every secret is compared, whichever matches, so the time says nothing.
    let good = match (token, salt, password) {
        (Some(token), Some(salt), None) => {
            let token = token.to_ascii_lowercase();
            secrets.iter().fold(false, |any, secret| {
                let mut hash = Md5::new();
                hash.update(secret.as_bytes());
                hash.update(salt.as_bytes());
                let expected: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
                ct_eq(&token, &expected) | any
            })
        }
        (None, None, Some(given)) => {
            let Some(given) = decode_password(given).map(Zeroizing::new) else {
                return Err(refuse(Some(user), bad()).await);
            };
            secrets
                .iter()
                .fold(false, |any, secret| ct_eq(&given, secret) | any)
        }
        (Some(_), None, None) => return Err(ApiError::missing("s")),
        (None, Some(_), None) => return Err(ApiError::missing("t")),
        _ => return Err(ApiError::missing("t")),
    };
    if good {
        if let Some(who) = backend.who(user).await? {
            return Ok(who);
        }
    }
    Err(refuse(Some(user), bad()).await)
}

/// A password parameter: as it is, or `enc:` and hex.
pub fn decode_password(given: &str) -> Option<String> {
    match given.strip_prefix("enc:") {
        Some(hex) => decode_hex(hex),
        None => Some(given.to_string()),
    }
}

fn decode_hex(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) || !hex.is_ascii() {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}
