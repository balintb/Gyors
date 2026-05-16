//! Session persistence - what the launcher needs to remember
//! between runs to avoid re-prompting for the password
//!
//! ## macOS: Keychain
//!
//! On macOS session blob lives in Keychain as a generic
//! password (`service = "com.gyors.sync"`, `account = "session"`).
//! See [`keychain`](crate::keychain). Session_token + salts
//! never touch filesystem in plaintext
//!
//! ## Other platforms: JSON file
//!
//! On non-macOS hosts (CI, future linux launcher) we fall back to
//! `<data_dir>/Gyors/sync.json` with `0o600` perms. Same data, same
//! `Session` API
//!
//! ## Override for tests
//!
//! Setting `GYORS_SYNC_SESSION_FILE=<path>` forces file backend
//! even on macOS. Two reasons it has to exist:
//!
//! 1. CI on macOS runners can't reach Keychain reliably (no UI
//!    session, prompts time out).
//! 2. Local developer tests benefit from leaving no Keychain
//!    residue between runs
//!
//! Holds session token, user's two salts, the tier, and the
//! email. Does NOT hold the password or the encryption key -
//! those are re-derived from the password every time user
//! unlocks sync

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::proto::{AccountTier, AuthResponse};

/// What we persist between runs. Everything the server already
/// knows about the account, the device's session token, AND the
/// derived encryption key so user doesn't have to retype their
/// password on every Sync Now
///
/// ## Why the key lives here
///
/// AES-256-GCM needs a 32-byte key. We derive it once at sign-in
/// via Argon2id over `(password, encryption_salt)` - 150ms of CPU.
/// Caching bytes alongside session avoids paying that cost
/// (and re-prompting user) on every tick
///
/// ## Trust model
///
/// On macOS the whole blob sits in Keychain, encrypted by the
/// user's login keychain and ACL'd to `com.gyors.sync`. An attacker
/// with running-process access as user can already exfiltrate
/// the password the moment user types it, so caching the
/// derived key isn't a meaningful downgrade vs. asking for the
/// password each time. On file backend (tests, non-macOS) the
/// blob is `0o600` JSON - same surface as session token
///
/// ## Schema
///
/// - `1`: pre-2026-05-12, no `encryption_key`. Reading these returns
///   an error so launcher prompts a re-sign-in; one-time
///   inconvenience and theres no way to re-derive without the
///   password anyway.
/// - `2`: current, includes `encryption_key`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default = "current_schema")]
    pub schema: u32,
    pub user_id: String,
    pub email: String,
    pub session_token: String,
    pub kdf_salt: String,
    pub encryption_salt: String,
    pub tier: AccountTier,
    pub expires_at: String,
    /// Base64-encoded 32-byte AES-256-GCM key, derived from
    /// `password + encryption_salt`. Optional in struct so
    /// schema-1 sessions deserialise; load-side validation rejects
    /// them before the engine sees a `None`
    #[serde(default)]
    pub encryption_key: Option<String>,
}

fn current_schema() -> u32 {
    2
}

impl Session {
    /// Build a Session from a fresh server auth response, plus the
    /// encryption key we just derived locally. The key never leaves
    /// the device; storing it here is just the "cache so we dont
    /// re-Argon2 every tick" trick
    pub fn from_auth(email: &str, resp: AuthResponse, encryption_key_b64: String) -> Self {
        Self {
            schema: current_schema(),
            user_id: resp.user_id,
            email: email.to_string(),
            session_token: resp.session_token,
            kdf_salt: resp.kdf_salt,
            encryption_salt: resp.encryption_salt,
            tier: resp.tier,
            expires_at: resp.expires_at,
            encryption_key: Some(encryption_key_b64),
        }
    }

    /// Persist using whichever backend is appropriate for host
    /// (Keychain on macOS, file elsewhere or when the override env
    /// var is set). `path` is only consulted when file backend
    /// is in use
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        if use_file_backend() {
            save_to_file(path, &json)
        } else {
            #[cfg(target_os = "macos")]
            {
                crate::keychain::write_session(&json)
            }
            #[cfg(not(target_os = "macos"))]
            {
                save_to_file(path, &json)
            }
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>> {
        let bytes = if use_file_backend() {
            load_from_file(path)?
        } else {
            #[cfg(target_os = "macos")]
            {
                crate::keychain::read_session()?
            }
            #[cfg(not(target_os = "macos"))]
            {
                load_from_file(path)?
            }
        };
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let session: Session =
            serde_json::from_slice(&bytes).context("parse session")?;
        if session.schema != current_schema() {
            return Err(anyhow!(
                "session schema {} is no longer supported (need {}); \
                 sign in again",
                session.schema,
                current_schema()
            ));
        }
        if session.encryption_key.is_none() {
            return Err(anyhow!(
                "session is missing the cached encryption key; \
                 sign in again to re-derive"
            ));
        }
        Ok(Some(session))
    }

    /// Decode cached encryption key into a `Secret`. Errors if
    /// the key is absent or malformed. Callers feed result into
    /// `AesGcmCrypto::new`
    pub fn decoded_encryption_key(&self) -> Result<crate::auth::Secret> {
        let b64 = self
            .encryption_key
            .as_deref()
            .ok_or_else(|| anyhow!("session has no encryption key cached"))?;
        crate::auth::Secret::from_b64(b64)
    }

    /// Is `expires_at` close enough that we should rotate
    /// the bearer? Default rotation window is 7 days; if the
    /// session expires sooner than that we call
    /// `/v1/auth/refresh` proactively before next sync. Returns
    /// `true` on parse failure too - we'd rather rotate spuriously
    /// than be stuck with a stale token because the timestamp
    /// format changed
    pub fn should_refresh(&self, now_unix: i64, threshold_secs: i64) -> bool {
        let Ok(expires_unix) = parse_expires_at(&self.expires_at) else {
            return true;
        };
        expires_unix - now_unix < threshold_secs
    }

    /// Update mutable fields after a successful `/v1/auth/refresh`.
    /// `kdf_salt` / `encryption_salt` / `encryption_key` stay put -
    /// refresh rotates the bearer, not the password-derived chain
    pub fn apply_refresh(&mut self, resp: crate::proto::AuthResponse) {
        self.session_token = resp.session_token;
        self.expires_at = resp.expires_at;
        self.tier = resp.tier;
    }

    pub fn clear(path: &Path) -> Result<()> {
        if use_file_backend() {
            return clear_file(path);
        }
        #[cfg(target_os = "macos")]
        {
            crate::keychain::delete_session()
        }
        #[cfg(not(target_os = "macos"))]
        {
            clear_file(path)
        }
    }

    /// Which backend a `save`/`load`/`clear` will hit. Useful for
    /// status surfaces - "session: keychain" vs "session: file"
    pub fn backend_label() -> &'static str {
        if use_file_backend() {
            "file (override)"
        } else if cfg!(target_os = "macos") {
            "keychain"
        } else {
            "file"
        }
    }
}

/// Parse an ISO-8601 `expires_at` (e.g.
/// `2026-06-11T15:21:17.000Z`) to a Unix timestamp. We dont drag
/// in `chrono` for this one use; format is server-controlled,
/// fixed, and well-defined
fn parse_expires_at(s: &str) -> Result<i64> {
    let s = s.trim_end_matches('Z');
    let (date, time) = s
        .split_once('T')
        .ok_or_else(|| anyhow!("missing T separator in expires_at: {s}"))?;
    let mut dp = date.split('-');
    let y: i64 = dp.next().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("bad year"))?;
    let m: i64 = dp.next().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("bad month"))?;
    let d: i64 = dp.next().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("bad day"))?;
    let time = time.split('.').next().unwrap_or(time);
    let mut tp = time.split(':');
    let hh: i64 = tp.next().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("bad hour"))?;
    let mm: i64 = tp.next().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("bad minute"))?;
    let ss: i64 = tp.next().and_then(|v| v.parse().ok()).ok_or_else(|| anyhow!("bad second"))?;
    Ok(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Howard Hinnant's `days_from_civil`. Civil date (year, month,
/// day) to Unix-epoch days; handles every leap-year case and
/// negative years
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Tests + non-mac hosts override Keychain via this env var so the
/// same code path exercises file backend regardless of OS
fn use_file_backend() -> bool {
    std::env::var_os("GYORS_SYNC_SESSION_FILE").is_some()
}

fn save_to_file(path: &Path, json: &[u8]) -> Result<()> {
    let target = file_path(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create session dir at {}", parent.display()))?;
    }
    let tmp = target.with_extension("json.tmp");
    std::fs::write(&tmp, json)
        .with_context(|| format!("write session tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, &target)
        .with_context(|| format!("rename session into place at {}", target.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&target)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&target, perms)?;
    }
    Ok(())
}

fn load_from_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let target = file_path(path);
    if !target.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&target)
        .with_context(|| format!("read session at {}", target.display()))?;
    Ok(Some(bytes))
}

fn clear_file(path: &Path) -> Result<()> {
    let target = file_path(path);
    match std::fs::remove_file(&target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("delete session"),
    }
}

/// If the override env var is set, that wins over caller's
/// `path` argument. Means a test can pin the location regardless of
/// what production code thinks canonical path should be
fn file_path(default: &Path) -> std::path::PathBuf {
    if let Some(override_path) = std::env::var_os("GYORS_SYNC_SESSION_FILE") {
        std::path::PathBuf::from(override_path)
    } else {
        default.to_path_buf()
    }
}

/// Default session path - `<data_dir>/Gyors/sync.json`
///
/// On macOS, `data_dir()` resolves to `~/Library/Application Support`,
/// matching rest of Gyors's storage. `dirs::data_dir()` returns
/// `None` only on exotic platforms; we error there so a misconfigured
/// run doesn't silently write into `/`
pub fn default_session_path() -> Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| anyhow!("no data dir on this platform"))?;
    Ok(base.join("Gyors").join("sync.json"))
}

#[cfg(test)]
mod tests {
    //! Tests pin file backend via `GYORS_SYNC_SESSION_FILE` so
    //! they never poke at user's real Keychain. The env var is
    //! process-global, which means we have to either set it once for
    //! the whole test binary or guard each test with a mutex; we
    //! pick (a) - simpler, and these tests dont run anywhere
    //! Keychain matters anyway
    use super::*;
    use base64::Engine as _;
    use std::sync::{Mutex, Once};
    use tempfile::tempdir;

    static FORCE_FILE: Once = Once::new();
    /// `GYORS_SYNC_SESSION_FILE` is process-global. Tests run in
    /// parallel by default, so without a serializing lock two
    /// tests' `with_tmp_path` calls will stomp each other's path
    /// between `save()` and `clear()`. Lock for the lifetime of
    /// closure
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn force_file_backend() {
        FORCE_FILE.call_once(|| {
            std::env::set_var("GYORS_SYNC_SESSION_FILE", "/tmp/__gyors_sync_session_test__");
        });
    }

    fn fixture(email: &str) -> Session {
        Session {
            schema: current_schema(),
            user_id: "u-1".into(),
            email: email.into(),
            session_token: "tok-abc".into(),
            kdf_salt: "AAAA".into(),
            encryption_salt: "BBBB".into(),
            tier: AccountTier::Plus,
            expires_at: "2099-01-01T00:00:00Z".into(),
            encryption_key: Some(base64::engine::general_purpose::STANDARD.encode([42u8; 32])),
        }
    }

    /// Per-test override that points file backend at a fresh
    /// tempfile. Holds `ENV_LOCK` for body so parallel tests
    /// dont trample each other's `GYORS_SYNC_SESSION_FILE`
    fn with_tmp_path<F: FnOnce(&Path)>(f: F) {
        force_file_backend();
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let path = dir.path().join("sync.json");
        std::env::set_var("GYORS_SYNC_SESSION_FILE", &path);
        f(&path);
    }

    #[test]
    fn roundtrip() {
        with_tmp_path(|path| {
            let s = fixture("a@b.com");
            s.save(path).unwrap();
            let loaded = Session::load(path).unwrap().unwrap();
            assert_eq!(loaded.user_id, "u-1");
            assert_eq!(loaded.tier, AccountTier::Plus);
        });
    }

    #[test]
    fn missing_file_returns_none() {
        with_tmp_path(|path| {
            assert!(Session::load(path).unwrap().is_none());
        });
    }

    #[test]
    fn clear_is_idempotent() {
        with_tmp_path(|path| {
            Session::clear(path).unwrap();
            fixture("a@b.com").save(path).unwrap();
            Session::clear(path).unwrap();
            assert!(!path.exists());
        });
    }

    #[test]
    fn save_then_clear_then_load_returns_none() {
        with_tmp_path(|path| {
            fixture("a@b.com").save(path).unwrap();
            Session::clear(path).unwrap();
            assert!(Session::load(path).unwrap().is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn permissions_locked_to_user() {
        use std::os::unix::fs::PermissionsExt;
        with_tmp_path(|path| {
            fixture("a@b.com").save(path).unwrap();
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        });
    }

    #[test]
    fn cached_encryption_key_round_trips() {
        // The whole point of schema 2: derive once at sign-in, read
        // it back without re-typing the password
        with_tmp_path(|path| {
            let s = fixture("a@b.com");
            s.save(path).unwrap();
            let loaded = Session::load(path).unwrap().unwrap();
            let key = loaded.decoded_encryption_key().unwrap();
            assert_eq!(key.as_bytes(), &[42u8; 32]);
        });
    }

    #[test]
    fn missing_encryption_key_rejected_on_load() {
        // Force-write a schema-2 file with no key. Load must refuse
        // it - we'd rather force re-sign-in than ship blobs encrypted
        // with `None`
        with_tmp_path(|path| {
            let mut s = fixture("a@b.com");
            s.encryption_key = None;
            std::fs::write(path, serde_json::to_vec(&s).unwrap()).unwrap();
            let err = Session::load(path).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("encryption key"),
                "expected encryption-key error, got: {msg}"
            );
        });
    }

    #[test]
    fn schema_one_files_rejected() {
        // Pre-Phase-2 sessions had no cached key. Force-write a
        // schema-1 blob and confirm `load` refuses it so the
        // launcher's "not signed in" path kicks in
        with_tmp_path(|path| {
            let mut s = fixture("a@b.com");
            s.schema = 1;
            s.encryption_key = None;
            std::fs::write(path, serde_json::to_vec(&s).unwrap()).unwrap();
            let err = Session::load(path).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("no longer supported") || msg.contains("schema"),
                "expected schema-mismatch error, got: {msg}"
            );
        });
    }

    #[test]
    fn corrupted_encryption_key_rejected() {
        // A short / non-base64 key should fail at decode time, not
        // panic. Mirrors what an attacker (or bit-rot) might
        // produce
        with_tmp_path(|path| {
            let mut s = fixture("a@b.com");
            s.encryption_key = Some("notbase64!".into());
            std::fs::write(path, serde_json::to_vec(&s).unwrap()).unwrap();
            let loaded = Session::load(path).unwrap().unwrap();
            assert!(loaded.decoded_encryption_key().is_err());
        });
    }

    // Regressions: rotation thresholds + apply_refresh

    #[test]
    fn should_refresh_when_within_threshold() {
        // Now = midnight on 2026-05-13. Session expires the same
        // day at 12:00 - 12 hours away. With a 7-day threshold
        // we should refresh
        let s = Session {
            schema: current_schema(),
            user_id: "u".into(),
            email: "e".into(),
            session_token: "t".into(),
            kdf_salt: "k".into(),
            encryption_salt: "e".into(),
            tier: AccountTier::Plus,
            expires_at: "2026-05-13T12:00:00.000Z".into(),
            encryption_key: Some("AAAA".into()),
        };
        let now = days_from_civil(2026, 5, 13) * 86_400; // midnight
        assert!(s.should_refresh(now, 7 * 86_400));
    }

    #[test]
    fn should_not_refresh_when_far_from_expiry() {
        let s = Session {
            schema: current_schema(),
            user_id: "u".into(),
            email: "e".into(),
            session_token: "t".into(),
            kdf_salt: "k".into(),
            encryption_salt: "e".into(),
            tier: AccountTier::Plus,
            // 60 days out
            expires_at: "2026-07-13T12:00:00.000Z".into(),
            encryption_key: Some("AAAA".into()),
        };
        let now = days_from_civil(2026, 5, 13) * 86_400;
        assert!(!s.should_refresh(now, 7 * 86_400));
    }

    #[test]
    fn should_refresh_when_expires_at_is_unparseable() {
        // If the server ever changes format and
        // we can't parse it, default to rotating rather than
        // staying stuck with a stale token
        let s = Session {
            schema: current_schema(),
            user_id: "u".into(),
            email: "e".into(),
            session_token: "t".into(),
            kdf_salt: "k".into(),
            encryption_salt: "e".into(),
            tier: AccountTier::Plus,
            expires_at: "totally-not-a-timestamp".into(),
            encryption_key: Some("AAAA".into()),
        };
        assert!(s.should_refresh(0, 7 * 86_400));
    }

    #[test]
    fn apply_refresh_rotates_bearer_and_expiry_only() {
        let mut s = Session {
            schema: current_schema(),
            user_id: "u".into(),
            email: "alice@gyo.rs".into(),
            session_token: "OLD-TOKEN".into(),
            kdf_salt: "salt-k".into(),
            encryption_salt: "salt-e".into(),
            tier: AccountTier::Plus,
            expires_at: "2026-06-11T15:21:17.000Z".into(),
            encryption_key: Some("CACHED-KEY".into()),
        };
        let resp = AuthResponse {
            user_id: "u".into(),
            session_token: "NEW-TOKEN".into(),
            kdf_salt: "should-be-ignored".into(),
            encryption_salt: "should-be-ignored".into(),
            wrapped_data_key: "should-be-ignored".into(),
            tier: AccountTier::Plus,
            expires_at: "2026-07-11T15:21:17.000Z".into(),
        };
        s.apply_refresh(resp);
        assert_eq!(s.session_token, "NEW-TOKEN");
        assert_eq!(s.expires_at, "2026-07-11T15:21:17.000Z");
        // The password-derived chain MUST NOT be touched -
        // refresh only rotates the bearer
        assert_eq!(s.kdf_salt, "salt-k");
        assert_eq!(s.encryption_salt, "salt-e");
        assert_eq!(s.encryption_key.as_deref(), Some("CACHED-KEY"));
    }

    #[test]
    fn parse_expires_at_handles_fractional_and_no_fractional_seconds() {
        // Server sometimes emits `.000Z`, sometimes not. Both
        // shapes must parse to same epoch second
        let a = parse_expires_at("2026-06-11T15:21:17.000Z").unwrap();
        let b = parse_expires_at("2026-06-11T15:21:17Z").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn from_auth_threads_the_key_through() {
        // Smoke-test the helper used by every signin/signup path
        let resp = AuthResponse {
            user_id: "u-7".into(),
            session_token: "tok".into(),
            kdf_salt: "AAAA".into(),
            encryption_salt: "BBBB".into(),
            wrapped_data_key: "CCCC".into(),
            tier: AccountTier::Plus,
            expires_at: "x".into(),
        };
        let key_b64 = base64::engine::general_purpose::STANDARD.encode([1u8; 32]);
        let s = Session::from_auth("u@v.com", resp, key_b64.clone());
        assert_eq!(s.encryption_key.as_deref(), Some(key_b64.as_str()));
        assert_eq!(s.schema, current_schema());
    }
}
