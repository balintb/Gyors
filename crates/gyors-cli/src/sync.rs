//! `gyors sync ...` - hand-driven sync flows for testing the
//! `gyors-cloud` integration before the launcher's sign-in UI lands
//!
//! Subcommands:
//!
//! - `signup <email>`: create an account, store session, print the
//!   kdf_salt for backup.
//! - `signin <email> <kdf_salt>`: sign back in on a fresh device.
//! - `signout`: wipe local session.
//! - `status`: show signed-in user + tier + outbox depth.
//! - `tick`: run one push + pull cycle
//!
//! Passwords are read from stdin (no `-p` flag) so they dont end up
//! in shell history. Pipe a password in for scripted use:
//!
//! ```sh
//! echo 'hunter2' | gyors sync signup me@example.com
//! ```

use anyhow::{anyhow, Context, Result};
use gyors_index::Index;
use gyors_sync::{
    auth, change_password, default_session_path, signin, signup, unwrap_dek, wrap_dek,
    AesGcmCrypto, ChangePasswordRequest, ClipboardResource, HttpTransport, Namespace, Session,
    SyncEngine, SyncEngineConfig,
};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn run(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    match sub {
        "signup" => cmd_signup(&args[1..]).await,
        "signin" => cmd_signin(&args[1..]).await,
        "signout" => cmd_signout(),
        "status" => cmd_status().await,
        "tick" => cmd_tick().await,
        "record-clipboard" => cmd_record_clipboard(&args[1..]),
        "clipboard-count" => cmd_clipboard_count(),
        "push-settings" => cmd_push_settings(),
        "settings-show" => cmd_settings_show(),
        "delete-account" => cmd_delete_account().await,
        "change-password" => cmd_change_password().await,
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("unknown sync subcommand: {other}");
            print_help();
            Ok(())
        }
    }
}

fn print_help() {
    println!(
        "gyors sync - manage E2EE sync against api.gyo.rs\n\n\
        usage:\n\
          gyors sync signup <email>             create account, save session\n\
          gyors sync signin <email> <kdf_salt>  sign in on this device\n\
          gyors sync signout                    wipe local session\n\
          gyors sync status                     show user, tier, outbox depth\n\
          gyors sync tick                       run one push + pull cycle\n\
          gyors sync record-clipboard <text>    record one item locally + outbox\n\
          gyors sync clipboard-count            print local clipboard count\n\
          gyors sync push-settings              enqueue current config.json\n\
          gyors sync settings-show              dump current config.json\n\
          gyors sync delete-account             hard-delete account on server + clear local\n\
          gyors sync change-password            rotate password (re-wraps DEK on new KEK)\n\n\
        env:\n\
          GYORS_SYNC_BASE  override API base (default https://api.gyo.rs/v1)\n\
          GYORS_DATA_DIR   override local DB / outbox location (testing)\n\
          GYORS_SYNC_SESSION_FILE  override session file (testing)"
    );
}

/// Headless equivalent of the GUI's `gyors_record_clipboard` path:
/// Inserts the content locally AND enqueues a sync outbox row.
/// Exists so e2e harness can drive the push side without
/// shipping a pasteboard watcher into the CLI binary
fn cmd_record_clipboard(args: &[String]) -> Result<()> {
    let content = args
        .first()
        .ok_or_else(|| anyhow!("usage: gyors sync record-clipboard <text>"))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let index = Index::open(index_path()?)?;
    let recorded = index.record_clipboard(content, ts)?;
    if recorded {
        gyors_sync::clipboard::enqueue_upsert(&Arc::new(index), content, ts)?;
        println!("recorded + enqueued: {} chars at ts={ts}", content.len());
    } else {
        println!("duplicate-of-previous, nothing enqueued");
    }
    Ok(())
}

fn cmd_clipboard_count() -> Result<()> {
    let index = Index::open(index_path()?)?;
    println!("{}", index.clipboard_count()?);
    Ok(())
}

/// Read the current config.json (resolved via `GYORS_CONFIG_PATH`
/// or default) and enqueue a settings outbox row. Mirrors what
/// the launcher's `save_config` hook does on the GUI side, so the
/// CLI / e2e harness can exercise the settings push path without a
/// running launcher
fn cmd_push_settings() -> Result<()> {
    let path = gyors_sync::settings::default_config_path();
    if !path.exists() {
        return Err(anyhow!(
            "no config at {} - write one before calling push-settings",
            path.display()
        ));
    }
    let index = Arc::new(Index::open(index_path()?)?);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    gyors_sync::settings::enqueue_upsert(&index, path.clone(), ts)?;
    println!("settings enqueued from {}", path.display());
    Ok(())
}

/// Dump the current on-disk config.json, resolved via same path
/// as `push-settings`. Used by the e2e harness to verify a pull
/// merged remote settings into a fresh device.
/// Hard-delete the current account on the server, then clear the
/// local session blob so next call to `status` reports
/// "not signed in." Used by the e2e harness's `afterEach` so test
/// accounts dont pile up in D1. Treats "401 unauthorized" as a
/// soft success because that's what the server returns when the
/// account has already been deleted - same outcome we want
async fn cmd_delete_account() -> Result<()> {
    let path = default_session_path()?;
    let session = match Session::load(&path) {
        Ok(Some(s)) => s,
        Ok(None) => {
            println!("not signed in - nothing to delete");
            return Ok(());
        }
        Err(e) => {
            // A corrupt session file can't sign requests but we
            // can still scrub local state
            tracing::warn!("session load: {e}; clearing local anyway");
            Session::clear(&path).ok();
            println!("local session cleared (server may still have data)");
            return Ok(());
        }
    };
    match gyors_sync::delete_account(&session.session_token).await {
        Ok(()) => {
            Session::clear(&path).context("clear local session")?;
            println!("account deleted - server returned 204, local session cleared");
        }
        Err(gyors_sync::TransportError::Unauthenticated)
        | Err(gyors_sync::TransportError::SessionExpired) => {
            Session::clear(&path).context("clear local session")?;
            println!("account already deleted or session expired - local session cleared");
        }
        Err(e) => return Err(anyhow!("delete-account: {e}")),
    }
    Ok(())
}

fn cmd_settings_show() -> Result<()> {
    let path = gyors_sync::settings::default_config_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            print!("{s}");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("{{}}");
            Ok(())
        }
        Err(e) => Err(e).context(format!("read {}", path.display())),
    }
}

async fn cmd_signup(args: &[String]) -> Result<()> {
    let email = args
        .first()
        .ok_or_else(|| anyhow!("usage: gyors sync signup <email>"))?;
    let password = read_password("password: ")?;
    let kdf_salt = auth::fresh_salt();
    let encryption_salt = auth::fresh_salt();
    let device_label = device_label();

    // Generate the DEK locally, derive the KEK from the
    // password, wrap. The server only ever sees opaque bytes
    let dek = auth::fresh_dek();
    let kek = auth::derive_encryption_key(&password, &encryption_salt)
        .map_err(|e| anyhow!("derive KEK: {e}"))?;
    let wrapped_data_key = wrap_dek(&kek, &dek).map_err(|e| anyhow!("wrap DEK: {e}"))?;

    println!("-> POST {}/auth/signup", gyors_sync::base_url());
    let resp = signup(
        email,
        &password,
        &kdf_salt,
        &encryption_salt,
        &wrapped_data_key,
        device_label,
    )
    .await
    .map_err(|e| anyhow!("signup: {e}"))?;
    // Cache the DEK (not the KEK) - that's what feeds AES-GCM on
    // every push/pull. After a password change the KEK rotates but
    // the DEK stays the same, so existing ciphertext keeps decrypting
    let session = Session::from_auth(email, resp, dek.to_b64());
    let path = default_session_path()?;
    session.save(&path).context("save session")?;

    println!("signed up - session saved to {}", path.display());
    println!("user_id   : {}", session.user_id);
    println!("tier      : {:?}", session.tier);
    println!("expires   : {}", session.expires_at);
    println!();
    println!("WRITE THIS DOWN - your kdf_salt is required to sign in on a new device:");
    println!("  {}", session.kdf_salt);
    Ok(())
}

async fn cmd_signin(args: &[String]) -> Result<()> {
    let email = args
        .first()
        .ok_or_else(|| anyhow!("usage: gyors sync signin <email> <kdf_salt>"))?;
    let kdf_salt = args
        .get(1)
        .ok_or_else(|| anyhow!("usage: gyors sync signin <email> <kdf_salt>"))?;
    let password = read_password("password: ")?;
    let device_label = device_label();

    println!("-> POST {}/auth/signin", gyors_sync::base_url());
    let resp = signin(email, &password, kdf_salt, device_label)
        .await
        .map_err(|e| anyhow!("signin: {e}"))?;
    // Re-derive KEK from password + encryption_salt, then
    // unwrap the DEK the server echoed back. Wrong password +
    // mismatch will fail at auth-tag step here rather than
    // silently producing junk bytes
    let kek = auth::derive_encryption_key(&password, &resp.encryption_salt)
        .map_err(|e| anyhow!("derive KEK: {e}"))?;
    let dek = unwrap_dek(&kek, &resp.wrapped_data_key)
        .map_err(|e| anyhow!("unwrap DEK (wrong password?): {e}"))?;
    let session = Session::from_auth(email, resp, dek.to_b64());
    let path = default_session_path()?;
    session.save(&path).context("save session")?;

    println!("signed in - session saved to {}", path.display());
    println!("user_id   : {}", session.user_id);
    println!("tier      : {:?}", session.tier);
    println!("expires   : {}", session.expires_at);
    Ok(())
}

/// Rotate the password. Reads old + new from stdin (in that
/// order), unwraps existing DEK with the OLD KEK, rewraps it
/// under the NEW KEK, ships everything to the server in one atomic
/// update. Session blob's cached DEK is unchanged - only the
/// salts get rotated locally - so existing bearer + every
/// encrypted blob keeps working
async fn cmd_change_password() -> Result<()> {
    let path = default_session_path()?;
    let mut session =
        Session::load(&path)?.ok_or_else(|| anyhow!("not signed in - run signin first"))?;
    let old_password = read_password("current password: ")?;
    let new_password = read_password("new password: ")?;
    if new_password == old_password {
        return Err(anyhow!("new password must differ from current"));
    }

    // The DEK lives in session blob from sign-in time. Wrong
    // old password is caught by the server (401 on verifier
    // mismatch); we dont try to verify locally because the
    // session doesn't keep a wrapped copy around
    let dek = session
        .decoded_encryption_key()
        .map_err(|e| anyhow!("session missing DEK: {e}"))?;

    // Derive the new auth verifier with a FRESH kdf_salt. The
    // encryption_salt also rotates - it's free and keeps the two
    // chains independent
    let new_kdf_salt = auth::fresh_salt();
    let new_encryption_salt = auth::fresh_salt();
    let new_verifier = auth::derive_verifier(&new_password, &new_kdf_salt)
        .map_err(|e| anyhow!("derive new verifier: {e}"))?
        .to_hex();
    let new_kek = auth::derive_encryption_key(&new_password, &new_encryption_salt)
        .map_err(|e| anyhow!("derive new KEK: {e}"))?;
    let new_wrapped = wrap_dek(&new_kek, &dek).map_err(|e| anyhow!("rewrap DEK: {e}"))?;

    let old_verifier = auth::derive_verifier(&old_password, &session.kdf_salt)
        .map_err(|e| anyhow!("derive old verifier: {e}"))?
        .to_hex();

    let req = ChangePasswordRequest {
        old_auth_verifier: old_verifier,
        new_auth_verifier: new_verifier,
        new_kdf_salt: new_kdf_salt.clone(),
        new_encryption_salt: new_encryption_salt.clone(),
        new_wrapped_data_key: new_wrapped,
    };
    println!("-> POST {}/account/password", gyors_sync::base_url());
    change_password(&session.session_token, &req)
        .await
        .map_err(|e| anyhow!("change-password: {e}"))?;

    // Server accepted the rotation - mirror the new salts locally
    // so a future re-sign-in derives same KEK chain. The DEK
    // (cached in `encryption_key`) stays put
    session.kdf_salt = new_kdf_salt;
    session.encryption_salt = new_encryption_salt;
    session.save(&path).context("save rotated session")?;

    println!("password rotated");
    println!();
    println!("WRITE THIS DOWN - your kdf_salt (rotated, required for sign-in on a new device):");
    println!("  {}", session.kdf_salt);
    Ok(())
}

fn cmd_signout() -> Result<()> {
    let path = default_session_path()?;
    Session::clear(&path)?;
    println!("signed out - session cleared");
    Ok(())
}

async fn cmd_status() -> Result<()> {
    let path = default_session_path()?;
    match Session::load(&path)? {
        None => {
            println!("not signed in");
            println!("session would live at: {}", path.display());
            return Ok(());
        }
        Some(s) => {
            println!("user      : {}", s.email);
            println!("user_id   : {}", s.user_id);
            println!("tier      : {:?}", s.tier);
            println!("expires   : {}", s.expires_at);
            println!("base url  : {}", gyors_sync::base_url());
            println!("session   : {}", path.display());
        }
    }

    let index = Arc::new(Index::open(index_path()?)?);
    let pending = index.outbox_pending(Namespace::ClipboardHistory.as_str())?;
    println!("outbox    : {pending} pending clipboard items");
    let cursor = index.sync_pull_cursor_get()?;
    println!(
        "pull tip  : {}",
        cursor.as_deref().unwrap_or("(never pulled)")
    );
    Ok(())
}

async fn cmd_tick() -> Result<()> {
    let path = default_session_path()?;
    let session = Session::load(&path)?
        .ok_or_else(|| anyhow!("not signed in - run `gyors sync signup` or `gyors sync signin` first"))?;
    // No password prompt - the encryption key was cached at
    // sign-in time and travels with session blob
    let key = session
        .decoded_encryption_key()
        .map_err(|e| anyhow!("session missing encryption key: {e}"))?;

    let index = Arc::new(Index::open(index_path()?)?);
    let crypto = Arc::new(AesGcmCrypto::new(&key));
    let transport = Arc::new(
        HttpTransport::new(&session.session_token).context("build http transport")?,
    );

    let mut engine = SyncEngine::new(
        Arc::clone(&index),
        crypto,
        transport,
        SyncEngineConfig {
            tier: session.tier,
        },
    );
    engine.register(Box::new(ClipboardResource::new(Arc::clone(&index))));
    engine.register(Box::new(gyors_sync::SettingsResource::new(
        gyors_sync::settings::default_config_path(),
    )));

    let report = engine.tick().await?;
    println!("pushed: {}", report.pushed);
    println!("pulled: {}", report.pulled);
    Ok(())
}

/// Read a password without echoing. If stdin isn't a TTY we read a
/// single line so user can pipe a password in for scripts. We
/// dont `rpassword` here - keeping the dep set tight is more
/// valuable than perfect terminal handling on a CLI nobody will use
/// once the launcher's sign-in UI lands
fn read_password(prompt: &str) -> Result<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        eprint!("{prompt}");
        eprint!("(echoes - install rpassword for masking) ");
        io::stderr().flush().ok();
    }
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .context("read password from stdin")?;
    let pw = line.trim().to_string();
    if pw.is_empty() {
        return Err(anyhow!("empty password"));
    }
    Ok(pw)
}

fn device_label() -> Option<String> {
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    host.map(|h| format!("gyors-cli@{h}"))
}

fn index_path() -> Result<PathBuf> {
    // Tests + e2e harness pin the on-disk location via env so we
    // dont smash the developer's real data directory
    if let Some(p) = std::env::var_os("GYORS_DATA_DIR") {
        return Ok(PathBuf::from(p).join("gyors.db"));
    }
    let base = dirs::data_dir().ok_or_else(|| anyhow!("no data dir on this platform"))?;
    Ok(base.join("Gyors").join("gyors.db"))
}
