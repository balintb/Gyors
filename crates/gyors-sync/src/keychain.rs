//! MacOS Keychain glue. Stores JSON-encoded [`Session`](crate::Session)
//! as a generic password item, so a `security` CLI dump (or another
//! tool with the right ACL) can inspect it but a casual filesystem
//! scan can't
//!
//! ## Why a single item, not field-by-field
//!
//! Fields we persist are tightly coupled - `(session_token,
//! kdf_salt, encryption_salt, tier, expires_at, user_id, email)`
//! travel as one unit. Splitting them would mean N times the
//! Keychain prompts on first access and N opportunities for a
//! partial-write mismatch on update. One item, atomic update
//!
//! ## Why not Keychain-on-everything
//!
//! Non-macOS builds (CI, future linux launcher) fall back to the
//! plain-file path in [`session`](crate::session). The fallback is
//! gated at a higher level (Session::save/load); this module only
//! exists on macOS

#![cfg(target_os = "macos")]

use anyhow::{anyhow, Context, Result};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

/// Service identifier - appears in Keychain Access as the "Where"
/// column. Matches the bundle id we expect to ship under, so
/// migrating from a dev build to a signed release doesn't strand
/// items under a different service name
const SERVICE: &str = "com.gyors.sync";
/// Account name. Single account per service today; if we ever
/// support multiple cloud accounts we'd key this by user_id
const ACCOUNT: &str = "session";

/// Warn once per process if we're an unsigned binary
///
/// macOS infers a Keychain ACL from calling binary's code
/// signature. For a signed/notarised production build that means
/// "only same signing identity can read this item." For an
/// unsigned dev build - which is what `scripts/build-app.sh`
/// produces, what new contributors run, what the CLI ships as
/// today - the inferred ACL is much wider: any unsigned process
/// running as user can read the item
///
/// Once a real Developer ID is wired into the build, we should
/// also set an explicit `kSecAttrAccessGroup` so even other
/// signed binaries from same team can't access the item
/// without consent. Until then, log a one-time warning at the
/// first Keychain operation so developers see it and dont
/// accidentally treat the dev build as production-secure
///
/// `OnceLock` guarantees the warning fires exactly once per
/// process lifetime regardless of how often Keychain ops happen
fn warn_if_unsigned_once() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if WARNED.set(()).is_err() {
        return;
    }
    if is_unsigned_binary() {
        tracing::warn!(
            "keychain items are stored with a permissive ACL because \
             this binary is unsigned. Any unsigned process running as you can \
             read them. Sign + notarise before production deploys; see \
             SECURITY.md."
        );
        // Tracing's release_max_level_warn means this is the
        // highest level a release build emits, so it always
        // reaches the logs
    }
}

/// Best-effort check: are we running an ad-hoc-signed or unsigned
/// binary? We invoke the system `codesign` CLI on our own
/// executable path because security_framework doesn't expose this
/// query without a lot of CFRuntime ceremony. A `codesign` failure
/// counts as "unsigned" - the failure modes overlap with what
/// we're worried about (no signing identity, invalid signature,
/// detached signature missing)
fn is_unsigned_binary() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return true,
    };
    let out = std::process::Command::new("/usr/bin/codesign")
        .arg("--display")
        .arg("--verbose=2")
        .arg(&exe)
        .output();
    let Ok(out) = out else {
        return true;
    };
    if !out.status.success() {
        return true;
    }
    // `codesign --display` prints `Authority=...` lines for
    // signed binaries. Ad-hoc-signed has `Signature=
    // adhoc`. Unsigned has no signature header at all (status
    // would be non-zero). We treat ad-hoc + no-authority as
    // unsigned for this warning's purposes
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("Signature=adhoc") {
        return true;
    }
    if !stderr.contains("Authority=") {
        return true;
    }
    false
}

/// Write encoded session into Keychain. Atomic from the
/// caller's perspective - Keychain replaces the whole item
pub fn write_session(payload: &[u8]) -> Result<()> {
    warn_if_unsigned_once();
    set_generic_password(SERVICE, ACCOUNT, payload)
        .context("keychain: set generic password")
}

/// Returns the raw bytes if the entry exists, `None` if it doesn't.
/// Other errors (e.g. user denied access) bubble up so caller
/// can show a useful message instead of treating "denied" as
/// "missing" and prompting user to sign in again
pub fn read_session() -> Result<Option<Vec<u8>>> {
    warn_if_unsigned_once();
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) => {
            // `security_framework::base::Error` exposes a numeric
            // code via `.code()`; -25300 is `errSecItemNotFound`.
            // Treat that as `None`; surface anything else
            if e.code() == -25300 {
                Ok(None)
            } else {
                Err(anyhow!("keychain: get generic password: {e}"))
            }
        }
    }
}

/// Idempotent delete - missing entry is success
pub fn delete_session() -> Result<()> {
    match delete_generic_password(SERVICE, ACCOUNT) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == -25300 => Ok(()),
        Err(e) => Err(anyhow!("keychain: delete generic password: {e}")),
    }
}
