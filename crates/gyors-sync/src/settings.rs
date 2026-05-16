//! [`SyncResource`] for the `settings` namespace
//!
//! Unlike clipboard history (one row per copy), settings are a
//! singleton blob: one JSON object covering user's launcher
//! prefs. We sync it under a fixed `resource_id = "root"`,
//! so multiple devices converge on whichever wrote last (LWW per
//! `Namespace::Settings.conflict_policy()`)
//!
//! ## Per-device denylist
//!
//! Not every config key should travel. Secrets stay on the device
//! that knows them, and machine-pinned values like Notes folder
//! paths dont make sense across Macs
//!
//! - `ai.api_key`, `jwt.secret` - credentials. Live in the local
//!   config (or, later, Keychain); never on the sync server.
//! - `notes_folder`, `terminal_app` - per-machine references
//!   (filesystem path / installed-app name).
//! - `sync_interval_secs` - preference for THIS device's
//!   background cadence; another device's pref shouldn't override
//!
//! Adding a key here is intentional. Default is "sync it",
//! since most settings are user-facing prefs user wants
//! everywhere (theme, hotkey, AI provider, snippets behavior)
//!
//! ## Merge semantics
//!
//! On pull, we dont replace whole config blob. We merge the
//! incoming payload into the local config, leaving denylisted
//! keys untouched. That way:
//!
//! - User signs in on Mac B -> Mac B's `ai.api_key` stays Mac B's,
//!   even though Mac A's bundle didn't include any `ai.api_key`.
//! - User changes theme on Mac A -> Mac B picks up the new theme
//!   without losing Mac B-specific paths

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::proto::Namespace;
use crate::resource::{LocalChange, RemoteChange, SyncOp, SyncResource};

/// Fixed `resource_id` for the singleton settings blob. Constant
/// so signups / signins all converge on same row
pub const SETTINGS_ID: &str = "root";

/// Resolve the on-disk path for `config.json`. Honors a
/// `GYORS_CONFIG_PATH` env override for tests + the e2e harness so
/// per-device isolation doesn't smash the real config; otherwise
/// falls back to `<config_dir>/Gyors/config.json` (matching the
/// launcher's production layout)
pub fn default_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("GYORS_CONFIG_PATH") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Gyors")
        .join("config.json")
}

/// Keys (or key prefixes ending in `.`) that must NEVER leave the
/// device. See module docs for the rationale
pub const LOCAL_ONLY_KEYS: &[&str] = &[
    "ai.api_key",
    "jwt.secret",
    "notes_folder",
    "terminal_app",
    "sync_interval_secs",
];

pub struct SettingsResource {
    /// Where to read/write the on-disk config.json
    config_path: PathBuf,
}

impl SettingsResource {
    pub fn new(config_path: PathBuf) -> Self {
        Self { config_path }
    }

    /// Build a `LocalChange` from the current on-disk config. The
    /// payload is the syncable subset, JSON-encoded
    pub fn change_for_current(&self, ts: i64) -> Result<LocalChange> {
        let root = load_config_or_empty(&self.config_path)?;
        change_for_root(&root, ts)
    }
}

/// Build a `LocalChange` from a config Value caller already has
/// in hand. Used by the `save_config` hook so we dont re-read the
/// file we just wrote
pub fn change_for_root(root: &Value, ts: i64) -> Result<LocalChange> {
    let syncable = syncable_subset(root);
    let payload = serde_json::to_vec(&syncable).context("serialize settings payload")?;
    Ok(LocalChange {
        id: SETTINGS_ID.to_string(),
        op: SyncOp::Upsert,
        payload,
        ts_local: ts,
    })
}

impl SyncResource for SettingsResource {
    fn namespace(&self) -> Namespace {
        Namespace::Settings
    }

    fn apply(&self, change: &RemoteChange) -> Result<()> {
        if matches!(change.op, SyncOp::Delete) {
            // Deleting the settings blob would wipe user's
            // prefs, which we never want from a pull-driven flow.
            // No-op; LWW handles the "I want different prefs" case
            // via a normal upsert
            return Ok(());
        }
        let incoming: Value =
            serde_json::from_slice(&change.payload).context("decode settings payload")?;
        let local = load_config_or_empty(&self.config_path)?;
        let merged = merge_preserving_local_only(local, incoming);
        write_config(&self.config_path, &merged)
            .context("write merged settings back to disk")?;
        Ok(())
    }

    fn validate_payload(&self, payload: &[u8]) -> Result<Vec<u8>> {
        // Round-trip through Value so a malformed blob fails loudly
        // before encryption
        let parsed: Value = serde_json::from_slice(payload)?;
        Ok(serde_json::to_vec(&parsed)?)
    }
}

/// Project `root` down to the keys we're willing to send to the
/// server. Walks `(top_level, dotted_child)` so e.g. `ai.api_key` is
/// stripped from a `ai: { api_key, provider }` object
pub fn syncable_subset(root: &Value) -> Value {
    let Value::Object(obj) = root else {
        // Non-object root is unusable as config anyway; ship as-is
        // (the engine will reject on the server side if it's not a
        // sensible JSON document)
        return root.clone();
    };
    let mut out = Map::new();
    for (k, v) in obj {
        if is_local_only(k) {
            continue;
        }
        // Recurse into nested objects so `ai.api_key` inside the
        // `ai` block is removed while `ai.provider` stays
        match v {
            Value::Object(_) => {
                let sub = syncable_subobject(k, v);
                if !sub.as_object().is_some_and(|m| m.is_empty()) {
                    out.insert(k.clone(), sub);
                }
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

fn syncable_subobject(prefix: &str, v: &Value) -> Value {
    let Value::Object(obj) = v else {
        return v.clone();
    };
    let mut out = Map::new();
    for (k, child) in obj {
        let dotted = format!("{prefix}.{k}");
        if is_local_only(&dotted) {
            continue;
        }
        out.insert(k.clone(), child.clone());
    }
    Value::Object(out)
}

fn is_local_only(key: &str) -> bool {
    LOCAL_ONLY_KEYS.iter().any(|d| d == &key)
}

/// Apply `incoming` over `local`, preserving local-only keys. Top-
/// level scalar keys and missing-on-remote keys are taken from
/// whichever side has them; for nested objects, recurse so an
/// incoming `ai.provider` doesn't wipe a locally-set `ai.api_key`
pub fn merge_preserving_local_only(local: Value, incoming: Value) -> Value {
    let (local_obj, incoming_obj) = match (local, incoming) {
        (Value::Object(l), Value::Object(i)) => (l, i),
        (_, fallback) => return fallback,
    };
    let mut out = local_obj;
    let incoming = incoming_obj;
    for (k, v) in incoming {
        if is_local_only(&k) {
            continue;
        }
        match (out.get_mut(&k), v) {
            (Some(existing @ Value::Object(_)), Value::Object(child_incoming)) => {
                let existing_obj = std::mem::replace(existing, Value::Null);
                *existing = merge_subobject(&k, existing_obj, child_incoming);
            }
            (_, v) => {
                out.insert(k, v);
            }
        }
    }
    Value::Object(out)
}

fn merge_subobject(prefix: &str, local: Value, incoming: Map<String, Value>) -> Value {
    let Value::Object(mut local_obj) = local else {
        return Value::Object(incoming);
    };
    for (k, v) in incoming {
        let dotted = format!("{prefix}.{k}");
        if is_local_only(&dotted) {
            continue;
        }
        local_obj.insert(k, v);
    }
    Value::Object(local_obj)
}

fn load_config_or_empty(path: &PathBuf) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Value::Object(Map::new())),
        Ok(s) => serde_json::from_str(&s).context("parse config.json"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(Map::new()))
        }
        Err(e) => Err(e).context(format!("read {}", path.display())),
    }
}

fn write_config(path: &PathBuf, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let text = serde_json::to_string_pretty(root).context("serialize config")?;
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Convenience: enqueue a settings upsert from an in-memory Value.
/// The launcher's `save_config` hook calls this so a user-driven
/// config change ships without waiting for next tick (and
/// without an extra disk round-trip)
pub fn enqueue_upsert_root(
    index: &Arc<gyors_index::Index>,
    root: &Value,
    ts: i64,
) -> Result<()> {
    let change = change_for_root(root, ts)?;
    index
        .outbox_enqueue(
            Namespace::Settings.as_str(),
            &change.id,
            change.op.as_str(),
            &change.payload,
            change.ts_local,
        )
        .context("enqueue settings upsert")?;
    Ok(())
}

/// File-backed variant: reads config.json from `config_path` and
/// enqueues. For periodic-sync code paths that dont have the Value
/// handy
pub fn enqueue_upsert(
    index: &Arc<gyors_index::Index>,
    config_path: PathBuf,
    ts: i64,
) -> Result<()> {
    let resource = SettingsResource::new(config_path);
    let change = resource.change_for_current(ts)?;
    index
        .outbox_enqueue(
            Namespace::Settings.as_str(),
            &change.id,
            change.op.as_str(),
            &change.payload,
            change.ts_local,
        )
        .context("enqueue settings upsert")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn syncable_subset_keeps_top_level_user_prefs() {
        let root = json!({
            "theme": "midnight",
            "hotkey": "opt+space",
            "ai": { "provider": "openai" }
        });
        let s = syncable_subset(&root);
        assert_eq!(s["theme"], "midnight");
        assert_eq!(s["hotkey"], "opt+space");
        assert_eq!(s["ai"]["provider"], "openai");
    }

    #[test]
    fn syncable_subset_strips_secrets_and_machine_specific() {
        let root = json!({
            "theme": "midnight",
            "notes_folder": "/Users/alice/Documents/Gyors",
            "terminal_app": "iTerm",
            "sync_interval_secs": "60",
            "jwt": { "secret": "shhh" },
            "ai": { "provider": "openai", "api_key": "sk-xxx" }
        });
        let s = syncable_subset(&root);
        assert!(s.get("notes_folder").is_none(),
            "notes_folder is per-machine, must not leave the device");
        assert!(s.get("terminal_app").is_none(),
            "terminal_app is per-machine");
        assert!(s.get("sync_interval_secs").is_none(),
            "sync interval is a per-device pref");
        assert!(s["jwt"].get("secret").is_none(),
            "jwt.secret is a credential");
        assert!(s["ai"].get("api_key").is_none(),
            "ai.api_key is a credential");
        // Non-secret AI fields still ship
        assert_eq!(s["ai"]["provider"], "openai");
    }

    #[test]
    fn syncable_subset_drops_ai_block_if_empty_after_stripping() {
        // Only api_key in ai block -> ai becomes empty -> we drop
        // the whole key rather than ship `{ "ai": {} }`
        let root = json!({ "ai": { "api_key": "sk-xxx" } });
        let s = syncable_subset(&root);
        assert!(s.get("ai").is_none() || s["ai"].as_object().unwrap().is_empty());
    }

    #[test]
    fn merge_keeps_local_only_keys_from_local() {
        let local = json!({
            "theme": "sunset",
            "notes_folder": "/Users/alice/Notes",
            "ai": { "provider": "ollama", "api_key": "sk-local" }
        });
        let incoming = json!({
            "theme": "midnight",
            "ai": { "provider": "openai" }
        });
        let merged = merge_preserving_local_only(local, incoming);
        assert_eq!(merged["theme"], "midnight", "remote theme wins");
        assert_eq!(merged["notes_folder"], "/Users/alice/Notes",
            "local notes_folder survives — wasn't in payload");
        assert_eq!(merged["ai"]["provider"], "openai",
            "remote AI provider wins");
        assert_eq!(merged["ai"]["api_key"], "sk-local",
            "local AI api_key survives a remote upsert");
    }

    #[test]
    fn merge_rejects_local_only_keys_even_when_remote_includes_them() {
        // A buggy or malicious device could push `ai.api_key` over the
        // wire. Our merge layer is the last line of defense
        let local = json!({ "ai": { "api_key": "sk-original" } });
        let incoming = json!({ "ai": { "api_key": "sk-injected" } });
        let merged = merge_preserving_local_only(local, incoming);
        assert_eq!(merged["ai"]["api_key"], "sk-original",
            "remote payload must not be able to overwrite local secrets");
    }

    #[test]
    fn merge_with_no_local_uses_incoming_subject_to_denylist() {
        let local = json!({});
        let incoming = json!({
            "theme": "neon",
            "notes_folder": "/Users/somebody-else/Notes"
        });
        let merged = merge_preserving_local_only(local, incoming);
        assert_eq!(merged["theme"], "neon");
        assert!(
            merged.get("notes_folder").is_none(),
            "denylist applies on incoming too — fresh device shouldn't inherit \
             notes_folder from another Mac"
        );
    }

    #[test]
    fn apply_writes_merged_blob_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            json!({ "ai": { "api_key": "sk-local" }, "theme": "sunset" }).to_string(),
        )
        .unwrap();

        let resource = SettingsResource::new(path.clone());
        let payload =
            serde_json::to_vec(&json!({ "theme": "midnight", "hotkey": "cmd+space" }))
                .unwrap();
        resource
            .apply(&RemoteChange {
                id: SETTINGS_ID.into(),
                op: SyncOp::Upsert,
                version: 1,
                updated_at: "x".into(),
                payload,
            })
            .unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["theme"], "midnight");
        assert_eq!(written["hotkey"], "cmd+space");
        assert_eq!(written["ai"]["api_key"], "sk-local",
            "local secret survived an incoming settings sync");
    }

    #[test]
    fn apply_delete_is_a_noop() {
        // We never want a pull-driven flow to wipe user's prefs
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, json!({ "theme": "sunset" }).to_string()).unwrap();

        let resource = SettingsResource::new(path.clone());
        resource
            .apply(&RemoteChange {
                id: SETTINGS_ID.into(),
                op: SyncOp::Delete,
                version: 1,
                updated_at: "x".into(),
                payload: vec![],
            })
            .unwrap();

        let still: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(still["theme"], "sunset", "delete must NOT wipe config");
    }

    #[test]
    fn change_for_current_emits_an_outbox_upsert_with_fixed_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, json!({ "theme": "neon" }).to_string()).unwrap();
        let r = SettingsResource::new(path);
        let c = r.change_for_current(42).unwrap();
        assert_eq!(c.id, SETTINGS_ID, "settings is a singleton — fixed id");
        assert_eq!(c.op, SyncOp::Upsert);
        let parsed: Value = serde_json::from_slice(&c.payload).unwrap();
        assert_eq!(parsed["theme"], "neon");
    }
}
