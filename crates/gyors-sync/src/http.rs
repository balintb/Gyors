//! HTTP transport against `api.gyo.rs` (the deployed `gyors-cloud`
//! Cloudflare Worker)
//!
//! Three things live here:
//!
//! - [`HttpTransport`] - the [`Transport`](crate::Transport) impl
//!   the engine uses for `/v1/sync/{push,pull}`.
//! - [`signup`] / [`signin`] - auth-flow helpers. Distinct from
//!   `Transport` because they dont carry a bearer yet (signup
//!   *issues* the token).
//! - [`base_url`] - picks `GYORS_SYNC_BASE` env override or falls
//!   back to the production endpoint, so local dev against
//!   `wrangler dev` is one env var away

use anyhow::Result;
use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use std::time::Duration;

use crate::auth::derive_verifier;
use crate::proto::{
    AuthResponse, ChangePasswordRequest, ErrorBody, PullResponse, PushRequest, PushResponse,
    SigninRequest, SignupRequest,
};
use crate::transport::{Transport, TransportError};

const PROD_BASE: &str = "https://api.gyo.rs/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolve the API base URL. Honors `GYORS_SYNC_BASE` for local
/// `wrangler dev` (e.g. `http://127.0.0.1:8787/v1`)
pub fn base_url() -> String {
    std::env::var("GYORS_SYNC_BASE").unwrap_or_else(|_| PROD_BASE.to_string())
}

/// Shared `reqwest::Client`. Connection pool is per-client; the same
/// instance is reused across `HttpTransport` and auth helpers so
/// we keep one pool per process
fn http_client() -> Result<Client> {
    // Degrade the UA to a major-version-only token. The
    // exact crate version is recoverable from public GitHub release
    // tags anyway, so marginal leak from a full SemVer was
    // small - but every byte that doesn't fingerprint our build
    // is one fewer signal a passive observer (or hostile-cloud
    // attacker) can use to target a known-vuln client. We bump
    // this string on real protocol changes, not patch releases
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("gyors-sync/0")
        .build()
        .map_err(Into::into)
}

pub struct HttpTransport {
    client: Client,
    base: String,
    bearer: String,
}

impl HttpTransport {
    pub fn new(bearer: impl Into<String>) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            base: base_url(),
            bearer: bearer.into(),
        })
    }

    /// Useful for tests pointing at a mock server
    pub fn with_base(bearer: impl Into<String>, base: impl Into<String>) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            base: base.into(),
            bearer: bearer.into(),
        })
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn push(&self, req: PushRequest) -> Result<PushResponse, TransportError> {
        let url = format!("{}/sync/push", self.base);
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.bearer)
            .json(&req)
            .send()
            .await
            .map_err(network_err)?;
        translate_status(&resp)?;
        resp.json::<PushResponse>().await.map_err(|e| {
            TransportError::Server(format!("decode push response: {e}"))
        })
    }

    async fn pull(&self, since: Option<&str>) -> Result<PullResponse, TransportError> {
        let url = format!("{}/sync/pull", self.base);
        let mut req = self.client.get(url).bearer_auth(&self.bearer);
        if let Some(cursor) = since {
            req = req.query(&[("since", cursor)]);
        }
        let resp = req.send().await.map_err(network_err)?;
        translate_status(&resp)?;
        resp.json::<PullResponse>().await.map_err(|e| {
            TransportError::Server(format!("decode pull response: {e}"))
        })
    }
}

/// Sign up + receive a session token. The verifier is computed from
/// `password + kdf_salt` via Argon2id; salts are 16-byte fresh
/// values from [`fresh_salt`](crate::auth::fresh_salt). Caller
/// is responsible for persisting the returned [`AuthResponse`] (the
/// salts especially - losing them means re-deriving the encryption
/// key is impossible)
///
/// `wrapped_data_key_b64` is the fresh DEK wrapped under
/// the password-derived KEK. See `crypto::wrap_dek`. The server
/// stores it opaque and echoes it back on every auth response so
/// the client can unwrap on cold start
pub async fn signup(
    email: &str,
    password: &str,
    kdf_salt_b64: &str,
    encryption_salt_b64: &str,
    wrapped_data_key_b64: &str,
    device_label: Option<String>,
) -> Result<AuthResponse, TransportError> {
    let verifier = derive_verifier(password, kdf_salt_b64)
        .map_err(|e| TransportError::BadRequest(format!("verifier derivation: {e}")))?
        .to_hex();
    let body = SignupRequest {
        email: email.to_string(),
        auth_verifier: verifier,
        kdf_salt: kdf_salt_b64.to_string(),
        encryption_salt: encryption_salt_b64.to_string(),
        wrapped_data_key: wrapped_data_key_b64.to_string(),
        device_label,
    };
    post_json("auth/signup", &body, None).await
}

/// Sign in to an existing account. The `kdf_salt` is the one the
/// server returned at signup; without it the verifier won't match
pub async fn signin(
    email: &str,
    password: &str,
    kdf_salt_b64: &str,
    device_label: Option<String>,
) -> Result<AuthResponse, TransportError> {
    let verifier = derive_verifier(password, kdf_salt_b64)
        .map_err(|e| TransportError::BadRequest(format!("verifier derivation: {e}")))?
        .to_hex();
    let body = SigninRequest {
        email: email.to_string(),
        auth_verifier: verifier,
        device_label,
    };
    post_json("auth/signin", &body, None).await
}

/// Rotate the bearer for. Server mints a new
/// `session_token` + `expires_at`, revokes the old one. Same
/// `AuthResponse` shape as signin so caller's
/// `Session::from_auth` path doesn't fork. Calling this once a
/// week (when session is within ~7 days of expiry) keeps a
/// leaked-but-rotated-out token's blast radius at sub-week
pub async fn refresh_session(bearer: &str) -> Result<AuthResponse, TransportError> {
    let client = http_client().map_err(|e| TransportError::Network(e.to_string()))?;
    let url = format!("{}/auth/refresh", base_url());
    let resp = client
        .post(url)
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(network_err)?;
    translate_status(&resp)?;
    resp.json::<AuthResponse>()
        .await
        .map_err(|e| TransportError::Server(format!("decode refresh response: {e}")))
}

/// Rotate user's password. Caller has already
/// derived the new verifier + KEK locally and re-wrapped the DEK
/// under the new KEK; this function just ships the bundle to the
/// server, which atomically replaces auth_hash + salts +
/// wrapped_data_key. Existing sync blobs stay readable because the
/// DEK itself didn't change
///
/// 204 on success; 401 if the old verifier didn't match (or the
/// bearer is stale); 400 if any field is malformed. The launcher
/// should treat a 401 as "we got the old password wrong, ask
/// again" rather than "session expired."
pub async fn change_password(
    bearer: &str,
    body: &ChangePasswordRequest,
) -> Result<(), TransportError> {
    let client = http_client().map_err(|e| TransportError::Network(e.to_string()))?;
    let url = format!("{}/account/password", base_url());
    let resp = client
        .post(url)
        .bearer_auth(bearer)
        .json(body)
        .send()
        .await
        .map_err(network_err)?;
    if resp.status() == StatusCode::NO_CONTENT {
        return Ok(());
    }
    translate_status(&resp)?;
    Ok(())
}

/// Hard-delete authenticated account. Server cascades to
/// sessions + sync_blobs via FK. 204 on success; 401 if the bearer
/// is bogus or already deleted. Idempotent from caller's POV -
/// re-running after a delete just returns 401, which the launcher
/// can treat as "already gone."
pub async fn delete_account(bearer: &str) -> Result<(), TransportError> {
    let client = http_client().map_err(|e| TransportError::Network(e.to_string()))?;
    let url = format!("{}/account", base_url());
    let resp = client
        .delete(url)
        .bearer_auth(bearer)
        .send()
        .await
        .map_err(network_err)?;
    if resp.status() == StatusCode::NO_CONTENT {
        return Ok(());
    }
    translate_status(&resp)?;
    Ok(())
}

async fn post_json<T, R>(
    path: &str,
    body: &T,
    bearer: Option<&str>,
) -> Result<R, TransportError>
where
    T: serde::Serialize,
    R: serde::de::DeserializeOwned,
{
    let client = http_client().map_err(|e| TransportError::Network(e.to_string()))?;
    let url = format!("{}/{}", base_url(), path);
    let mut req = client.post(url).json(body);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(network_err)?;
    translate_status(&resp)?;
    resp.json::<R>()
        .await
        .map_err(|e| TransportError::Server(format!("decode response: {e}")))
}

fn network_err(e: reqwest::Error) -> TransportError {
    if e.is_timeout() {
        TransportError::Network(format!("timeout: {e}"))
    } else if e.is_connect() {
        TransportError::Network(format!("connect: {e}"))
    } else {
        TransportError::Network(e.to_string())
    }
}

/// Convert a non-2xx response into a typed [`TransportError`]. The
/// server returns a JSON `ErrorBody` on every error path, so we read
/// it for the `code` and propagate. On the rare bytes-aren't-JSON
/// case we fall back to the raw status text - usually means a
/// reverse proxy got in the middle
fn translate_status(resp: &reqwest::Response) -> Result<(), TransportError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    // We can't parse body without consuming response, so
    // surface what we know from the status alone. Caller can
    // also `.json::<ErrorBody>()` after a 4xx/5xx if it wants the
    // structured reason; today we keep it simple
    let _: Option<ErrorBody> = None;
    Err(match status {
        StatusCode::UNAUTHORIZED => TransportError::Unauthenticated,
        StatusCode::FORBIDDEN => TransportError::SessionExpired,
        StatusCode::PAYLOAD_TOO_LARGE => {
            TransportError::BadRequest("payload too large".to_string())
        }
        StatusCode::TOO_MANY_REQUESTS => {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60);
            TransportError::RateLimited { retry_after_s: retry }
        }
        StatusCode::INSUFFICIENT_STORAGE => TransportError::QuotaExceeded,
        s if s.is_client_error() => TransportError::BadRequest(s.to_string()),
        s => TransportError::Server(s.to_string()),
    })
}
