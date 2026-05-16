//! C FFI surface for the cloud-sync flows. Lives next to the rest
//! of the IPC layer because Swift only has one shared library to
//! link against (`libgyors_ipc.a`)
//!
//! Each function returns a JSON string Swift caller parses. We
//! lean on JSON instead of a typed FFI struct because auth
//! flows are infrequent (one call per sign-in) and JSON keeps the
//! `BridgingHeader.h` surface small
//!
//! Async work runs on bridge's existing tokio runtime via
//! `block_on` - these calls are user-initiated and can take a few
//! hundred ms (Argon2id on signup is the dominant cost), so we dont
//! try to be clever about non-blocking

use std::os::raw::c_char;
use std::sync::Arc;

use gyors_index::{Index, CLIPBOARD_RETENTION_DEFAULT};
use gyors_sync::{
    auth, change_password, default_session_path, signin, signup, settings::SettingsResource,
    unwrap_dek, wrap_dek, AccountTier, AesGcmCrypto, ChangePasswordRequest, ClipboardResource,
    HttpTransport, Namespace, Session, SyncEngine, SyncEngineConfig, TickReport,
};
use serde::Serialize;

use crate::BRIDGE;

// Settings resource path now lives in gyors_sync::settings::default_config_path
// (honors GYORS_CONFIG_PATH override for tests)

/// Bearer rotation window. We refresh session when
/// fewer than this many seconds remain. Seven days is a balance
/// between (a) rotating often enough that a leaked token's blast
/// radius is bounded and (b) not hammering `/v1/auth/refresh` on
/// every tick
const SESSION_REFRESH_THRESHOLD_SECS: i64 = 7 * 24 * 3600;

/// Refresh session if it's about to expire, persist the new
/// bearer + expiry back to Keychain-resident blob. Returns
/// the (possibly rotated) session
///
/// Failure modes:
/// - server returns 401: token already invalid; caller will see
///   next sync call fail and surface "please sign in again".
///   We bubble error up rather than clobber session.
/// - network error: leave session alone; next tick retries.
///
/// Crate-internal alias so `lib.rs::try_one_background_tick` can
/// call into same refresh path without the FFI module having
/// to be `pub`
pub(crate) async fn refresh_if_due_public(session: Session) -> anyhow::Result<Session> {
    refresh_if_due(session).await
}

async fn refresh_if_due(mut session: Session) -> anyhow::Result<Session> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if !session.should_refresh(now, SESSION_REFRESH_THRESHOLD_SECS) {
        return Ok(session);
    }
    tracing::info!("session near expiry; rotating bearer");
    let resp = gyors_sync::refresh_session(&session.session_token)
        .await
        .map_err(|e| anyhow::anyhow!("refresh: {e}"))?;
    session.apply_refresh(resp);
    let path = gyors_sync::default_session_path()?;
    session.save(&path)?;
    Ok(session)
}

/// Build the engine, register every resource the launcher knows
/// about, and run one tick. Single place that wires registrations
/// so FFI and the background task dont drift in what they
/// sync
pub(crate) async fn run_tick(
    index: std::sync::Arc<Index>,
    session: &Session,
) -> anyhow::Result<TickReport> {
    let key = session.decoded_encryption_key()?;
    let crypto = std::sync::Arc::new(AesGcmCrypto::new(&key));
    let transport = std::sync::Arc::new(HttpTransport::new(&session.session_token)?);
    let mut engine = SyncEngine::new(
        std::sync::Arc::clone(&index),
        crypto,
        transport,
        SyncEngineConfig {
            tier: session.tier,
        },
    );
    engine.register(Box::new(ClipboardResource::new(std::sync::Arc::clone(&index))));
    engine.register(Box::new(SettingsResource::new(
        gyors_sync::settings::default_config_path(),
    )));
    engine.tick().await
}

/// Apply user's local clipboard retention preference. This is
/// purely a local concern - sync tier doesn't factor in, because
/// local history isn't what the paid tier gates. Idempotent; safe
/// to call on init, after config saves, and on auth events
pub(crate) fn apply_local_clipboard_cap() {
    let cap = gyors_providers::config::clipboard_max_items_pref()
        .unwrap_or(CLIPBOARD_RETENTION_DEFAULT);
    if let Some(mu) = BRIDGE.get() {
        let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = bridge.index.set_clipboard_cap(cap) {
            tracing::warn!("apply_local_clipboard_cap({cap}): {e}");
        }
    }
}

/// Compatibility shim - older callers used this for tier transitions.
/// Local cap doesn't depend on tier any more; we still re-read the
/// config in case user updated it between auth events
pub(crate) fn apply_tier(_tier: Option<AccountTier>) {
    apply_local_clipboard_cap();
}

#[derive(Serialize)]
struct StatusResponse {
    signed_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    /// The `kdf_salt` is what a user needs to sign back in on a
    /// fresh device - surfacing it here lets the launcher show a
    /// "your recovery code" panel
    #[serde(skip_serializing_if = "Option::is_none")]
    kdf_salt: Option<String>,
    backend: &'static str,
    base_url: String,
    outbox_pending: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pull_cursor: Option<String>,
}

#[derive(Serialize)]
struct OkResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Returned by `signup`; the launcher must show this once and
    /// only once and tell user to back it up. Without it the
    /// user can't sign in on a new device
    #[serde(skip_serializing_if = "Option::is_none")]
    kdf_salt: Option<String>,
    /// Returned by `tick`
    #[serde(skip_serializing_if = "Option::is_none")]
    pushed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pulled: Option<usize>,
    /// True when the server returned 401 - the bearer is dead, user
    /// needs to sign in again. Swift maps this to a re-signin prompt
    /// and the background loop auto-pauses until they do
    #[serde(skip_serializing_if = "is_false")]
    session_expired: bool,
    /// True when the server returned 507 - user's storage is
    /// full. Swift surfaces a "sync paused, storage full" banner;
    /// the outbox is preserved so freeing bytes (or upgrading)
    /// resumes from where we left off
    #[serde(skip_serializing_if = "is_false")]
    quota_exceeded: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl OkResponse {
    fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            kdf_salt: None,
            pushed: None,
            pulled: None,
            session_expired: false,
            quota_exceeded: false,
        }
    }
    fn err(e: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error: Some(e.to_string()),
            kdf_salt: None,
            pushed: None,
            pulled: None,
            session_expired: false,
            quota_exceeded: false,
        }
    }
}

fn to_json<T: Serialize>(t: &T) -> *mut c_char {
    let s = serde_json::to_string(t).unwrap_or_else(|_| "{}".into());
    crate::to_raw(s)
}

/// Bounded variant of `CStr::to_str`. Auth fields stay
/// far under MAX_AUTH_FIELD_LEN in real use; the cap is a safety
/// belt against multi-MB inputs that would otherwise force the
/// runtime to alloc + UTF-8-validate before we get a chance to
/// reject. All sync FFI entries go through here
fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    crate::cstr_bounded(p, crate::MAX_AUTH_FIELD_LEN)
}

#[no_mangle]
pub extern "C" fn gyors_sync_status() -> *mut c_char {
    let path = match default_session_path() {
        Ok(p) => p,
        Err(e) => return to_json(&OkResponse::err(format!("session path: {e}"))),
    };
    let session = match Session::load(&path) {
        Ok(s) => s,
        Err(e) => return to_json(&OkResponse::err(format!("session load: {e}"))),
    };
    let (outbox_pending, pull_cursor) = match BRIDGE.get() {
        Some(mu) => {
            let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
            let pending = bridge
                .index_outbox_pending(Namespace::ClipboardHistory.as_str())
                .unwrap_or(0);
            let cursor = bridge.index_sync_pull_cursor().ok().flatten();
            (pending, cursor)
        }
        None => (0, None),
    };
    let resp = match session {
        None => StatusResponse {
            signed_in: false,
            email: None,
            tier: None,
            user_id: None,
            expires_at: None,
            kdf_salt: None,
            backend: Session::backend_label(),
            base_url: gyors_sync::base_url(),
            outbox_pending,
            pull_cursor,
        },
        Some(s) => StatusResponse {
            signed_in: true,
            email: Some(s.email),
            tier: Some(tier_str(s.tier)),
            user_id: Some(s.user_id),
            expires_at: Some(s.expires_at),
            kdf_salt: Some(s.kdf_salt),
            backend: Session::backend_label(),
            base_url: gyors_sync::base_url(),
            outbox_pending,
            pull_cursor,
        },
    };
    to_json(&resp)
}

#[no_mangle]
pub extern "C" fn gyors_sync_signup(
    email: *const c_char,
    password: *const c_char,
) -> *mut c_char {
    let Some(email) = cstr(email) else {
        return to_json(&OkResponse::err("email required"));
    };
    let Some(password) = cstr(password) else {
        return to_json(&OkResponse::err("password required"));
    };
    let kdf_salt = auth::fresh_salt();
    let encryption_salt = auth::fresh_salt();

    let result = with_runtime(async {
        // Generate the DEK locally, wrap with the
        // password-derived KEK, ship the opaque wrapped form to
        // the server. Session caches the raw DEK so push/pull
        // doesn't need the password again - rotating the password
        // later just re-wraps same DEK under a new KEK
        let dek = auth::fresh_dek();
        let kek = auth::derive_encryption_key(password, &encryption_salt)
            .map_err(|e| anyhow::anyhow!("derive KEK: {e}"))?;
        let wrapped = wrap_dek(&kek, &dek).map_err(|e| anyhow::anyhow!("wrap DEK: {e}"))?;
        let resp = signup(
            email,
            password,
            &kdf_salt,
            &encryption_salt,
            &wrapped,
            device_label(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let tier = resp.tier;
        let session = Session::from_auth(email, resp, dek.to_b64());
        let path = default_session_path()?;
        session.save(&path)?;
        Ok::<(String, AccountTier), anyhow::Error>((session.kdf_salt, tier))
    });

    match result {
        Ok((kdf_salt, tier)) => {
            apply_tier(Some(tier));
            crate::clear_bg_sync_pause();
            to_json(&OkResponse {
                ok: true,
                error: None,
                kdf_salt: Some(kdf_salt),
                pushed: None,
                pulled: None,
                session_expired: false,
                quota_exceeded: false,
            })
        }
        Err(e) => to_json(&OkResponse::err(e)),
    }
}

#[no_mangle]
pub extern "C" fn gyors_sync_signin(
    email: *const c_char,
    password: *const c_char,
    kdf_salt: *const c_char,
) -> *mut c_char {
    let Some(email) = cstr(email) else {
        return to_json(&OkResponse::err("email required"));
    };
    let Some(password) = cstr(password) else {
        return to_json(&OkResponse::err("password required"));
    };
    let Some(kdf_salt) = cstr(kdf_salt) else {
        return to_json(&OkResponse::err("kdf_salt required"));
    };

    let result = with_runtime(async {
        let resp = signin(email, password, kdf_salt, device_label())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let tier = resp.tier;
        // Derive the KEK from password + the server's
        // encryption_salt, then unwrap the DEK the server echoed
        // back. A wrong password fails here with an auth-tag error
        // - we surface that as "wrong password" rather than
        // silently caching junk bytes
        let kek = auth::derive_encryption_key(password, &resp.encryption_salt)
            .map_err(|e| anyhow::anyhow!("derive KEK: {e}"))?;
        let dek = unwrap_dek(&kek, &resp.wrapped_data_key)
            .map_err(|e| anyhow::anyhow!("unwrap DEK (wrong password?): {e}"))?;
        let session = Session::from_auth(email, resp, dek.to_b64());
        let path = default_session_path()?;
        session.save(&path)?;
        Ok::<AccountTier, anyhow::Error>(tier)
    });

    match result {
        Ok(tier) => {
            apply_tier(Some(tier));
            crate::clear_bg_sync_pause();
            to_json(&OkResponse::ok())
        }
        Err(e) => to_json(&OkResponse::err(e)),
    }
}

/// Rotate the password. Old + new come in via FFI; we
/// re-wrap cached DEK under the new KEK locally, ship the
/// bundle to `/v1/account/password`, then mirror the new salts
/// into session blob. Existing bearer + sync blobs stay
/// valid because neither session token nor the DEK rotate
#[no_mangle]
pub extern "C" fn gyors_sync_change_password(
    old_password: *const c_char,
    new_password: *const c_char,
) -> *mut c_char {
    let Some(old_password) = cstr(old_password) else {
        return to_json(&OkResponse::err("old_password required"));
    };
    let Some(new_password) = cstr(new_password) else {
        return to_json(&OkResponse::err("new_password required"));
    };
    if new_password == old_password {
        return to_json(&OkResponse::err("new password must differ from current"));
    }

    let result = with_runtime(async {
        let path = default_session_path()?;
        let mut session =
            Session::load(&path)?.ok_or_else(|| anyhow::anyhow!("not signed in"))?;
        let dek = session
            .decoded_encryption_key()
            .map_err(|e| anyhow::anyhow!("session missing DEK: {e}"))?;

        // Fresh salts on every rotation - the chain stays
        // independent of any prior leaked KEK
        let new_kdf_salt = auth::fresh_salt();
        let new_encryption_salt = auth::fresh_salt();
        let new_verifier = auth::derive_verifier(new_password, &new_kdf_salt)
            .map_err(|e| anyhow::anyhow!("derive new verifier: {e}"))?
            .to_hex();
        let new_kek = auth::derive_encryption_key(new_password, &new_encryption_salt)
            .map_err(|e| anyhow::anyhow!("derive new KEK: {e}"))?;
        let new_wrapped =
            wrap_dek(&new_kek, &dek).map_err(|e| anyhow::anyhow!("rewrap DEK: {e}"))?;
        let old_verifier = auth::derive_verifier(old_password, &session.kdf_salt)
            .map_err(|e| anyhow::anyhow!("derive old verifier: {e}"))?
            .to_hex();

        let req = ChangePasswordRequest {
            old_auth_verifier: old_verifier,
            new_auth_verifier: new_verifier,
            new_kdf_salt: new_kdf_salt.clone(),
            new_encryption_salt: new_encryption_salt.clone(),
            new_wrapped_data_key: new_wrapped,
        };
        change_password(&session.session_token, &req)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        // Server accepted - mirror the salt rotation locally. DEK
        // is unchanged so existing blobs decrypt as before
        session.kdf_salt = new_kdf_salt;
        session.encryption_salt = new_encryption_salt;
        session.save(&path)?;
        Ok::<String, anyhow::Error>(session.kdf_salt)
    });

    match result {
        Ok(new_kdf_salt) => to_json(&OkResponse {
            ok: true,
            error: None,
            kdf_salt: Some(new_kdf_salt),
            pushed: None,
            pulled: None,
            session_expired: false,
            quota_exceeded: false,
        }),
        Err(e) => to_json(&OkResponse::err(e)),
    }
}

#[no_mangle]
pub extern "C" fn gyors_sync_signout() -> *mut c_char {
    let resp = match default_session_path().and_then(|p| Session::clear(&p)) {
        Ok(()) => {
            apply_tier(None);
            // Clear the bg-sync pause flag - next signin should
            // resume the loop fresh. Loop itself no-ops with no
            // session, so leaving the flag set is harmless, but
            // clearing it on signout matches user intent ("I'm out")
            crate::clear_bg_sync_pause();
            OkResponse::ok()
        }
        Err(e) => OkResponse::err(e),
    };
    to_json(&resp)
}

/// Run one push + one pull cycle. Reads cached encryption key
/// from Keychain-resident session - user typed their
/// password at sign-in time, we never need to ask again. Phase 2
/// (token rotation) will surface "session expired - sign in" when
/// the server starts rejecting the bearer
#[no_mangle]
pub extern "C" fn gyors_sync_tick() -> *mut c_char {
    let result = with_runtime(async {
        let path = default_session_path()?;
        let session = Session::load(&path)?
            .ok_or_else(|| anyhow::anyhow!("not signed in"))?;
        // Rotate the bearer if it's near expiry before
        // committing it to the upcoming sync round-trip. Refresh
        // failures bubble up (rather than silently using the
        // soon-to-be-stale token) so launcher can surface
        // "session expired - sign in" instead of N copies of
        // "unauthorized" in the logs
        //
        // If the refresh itself returns a 401/SessionExpired (the
        // bearer is already dead), surface that via `session_expired`
        // in the OkResponse rather than as an opaque error - the
        // Swift side needs the flag to drive the re-signin prompt
        let session = match refresh_if_due(session).await {
            Ok(s) => s,
            Err(e) => {
                if let Some(t) = downcast_transport(&e) {
                    if matches!(
                        t,
                        gyors_sync::TransportError::Unauthenticated
                            | gyors_sync::TransportError::SessionExpired,
                    ) {
                        return Ok(TickReport {
                            session_expired: true,
                            ..Default::default()
                        });
                    }
                }
                return Err(e);
            }
        };
        let Some(mu) = BRIDGE.get() else {
            return Err(anyhow::anyhow!("bridge not initialised"));
        };
        let index = {
            let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(&bridge.index)
        };
        run_tick(index, &session).await
    });

    match result {
        Ok(report) => to_json(&OkResponse {
            ok: true,
            error: None,
            kdf_salt: None,
            pushed: Some(report.pushed),
            pulled: Some(report.pulled),
            session_expired: report.session_expired,
            quota_exceeded: report.quota_exceeded,
        }),
        Err(e) => to_json(&OkResponse::err(e)),
    }
}

/// Walk an anyhow error chain looking for a wrapped
/// `gyors_sync::TransportError`. Tick + refresh both `.context()`
/// their errors a few layers deep before bubbling, so a plain
/// `downcast_ref` on the top-level error misses variant we care
/// about. Returning the borrow lets caller pattern-match without
/// re-importing the transport types at call site
fn downcast_transport(err: &anyhow::Error) -> Option<&gyors_sync::TransportError> {
    err.chain()
        .find_map(|c| c.downcast_ref::<gyors_sync::TransportError>())
}

fn tier_str(t: AccountTier) -> &'static str {
    match t {
        AccountTier::Free => "free",
        AccountTier::Plus => "plus",
        AccountTier::Pro => "pro",
    }
}

fn device_label() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|h| format!("gyors-mac@{h}"))
}

/// Run an async future to completion. We spin up a dedicated
/// multi-thread runtime per call instead of reusing bridge's
/// current-thread runtime - `Handle::block_on` against a
/// current-thread runtime that no thread is actively driving will
/// hang forever (which is exactly the bug we hit on the first
/// signup attempt). Cost is ~1ms of thread allocation, paid only on
/// user-initiated sync flows, so it's a non-issue
fn with_runtime<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(fut)
}
