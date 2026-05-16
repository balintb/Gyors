//! In-launcher config browser / editor
//!
//! Keyword: `config`
//!
//! Three views:
//!
//! - `config` / `config <filter>` - list known fields with their
//!   current values and a "Open config.json" shortcut.
//! - `config set <key> <value>` - confirm candidate that writes the
//!   value to disk. Unknown keys are still accepted (plugins,
//!   experimental flags) but flagged in the subtitle.
//! - `config reset <key>` - confirm candidate that removes the key,
//!   letting the built-in default take over. Never *writes* the
//!   default - so fresh defaults in new versions flow through
//!
//! Schema below names fields the launcher knows about.
//! Adding a new one is purely declarative - extend `FIELDS` and the
//! list / autocomplete picks it up automatically

use crate::registry::{is_always_on, new_disabled_set, DisabledProviders};
use anyhow::{Context, Result};
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

pub struct ConfigProvider {
    /// Shared disabled-providers set. Updated in-place when user
    /// toggles a provider via this UI so registry sees the change
    /// immediately - no restart required
    gate: DisabledProviders,
    /// Ids of every provider the live registry knows about. Seeded at
    /// construction so config list can surface one toggle row per
    /// provider without hard-coding the set
    provider_ids: Vec<String>,
}

impl Default for ConfigProvider {
    fn default() -> Self {
        Self::new(new_disabled_set(), Vec::new())
    }
}

impl ConfigProvider {
    pub fn new(gate: DisabledProviders, provider_ids: Vec<String>) -> Self {
        let mut provider_ids = provider_ids;
        // Strip core providers that aren't toggleable - showing them
        // with no-op actions would be confusing
        provider_ids.retain(|id| !is_always_on(id));
        provider_ids.sort();
        provider_ids.dedup();
        Self { gate, provider_ids }
    }
}

const RESULT_LIMIT: usize = 30;

#[derive(Debug, Clone, Copy)]
pub enum FieldType {
    /// Free-form string (hotkey combo, model name, ...)
    Text,
    /// Boolean. Accepts `true/false/1/0/on/off` on input side
    Bool,
    /// Filesystem path (tilde-expanded on read)
    Path,
    /// One of a closed set of values. Typo-friendly - the check is
    /// applied at write time so users get an informative error
    Enum(&'static [&'static str]),
    /// Bundle name of an installed `.app`. The set-preview surface
    /// fuzzy-searches `/Applications`, `~/Applications`, and the
    /// system app folders so user can type "iterm" and pick
    /// iTerm without knowing the exact bundle name. Stored value is
    /// the bundle name (what `open -a <name>` accepts), not the
    /// full path - keeps configs portable across machines
    App,
}

pub struct Field {
    /// Dotted path into JSON object. `ai.model` sets
    /// `root["ai"]["model"]`
    pub key: &'static str,
    pub description: &'static str,
    /// What the app falls back to when the key isn't present. Shown
    /// in the subtitle and used by `reset` messages - never written
    /// verbatim
    pub default: &'static str,
    pub ty: FieldType,
}

/// Known keys. Ordered by "how likely a user is to edit this" -
/// hotkey and theme first, advanced keys last
pub const FIELDS: &[Field] = &[
    Field { key: "theme", description: "UI theme", default: "system",
        ty: FieldType::Enum(&[
            "system", "midnight", "sunset", "forest", "monochrome",
            "neon", "nord", "dracula", "solarized-dark", "tokyo-night",
        ]) },
    Field { key: "panel_opacity",
        description: "Panel background opacity (0.0–1.0). Empty = use the theme's own default. Affects only the background; text stays fully opaque.",
        default: "", ty: FieldType::Text },
    Field { key: "panel_blur",
        description: "Background blur (true / false). Empty = use the theme's own default. Spotlight-style desktop blur behind the panel.",
        default: "", ty: FieldType::Text },
    Field { key: "hotkey", description: "Global hotkey combo",
        default: "opt+shift+space", ty: FieldType::Text },
    Field { key: "notes_folder", description: "Where notes are stored",
        default: "~/Documents/Gyors", ty: FieldType::Path },
    Field { key: "repo_roots",
        description: "Where the `repo` keyword looks for git repos. Comma-separated. Each entry must be an absolute path (/…) or a tilde path (~/…); bare names are ignored. Scanned to depth 4 at startup. Default: ~/Documents, ~/Projects, ~/dev.",
        default: "~/Documents, ~/Projects, ~/dev",
        ty: FieldType::Text },
    Field { key: "clipboard_enabled", description: "Track clipboard history",
        default: "true", ty: FieldType::Bool },
    Field { key: "clipboard_max_items",
        description: "How many clipboard entries to keep locally. Independent of cloud sync - local history is always full-fat. Default 500.",
        default: "500", ty: FieldType::Text },
    Field { key: "sync_interval_secs",
        description: "Background sync cadence in seconds (10-3600). Lower = items appear on other devices sooner; higher = less network / battery. Per-device preference; not synced. Default 60.",
        default: "60", ty: FieldType::Text },
    Field { key: "preview_markdown", description: "Render markdown in inline previews (AI answers, conversion output)",
        default: "true", ty: FieldType::Bool },
    Field { key: "preview_syntax", description: "Syntax-highlight code in inline previews (json / yaml / toml / md)",
        default: "true", ty: FieldType::Bool },
    Field { key: "ai.provider", description: "AI backend (apple = on-device Foundation Models, requires Apple Intelligence)",
        default: "ollama",
        ty: FieldType::Enum(&["ollama", "openai", "anthropic", "apple"]) },
    Field { key: "ai.model", description: "AI model id",
        default: "llama3", ty: FieldType::Text },
    Field { key: "ai.endpoint", description: "AI provider endpoint URL",
        default: "", ty: FieldType::Text },
    Field { key: "ai.api_key", description: "AI provider API key",
        default: "", ty: FieldType::Text },
    Field { key: "jwt.secret", description: "HS256 signing secret used by `newjwt`",
        default: "", ty: FieldType::Text },
    Field { key: "snippets.expand_globally",
        description: "System-wide snippet expansion (;trigger; inserts snippet text anywhere). Requires Accessibility permission.",
        default: "false", ty: FieldType::Bool },
    Field { key: "ai.router_enabled",
        description: "AI command router - translates natural-language queries (e.g. `100 dollars in euros`) into provider keyword form when no keyword matches. Off by default.",
        default: "false", ty: FieldType::Bool },
    Field { key: "terminal_app",
        description: "Which Terminal-style app to launch ssh / shell rows in. Type to search installed apps; pick one to save its bundle name.",
        default: "Terminal", ty: FieldType::App },
];

#[async_trait]
impl Provider for ConfigProvider {
    fn id(&self) -> &str {
        "config"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(rest) = strip_keyword(pattern) else { return vec![]; };
        let rest = rest.trim();

        // Sub-commands first
        if let Some(cmd) = rest.strip_prefix("set ") {
            return set_preview_candidates(cmd.trim(), &self.provider_ids);
        }
        if let Some(cmd) = rest.strip_prefix("reset ") {
            return reset_preview_candidates(cmd.trim());
        }
        if rest == "open" {
            return vec![open_config_candidate()];
        }

        // Default: a filterable list of known fields + a toggle row per
        // registered provider + the "Open file" row
        self.list_candidates(rest)
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        if id == "config::open" {
            return Ok(Effect::OpenPath(config_path()));
        }
        if let Some(key) = id.strip_prefix("config::field::") {
            return Ok(handle_field_action(key, action));
        }
        if let Some(key) = id.strip_prefix("config::browse::") {
            // Hand the key to Swift shell, which pops NSOpenPanel
            // and writes result. Keeping Rust out of the GUI
            // opening path means we can test this provider without a
            // display
            let description = FIELDS
                .iter()
                .find(|f| f.key == key)
                .map(|f| f.description.to_string())
                .unwrap_or_else(|| format!("Pick a folder for {key}"));
            return Ok(Effect::PickDirectory {
                config_key: key.to_string(),
                prompt: description,
            });
        }
        if let Some(tail) = id.strip_prefix("config::set::") {
            let (key, value) = split_once_on_double_colon(tail)
                .ok_or_else(|| anyhow::anyhow!("malformed set id: {id}"))?;
            apply_set(key, value)?;
            // Provider toggle? Sync the in-memory gate so registry
            // picks up the change without waiting for a restart
            if let Some(provider_id) = provider_enabled_key(key) {
                let enabled = value_to_bool(value);
                self.apply_gate_update(&provider_id, enabled);
            }
            // Live-apply hook: a few keys can take effect without a
            // restart, but only if we tell Swift shell to do so.
            // For everything else the on-disk write is the whole
            // story and user sees a confirmation toast
            if key == "theme" {
                return Ok(Effect::ApplyTheme(value.to_string()));
            }
            return Ok(Effect::Notification {
                title: format!("Set {key}"),
                body: Some(format!("= {value}")),
            });
        }
        if let Some(key) = id.strip_prefix("config::reset::") {
            apply_reset(key)?;
            // Reset on a provider toggle -> re-enable it (gate default
            // is "all on"; removing the key returns to that default)
            if let Some(provider_id) = provider_enabled_key(key) {
                self.apply_gate_update(&provider_id, true);
            }
            // Theme reset -> fall back to the built-in default at
            // runtime so user doesn't have to relaunch
            if key == "theme" {
                return Ok(Effect::ApplyTheme("system".into()));
            }
            return Ok(Effect::Notification {
                title: format!("Reset {key}"),
                body: Some("Removed from config; default applies".into()),
            });
        }
        anyhow::bail!("unknown config candidate id: {id}")
    }
}

impl ConfigProvider {
    /// Seed the shared gate from config.json (at startup). Any provider
    /// id whose `providers.<id>.enabled` is `false` is added to the
    /// disabled set. Call once before registry starts serving
    /// queries
    pub fn load_disabled_into(gate: &DisabledProviders) {
        let cfg = load_config();
        let Some(providers) = cfg.get("providers").and_then(|v| v.as_object()) else {
            return;
        };
        let disabled: HashSet<String> = providers
            .iter()
            .filter_map(|(id, entry)| {
                // Accept value as bool (`false`), string (`"false"`,
                // `"off"`, `"no"`, `"0"`) or number (`0`). Hand-edited
                // config.json files frequently use quoted strings -
                // treating those as literal truthy values silently
                // defeats the intent. Missing key -> enabled (default)
                let value = entry.get("enabled")?;
                if interpret_falsy(value) { Some(id.clone()) } else { None }
            })
            .collect();
        gate.store(Arc::new(disabled));
    }

    fn apply_gate_update(&self, provider_id: &str, enabled: bool) {
        let current = self.gate.load();
        let mut next: HashSet<String> = (**current).clone();
        if enabled { next.remove(provider_id); } else { next.insert(provider_id.to_string()); }
        self.gate.store(Arc::new(next));
    }

    fn list_candidates(&self, filter: &str) -> Vec<Candidate> {
        let cfg = load_config();
        let filter_lower = filter.to_lowercase();
        let mut out = Vec::with_capacity(FIELDS.len() + self.provider_ids.len() + 1);

        // Static fields first (hotkey, theme, etc.) - they're what
        // user is typing TOWARDS. Surfacing them above the
        // generic "Open config.json" row means typing `config th`
        // highlights `theme` first instead of the open row that
        // happens to be permanently pinned at index 0
        for field in FIELDS {
            let current = current_value_display(&cfg, field);
            if !filter_lower.is_empty() && !matches_filter(field, &current, &filter_lower) {
                continue;
            }
            out.push(field_candidate(field, &current));
            if out.len() >= RESULT_LIMIT { break; }
        }

        // One toggle row per provider - fully dynamic, so adding a new
        // provider to registry automatically gets a config UI
        let disabled = self.gate.load();
        for pid in &self.provider_ids {
            let enabled = !disabled.contains(pid);
            let key = format!("providers.{pid}.enabled");
            if !filter_lower.is_empty()
                && !key.to_lowercase().contains(&filter_lower)
                && !pid.to_lowercase().contains(&filter_lower)
            {
                continue;
            }
            out.push(provider_toggle_candidate(pid, enabled));
            if out.len() >= RESULT_LIMIT { break; }
        }

        // "Open config.json" lands LAST - it's a navigation
        // shortcut, not a setting user is searching for. With a
        // non-empty filter that doesn't say "open" / "config.json",
        // skip it entirely so it doesn't pollute keystroke-narrow
        // results
        let open_matches_filter = filter_lower.is_empty()
            || "open config.json".contains(&filter_lower)
            || "config.json".contains(&filter_lower)
            || "open".starts_with(&filter_lower);
        if open_matches_filter && out.len() < RESULT_LIMIT {
            out.push(open_config_candidate());
        }

        out
    }
}

/// If `key` is `providers.<id>.enabled`, return `<id>`. Otherwise None.
/// Exposed because both the list UI and the apply path need the same
/// parse - keeping it in one place avoids drift
fn provider_enabled_key(key: &str) -> Option<String> {
    let rest = key.strip_prefix("providers.")?;
    let id = rest.strip_suffix(".enabled")?;
    if id.is_empty() { return None; }
    Some(id.to_string())
}

fn value_to_bool(value: &str) -> bool {
    matches!(value.to_lowercase().as_str(), "true" | "1" | "on")
}

fn provider_toggle_candidate(pid: &str, enabled: bool) -> Candidate {
    let state = if enabled { "on" } else { "off" };
    let icon = if enabled { "switch.2" } else { "circle.slash" };
    Candidate {
        id: format!("config::field::providers.{pid}.enabled"),
        title: format!("providers.{pid}"),
        subtitle: Some(format!(
            "Enable / disable the '{pid}' provider · currently {state} · default: on  ·  ↵ edit · → reset / open file"
        )),
        icon: Icon::SfSymbol(icon.into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Edit"),
            Action::new("reset", "Reset to default (enable)"),
            Action::new("open-file", "Open config.json"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Accept `config` (bare) and `config <rest>` - any other form is
/// someone else's problem
fn strip_keyword(s: &str) -> Option<&str> {
    if s == "config" { return Some(""); }
    s.strip_prefix("config ")
}


fn matches_filter(field: &Field, current: &str, filter_lower: &str) -> bool {
    field.key.to_lowercase().contains(filter_lower)
        || field.description.to_lowercase().contains(filter_lower)
        || current.to_lowercase().contains(filter_lower)
}

fn field_candidate(field: &Field, current: &str) -> Candidate {
    // Subtitle now carries the *how* as well as the *what*: users see
    // the description, current value, default, and the two key
    // gestures (Enter edit - -> for more actions, incl. Reset). The "Reset
    // to default" action-menu entry is where actual restore
    // happens; surfacing it in the subtitle makes it findable
    let subtitle = {
        let cur = if current.is_empty() { "(unset)".to_string() } else { format!("current: {current}") };
        let def = if field.default.is_empty() { String::new() } else { format!(" · default: {}", field.default) };
        let hint = "↵ edit · → reset / open file";
        format!("{} · {cur}{def}  ·  {hint}", field.description)
    };
    Candidate {
        id: format!("config::field::{}", field.key),
        title: field.key.into(),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol(icon_for(field.ty).into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Edit"),
            Action::new("reset", "Reset to default"),
            Action::new("open-file", "Open config.json"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn open_config_candidate() -> Candidate {
    Candidate {
        id: "config::open".into(),
        title: "Open config.json".into(),
        subtitle: Some(config_path().display().to_string()),
        icon: Icon::SfSymbol("doc.badge.gearshape".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Open")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// After `config set <key> <value>` user sees a preview confirm
/// row. Enter writes. When value is still empty (user just drilled
/// in), we surface:
/// - a header row with field's full description + current/default,
/// - for enum/bool types, one row per allowed value - Enter picks it,
///   no typing required
fn set_preview_candidates(rest: &str, provider_ids: &[String]) -> Vec<Candidate> {
    if rest.is_empty() { return vec![]; }

    let (key, value) = match rest.split_once(char::is_whitespace) {
        Some((k, v)) => (k.trim(), v.trim()),
        None => (rest, ""),
    };
    if key.is_empty() { return vec![]; }

    let field = FIELDS.iter().find(|f| f.key == key);
    let provider_toggle_id = provider_enabled_key(key)
        .filter(|id| provider_ids.iter().any(|p| p == id));

    // Value is empty -> browse mode: show explanation + choices
    if value.is_empty() {
        return browse_value_candidates(key, field, provider_toggle_id.as_deref());
    }

    // Value present -> confirm/save preview
    let (ok, subtitle) = if let Some(f) = field {
        match validate(f, value) {
            Ok(()) => (true, format!("{} · will write verbatim", f.description)),
            Err(e) => (false, format!("⚠ {e}")),
        }
    } else if let Some(id) = &provider_toggle_id {
        match value.to_lowercase().as_str() {
            "true" | "false" | "on" | "off" | "1" | "0" => (
                true,
                format!("Toggle '{id}' provider  ·  accepts true/false/on/off"),
            ),
            _ => (false, "⚠ expects true/false/on/off".to_string()),
        }
    } else {
        (true, "⚠ unknown key - written verbatim".to_string())
    };

    let icon = if ok { "checkmark.circle" } else { "exclamationmark.triangle.fill" };
    let mut out = vec![Candidate {
        id: format!("config::set::{key}::{value}"),
        title: format!("Set {key} = {value}"),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol(icon.into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary(if ok { "Save" } else { "Save anyway" })],
        search_text: String::new(),
        bypass_rank: true,
    }];

    // For enum/bool fields, also offer any allowed values that still
    // prefix-match the partial input - classic autocomplete. Skip
    // the exact match so it doesn't duplicate the confirm row above
    if let Some(f) = field {
        if matches!(f.ty, FieldType::App) {
            // App-typed fields fan out into a fuzzy app picker -
            // type "iterm" and pick iTerm without remembering the
            // exact path. Candidate row reads as the friendly
            // display name; the saved value is the bundle's full
            // path, which `open -a` always resolves correctly
            for hit in match_apps(value, 12) {
                if hit.path_str == value { continue; }
                out.push(app_suggestion_candidate(key, &hit));
            }
            return out;
        }
        for suggestion in suggestions_for(f.ty, Some(value)) {
            if suggestion == value { continue; }
            out.push(value_suggestion_candidate_field(key, &suggestion, f));
        }
    } else if provider_toggle_id.is_some() {
        for suggestion in ["true", "false"] {
            if suggestion == value { continue; }
            if !suggestion.to_lowercase().starts_with(&value.to_lowercase()) { continue; }
            out.push(value_suggestion_candidate_bool(key, suggestion));
        }
    }
    out
}

/// Search installed `.app` bundles by fuzzy substring against both
/// display name and bundle id. Used by `App`-typed config fields.
/// Cached per-process since the scan is inexpensive but not free
///
/// Returned values are bundle absolute paths
/// (e.g. `/Applications/iTerm.app`), not display names. `open -a`
/// accepts both, but paths sidestep the cases where the bundle's
/// `CFBundleDisplayName` disagrees with its on-disk name (or with
/// what Launch Services would resolve from the display alone) -
/// path always works, no surprise lookup failures
pub struct AppHit {
    pub display_name: String,
    pub path_str: String,
}

fn match_apps(filter: &str, limit: usize) -> Vec<AppHit> {
    let apps = cached_apps();
    let f = filter.to_lowercase();
    let mut hits: Vec<&crate::apps::AppEntry> = apps
        .iter()
        .filter(|a| {
            f.is_empty()
                || a.display_name.to_lowercase().contains(&f)
                || a.bundle_id
                    .as_deref()
                    .map(|b| b.to_lowercase().contains(&f))
                    .unwrap_or(false)
        })
        .collect();
    // Shortest-name first surfaces "iTerm" above "iTerm Wrapper"
    // when both match
    hits.sort_by_key(|a| a.display_name.len());
    hits.truncate(limit);
    hits.into_iter()
        .map(|a| AppHit {
            display_name: a.display_name.clone(),
            path_str: a.path.to_string_lossy().into_owned(),
        })
        .collect()
}

fn cached_apps() -> &'static Vec<crate::apps::AppEntry> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<crate::apps::AppEntry>> = OnceLock::new();
    CACHE.get_or_init(crate::apps::scan_app_bundles)
}

/// Present user with a rich "what does this setting do, and what
/// can I put here?" view when they've drilled in via Edit but haven't
/// typed a value yet
fn browse_value_candidates(
    key: &str,
    field: Option<&Field>,
    provider_toggle_id: Option<&str>,
) -> Vec<Candidate> {
    let cfg = load_config();
    let current = get_dotted(&cfg, key).map(value_to_display).unwrap_or_default();

    let mut out = Vec::new();
    if let Some(f) = field {
        // Static-field explanation header + suggestions
        let expected = expected_values(f.ty);
        let cur = if current.is_empty() { "(unset)".to_string() } else { current.clone() };
        let def = if f.default.is_empty() { "(unset)".to_string() } else { f.default.to_string() };
        let subtitle = format!(
            "{} · current: {cur} · default: {def} · expects {expected}",
            f.description
        );
        out.push(Candidate {
            id: format!("config::help::{key}"),
            title: key.to_string(),
            subtitle: Some(subtitle),
            icon: Icon::SfSymbol("info.circle.fill".into()),
            kind: CandidateKind::Action,
            actions: vec![
                Action::primary("Open config.json"),
                Action::new("reset", "Reset to default"),
            ],
            search_text: String::new(),
            bypass_rank: true,
        });
        // For Path-typed fields, surface a Browse row that opens the
        // native folder picker. Keyboard-only users still type a
        // path; pointer/screenshot users get the list-based selector
        // they'd expect from any macOS app
        if matches!(f.ty, FieldType::Path) {
            out.push(browse_folder_candidate(key, f));
        }
        // App-typed fields surface a fuzzy app picker, but only
        // once user has typed at least one character. Showing
        // every installed app on the bare browse view drowned the
        // help row in noise - type "iterm" and you get iTerm; do
        // nothing and you get the description + a hint
        for suggestion in suggestions_for(f.ty, None) {
            out.push(value_suggestion_candidate_field(key, &suggestion, f));
        }
    } else if let Some(id) = provider_toggle_id {
        // Dynamic provider toggle - treat as Bool
        let cur = if current.is_empty() { "enabled (default)".to_string() } else { current.clone() };
        let subtitle = format!(
            "Enable or disable the '{id}' provider · current: {cur} · default: enabled · expects true/false"
        );
        out.push(Candidate {
            id: format!("config::help::{key}"),
            title: key.to_string(),
            subtitle: Some(subtitle),
            icon: Icon::SfSymbol("switch.2".into()),
            kind: CandidateKind::Action,
            actions: vec![
                Action::primary("Open config.json"),
                Action::new("reset", "Reset to default (enable)"),
            ],
            search_text: String::new(),
            bypass_rank: true,
        });
        for suggestion in ["true", "false"] {
            out.push(value_suggestion_candidate_bool(key, suggestion));
        }
    } else {
        // Completely unknown key - let user still write it, but
        // flag clearly
        out.push(Candidate {
            id: format!("config::help::{key}"),
            title: key.to_string(),
            subtitle: Some(format!(
                "unknown key · current: {} · any value will be written verbatim",
                if current.is_empty() { "(unset)" } else { &current }
            )),
            icon: Icon::SfSymbol("questionmark.circle".into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("OK")],
            search_text: String::new(),
            bypass_rank: true,
        });
    }
    out
}

fn browse_folder_candidate(key: &str, field: &Field) -> Candidate {
    Candidate {
        id: format!("config::browse::{key}"),
        title: format!("Browse folder for {key}…"),
        subtitle: Some(format!("{} · opens the native folder picker", field.description)),
        icon: Icon::SfSymbol("folder.badge.plus".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Browse…")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn value_suggestion_candidate_field(key: &str, value: &str, field: &Field) -> Candidate {
    Candidate {
        id: format!("config::set::{key}::{value}"),
        title: format!("{key} = {value}"),
        subtitle: Some(format!("Set and save  ·  {}", field.description)),
        icon: Icon::SfSymbol("arrow.right.circle".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Save")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Specialised version for App-typed fields. Title shows the
/// friendly name, subtitle shows path that'll be saved, id
/// encodes path so `apply_set` writes that
fn app_suggestion_candidate(key: &str, hit: &AppHit) -> Candidate {
    Candidate {
        id: format!("config::set::{key}::{}", hit.path_str),
        title: format!("{key} = {}", hit.display_name),
        subtitle: Some(format!("Save path: {}", hit.path_str)),
        icon: Icon::SfSymbol("app.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Save")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn value_suggestion_candidate_bool(key: &str, value: &str) -> Candidate {
    let hint = match value {
        "true"  => "Enable",
        "false" => "Disable",
        _ => "Set",
    };
    Candidate {
        id: format!("config::set::{key}::{value}"),
        title: format!("{key} = {value}"),
        subtitle: Some(format!("{hint} and save")),
        icon: Icon::SfSymbol(if value == "true" { "checkmark.circle.fill" } else { "xmark.circle" }.into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Save")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn expected_values(ty: FieldType) -> String {
    match ty {
        FieldType::Bool => "true / false".into(),
        FieldType::Text => "any text".into(),
        FieldType::Path => "filesystem path (~ / $HOME expanded)".into(),
        FieldType::Enum(vs) => format!("one of: {}", vs.join(", ")),
        FieldType::App => "name of an installed app - type to search".into(),
    }
}

/// For enum/bool types, enumerate allowed values. If `filter` is
/// supplied, narrow to prefix-matching entries
fn suggestions_for(ty: FieldType, filter: Option<&str>) -> Vec<String> {
    let pool: Vec<String> = match ty {
        FieldType::Bool => vec!["true".into(), "false".into()],
        FieldType::Enum(vs) => vs.iter().map(|s| (*s).to_string()).collect(),
        _ => return Vec::new(),
    };
    match filter {
        Some(f) if !f.is_empty() => pool
            .into_iter()
            .filter(|v| v.to_lowercase().starts_with(&f.to_lowercase()))
            .collect(),
        _ => pool,
    }
}

fn reset_preview_candidates(key: &str) -> Vec<Candidate> {
    if key.is_empty() { return vec![]; }
    let default = FIELDS.iter().find(|f| f.key == key).map(|f| f.default).unwrap_or("(removed)");
    vec![Candidate {
        id: format!("config::reset::{key}"),
        title: format!("Reset {key}"),
        subtitle: Some(format!("Removes the key from config; built-in default applies ({default})")),
        icon: Icon::SfSymbol("arrow.uturn.backward.circle".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Reset")],
        search_text: String::new(),
        bypass_rank: true,
    }]
}

fn icon_for(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Bool => "switch.2",
        FieldType::Text => "textformat",
        FieldType::Path => "folder",
        FieldType::Enum(_) => "list.bullet.rectangle",
        FieldType::App => "app.dashed",
    }
}


fn handle_field_action(key: &str, action: &str) -> Effect {
    match action {
        "default" | "edit" => {
            // Autocomplete-style: pre-fill the edit command so user
            // can tab/type their way through the change
            Effect::SetInput(format!("config set {key} "))
        }
        "reset" => Effect::SetInput(format!("config reset {key}")),
        "open-file" => Effect::OpenPath(config_path()),
        _ => Effect::None,
    }
}

/// Persist a provider on/off toggle and update the in-memory gate in
/// one call. Pulls together the two halves that ConfigProvider does
/// inline (`apply_set` + `apply_gate_update`) so providers other than
/// config UI can flip themselves off - e.g. a "Dont show again"
/// row that hides a discoverability hint set
pub fn set_provider_enabled(
    provider_id: &str,
    enabled: bool,
    gate: &DisabledProviders,
) -> Result<()> {
    let key = format!("providers.{provider_id}.enabled");
    apply_set(&key, if enabled { "true" } else { "false" })?;
    let current = gate.load();
    let mut next: HashSet<String> = (**current).clone();
    if enabled {
        next.remove(provider_id);
    } else {
        next.insert(provider_id.to_string());
    }
    gate.store(Arc::new(next));
    Ok(())
}

fn apply_set(key: &str, value: &str) -> Result<()> {
    // Known keys get type coercion (bool strings -> JSON bool, etc.).
    // `providers.<id>.enabled` is a dynamic key (one per registered
    // provider, not in FIELDS) but semantically a Bool - force bool
    // coercion for it too, so toggles persist as JSON `true`/`false`
    // rather than `"true"`/`"false"` strings. Mixed-form config
    // files used to confuse the gate loader (the string `"false"`
    // does get interpreted as disabled today, but round-tripping via
    // the UI could leave stale string values behind that were hard
    // to audit)
    let coerced = if provider_enabled_key(key).is_some() {
        coerce(FieldType::Bool, value)?
    } else {
        match FIELDS.iter().find(|f| f.key == key) {
            Some(f) => coerce(f.ty, value)?,
            // Unknown keys pass through as string so contributors can
            // write plugin config without the launcher second-guessing
            None => Value::String(value.to_string()),
        }
    };
    let mut root = load_config();
    set_dotted(&mut root, key, coerced);
    save_config(&root)?;
    Ok(())
}

fn apply_reset(key: &str) -> Result<()> {
    let mut root = load_config();
    if remove_dotted(&mut root, key) {
        save_config(&root)?;
    }
    Ok(())
}


fn validate(field: &Field, value: &str) -> Result<()> {
    match field.ty {
        FieldType::Bool => {
            match value.to_lowercase().as_str() {
                "true" | "false" | "1" | "0" | "on" | "off" => Ok(()),
                _ => anyhow::bail!("bool expects true/false/on/off, got {value:?}"),
            }
        }
        FieldType::Enum(allowed) => {
            if allowed.contains(&value) {
                Ok(())
            } else {
                anyhow::bail!("must be one of {}", allowed.join(", "))
            }
        }
        FieldType::Path | FieldType::Text | FieldType::App => {
            if value.is_empty() { anyhow::bail!("value cannot be empty"); }
            Ok(())
        }
    }
}

fn coerce(ty: FieldType, value: &str) -> Result<Value> {
    match ty {
        FieldType::Bool => match value.to_lowercase().as_str() {
            "true" | "1" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "off" => Ok(Value::Bool(false)),
            _ => anyhow::bail!("cannot coerce {value:?} to bool"),
        },
        FieldType::Enum(_) | FieldType::Text | FieldType::Path | FieldType::App => {
            Ok(Value::String(value.to_string()))
        }
    }
}


pub fn get_dotted<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    let mut cur = root;
    for segment in key.split('.') {
        cur = cur.get(segment)?;
    }
    Some(cur)
}

pub fn set_dotted(root: &mut Value, key: &str, value: Value) {
    // Promote the root to an object if it isn't one yet (fresh config)
    if !root.is_object() {
        *root = Value::Object(Map::new());
    }
    let parts: Vec<&str> = key.split('.').collect();
    let mut cur = root;
    for seg in &parts[..parts.len() - 1] {
        let obj = cur.as_object_mut().expect("object");
        if !matches!(obj.get(*seg), Some(Value::Object(_))) {
            obj.insert((*seg).to_string(), Value::Object(Map::new()));
        }
        cur = obj.get_mut(*seg).expect("just-inserted");
    }
    cur.as_object_mut()
        .expect("object")
        .insert(parts.last().unwrap().to_string(), value);
}

/// Remove `key` if present. Returns true iff the tree actually changed
pub fn remove_dotted(root: &mut Value, key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    let mut cur = root;
    for seg in &parts[..parts.len() - 1] {
        match cur.get_mut(*seg) {
            Some(v @ Value::Object(_)) => cur = v,
            _ => return false,
        }
    }
    cur.as_object_mut()
        .and_then(|o| o.remove(*parts.last().unwrap()))
        .is_some()
}

fn value_to_display(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => v.to_string(),
    }
}

fn current_value_display(cfg: &Value, field: &Field) -> String {
    get_dotted(cfg, field.key).map(value_to_display).unwrap_or_default()
}

fn split_once_on_double_colon(s: &str) -> Option<(&str, &str)> {
    s.find("::").map(|i| (&s[..i], &s[i + 2..]))
}


pub fn config_path() -> PathBuf {
    // Tests need to redirect writes away from user's real config.
    // `XDG_DATA_HOME` is the Freedesktop convention - but on macOS
    // `dirs::data_local_dir()` ignores it (returns `~/Library/
    // Application Support` unconditionally), so tests that set
    // `XDG_DATA_HOME` on macOS silently clobber the real user config
    // whenever they invoke `apply_set`. That actually happened: every
    // `cargo test` run was flipping `providers.note.enabled` to false
    // on the developer's machine
    //
    // Gyors-specific override wins everywhere: set `GYORS_CONFIG_DIR`
    // and writes land inside it instead. Production never sets this
    // so users see no behaviour change
    if let Ok(dir) = std::env::var("GYORS_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("config.json");
        }
    }
    dirs::data_local_dir()
        .map(|b| b.join("Gyors").join("config.json"))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

/// Return the Gyors data root, ensuring it exists with
/// 0700 perms before handing path back. Files inside the root
/// are typically 0600 already; the parent directory was created
/// with user's umask (commonly 0755), which means a different
/// user on a shared Mac can list plugin ids, theme names, and
/// clipboard image SHAs even though they can't read the contents
///
/// Idempotent + cheap (one stat + one chmod when the perms already
/// match the OS no-op'd). Call this from every save path that
/// previously relied on bare `create_dir_all(parent)`
pub fn ensure_data_dir_secured() -> std::io::Result<PathBuf> {
    let dir = gyors_data_dir();
    std::fs::create_dir_all(&dir)?;
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(&dir, perms)?;
    Ok(dir)
}

/// The Gyors data root as a path. Mirrors `config_path()`'s
/// environment-override logic so tests with `GYORS_CONFIG_DIR` set
/// chmod a tempdir, not user's real `~/Library/Application
/// Support/Gyors/`
pub fn gyors_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("GYORS_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    dirs::data_local_dir()
        .map(|b| b.join("Gyors"))
        .unwrap_or_else(|| PathBuf::from("Gyors"))
}

/// Shared test-only utilities. Other providers' tests that touch
/// `apply_set` (and therefore `GYORS_CONFIG_DIR`) must acquire
/// `CONFIG_PATH_LOCK` so env-var override doesn't get clobbered
/// by a parallel test setting it to a different tempdir
#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) static CONFIG_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}

pub fn load_config() -> Value {
    match std::fs::read_to_string(config_path()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| Value::Object(Map::new())),
        Err(_) => Value::Object(Map::new()),
    }
}

/// User-configured clipboard retention. Returns the raw preference
/// (no tier clamping) so caller can decide how to combine it
/// with active tier's hard ceiling. Garbage values (negative,
/// non-numeric, zero) come back as `None`, falling back to the
/// caller's default. Reads the live config file every call - that's
/// fine because the sync FFI's `apply_tier` is only caller and
/// it fires on auth transitions, not the hot path
pub fn clipboard_max_items_pref() -> Option<usize> {
    let root = load_config();
    let raw = root.get("clipboard_max_items")?;
    let n = match raw {
        Value::Number(n) => n.as_i64()?,
        Value::String(s) => s.trim().parse::<i64>().ok()?,
        _ => return None,
    };
    if n < 1 {
        return None;
    }
    Some(n as usize)
}

/// User-configured roots for the `repo` keyword scanner. Returns a
/// parsed list of paths when the `repo_roots` key is present and
/// produces at least one valid entry, otherwise `None` so caller
/// falls back to the built-in defaults
///
/// Each entry MUST be either an absolute path (starts with `/`) or a
/// tilde-prefixed path (`~` alone, or `~/...`). Bare names like
/// `Documents` are rejected with a log line, not silently
/// reinterpreted as `$CWD/Documents` - that's almost never what the
/// user meant and the CWD of a launcher process is opaque
///
/// The split is comma-tolerant of incidental whitespace so users can
/// write either `~/Documents,~/Projects` or `~/Documents, ~/Projects`
pub fn repo_roots_pref() -> Option<Vec<PathBuf>> {
    let root = load_config();
    let raw = root.get("repo_roots")?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let paths: Vec<PathBuf> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            if s == "~" || s.starts_with("~/") || s.starts_with('/') {
                Some(crate::notes::expand_tilde(s))
            } else {
                tracing::warn!(
                    target: "gyors::config",
                    "repo_roots entry {:?} ignored: must be an absolute (/…) or tilde (~/…) path",
                    s
                );
                None
            }
        })
        .collect();
    if paths.is_empty() {
        None
    } else {
        Some(paths)
    }
}

/// Liberal truthiness check for `enabled`-style toggles in hand-edited
/// config files. Accepts JSON native `false`, common string
/// representations (`"false"`, `"no"`, `"off"`, `"0"`), and numeric
/// `0`. Anything else (including obvious truthy values and garbage)
/// is treated as NOT-falsy, i.e. provider stays enabled -
/// defaults favour visibility over silent disablement
fn interpret_falsy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => !b,
        Value::String(s) => {
            matches!(
                s.trim().to_lowercase().as_str(),
                "false" | "no" | "off" | "0" | "disabled"
            )
        }
        Value::Number(n) => n.as_i64().map(|x| x == 0).unwrap_or(false),
        _ => false,
    }
}

pub fn save_config(root: &Value) -> Result<()> {
    let path = config_path();
    // Chmod the Gyors data root to 0700 every save. The
    // call is idempotent so paying it here means we never miss it
    // for an early-startup save before AppDelegate has wired up
    let _ = ensure_data_dir_secured();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let text = serde_json::to_string_pretty(root).context("serialize config")?;
    std::fs::write(&path, text).context("write config.json")?;
    // Fire the post-save hook (if any) so layers above can react -
    // e.g. gyors-ipc registers a hook that enqueues a sync outbox
    // row whenever config changes. We can't depend on
    // gyors-sync from here without inverting layer order, so the
    // wire-up is callback-based
    if let Some(hook) = SAVE_HOOK.read().ok().and_then(|g| g.as_ref().map(Arc::clone)) {
        hook(root);
    }
    Ok(())
}

/// Hook type called after every successful `save_config`. Synchronous;
/// callers should keep work cheap (enqueue, schedule, log). Heavy
/// work belongs on a background thread that the hook kicks off
pub type SaveHook = Arc<dyn Fn(&Value) + Send + Sync>;

static SAVE_HOOK: std::sync::RwLock<Option<SaveHook>> = std::sync::RwLock::new(None);

/// Register a function to fire after every `save_config`. The
/// previous hook (if any) is replaced - we expect one wiring point
/// per process (the launcher's bridge), not a stack of subscribers
///
/// Cleanest setter for tests / re-init scenarios. Production code
/// calls this once during launcher startup
pub fn set_save_hook(hook: SaveHook) {
    if let Ok(mut g) = SAVE_HOOK.write() {
        *g = Some(hook);
    }
}

/// Drop the registered hook. Tests use this to keep callbacks from
/// leaking between cases when they share a process
pub fn clear_save_hook() {
    if let Ok(mut g) = SAVE_HOOK.write() {
        *g = None;
    }
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)] // test serialization uses sync Mutex on $HOME
mod tests {
    use super::*;
    use serde_json::json;


    #[test]
    fn interpret_falsy_accepts_json_bool() {
        assert!(interpret_falsy(&json!(false)));
        assert!(!interpret_falsy(&json!(true)));
    }

    #[test]
    fn interpret_falsy_accepts_strings_users_actually_write() {
        // The whole point: a config file that says `"enabled": "false"`
        // (quoted) should disable provider, not be silently ignored
        for s in ["false", "FALSE", "False", " false ", "no", "off", "0", "disabled"] {
            assert!(
                interpret_falsy(&json!(s)),
                "expected {s:?} to disable"
            );
        }
    }

    #[test]
    fn interpret_falsy_leaves_truthy_and_garbage_alone() {
        for v in [json!("true"), json!("yes"), json!("on"), json!(1), json!("perhaps"), json!(null)] {
            assert!(!interpret_falsy(&v), "expected {v:?} to stay enabled");
        }
    }

    #[test]
    fn interpret_falsy_numeric_zero() {
        assert!(interpret_falsy(&json!(0)));
        assert!(!interpret_falsy(&json!(2)));
    }


    #[test]
    fn get_dotted_finds_nested() {
        let v = json!({ "a": { "b": { "c": 42 } } });
        assert_eq!(get_dotted(&v, "a.b.c"), Some(&json!(42)));
        assert_eq!(get_dotted(&v, "a.missing"), None);
    }

    #[test]
    fn set_dotted_creates_missing_parents() {
        let mut v = Value::Object(Map::new());
        set_dotted(&mut v, "ai.model", json!("llama3"));
        assert_eq!(v, json!({ "ai": { "model": "llama3" } }));
    }

    #[test]
    fn set_dotted_preserves_siblings() {
        let mut v = json!({ "theme": "nord", "ai": { "model": "gpt-4" } });
        set_dotted(&mut v, "ai.provider", json!("openai"));
        assert_eq!(v["theme"], "nord");
        assert_eq!(v["ai"]["model"], "gpt-4");
        assert_eq!(v["ai"]["provider"], "openai");
    }

    #[test]
    fn set_dotted_promotes_non_object_root() {
        let mut v = json!(null);
        set_dotted(&mut v, "theme", json!("nord"));
        assert_eq!(v, json!({ "theme": "nord" }));
    }

    #[test]
    fn remove_dotted_returns_true_when_changed() {
        let mut v = json!({ "a": { "b": 1 } });
        assert!(remove_dotted(&mut v, "a.b"));
        assert_eq!(v, json!({ "a": {} }));
    }

    #[test]
    fn remove_dotted_false_when_missing() {
        let mut v = json!({ "a": 1 });
        assert!(!remove_dotted(&mut v, "nope"));
    }


    #[test]
    fn coerce_bool_accepts_multiple_spellings() {
        assert_eq!(coerce(FieldType::Bool, "true").unwrap(), json!(true));
        assert_eq!(coerce(FieldType::Bool, "FALSE").unwrap(), json!(false));
        assert_eq!(coerce(FieldType::Bool, "on").unwrap(), json!(true));
        assert_eq!(coerce(FieldType::Bool, "0").unwrap(), json!(false));
    }

    #[test]
    fn coerce_bool_rejects_junk() {
        assert!(coerce(FieldType::Bool, "maybe").is_err());
    }

    #[test]
    fn validate_enum_rejects_outsiders() {
        let f = Field { key: "x", description: "", default: "",
            ty: FieldType::Enum(&["a", "b", "c"]) };
        assert!(validate(&f, "a").is_ok());
        assert!(validate(&f, "z").is_err());
    }

    #[test]
    fn validate_text_requires_non_empty() {
        let f = Field { key: "x", description: "", default: "", ty: FieldType::Text };
        assert!(validate(&f, "").is_err());
        assert!(validate(&f, "anything").is_ok());
    }


    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = ConfigProvider::default();
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_fields_plus_open_row() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config")).await;
        assert!(out.iter().any(|c| c.id == "config::open"));
        assert!(out.iter().any(|c| c.id.starts_with("config::field::theme")));
    }

    #[tokio::test]
    async fn fields_appear_above_open_row_so_typing_narrows_to_them() {
        // Regression: typing `config th` used to highlight "Open
        // config.json" as the top row because it was always pinned
        // at index 0. Users typing toward a setting want the SETTING
        // surfaced first; the open row is a navigation shortcut,
        // not what they're searching for
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config th")).await;
        let theme_pos = out.iter().position(|c| c.id == "config::field::theme");
        let open_pos = out.iter().position(|c| c.id == "config::open");
        assert!(theme_pos.is_some(), "theme field must be in results");
        match (theme_pos, open_pos) {
            (Some(t), Some(o)) => assert!(
                t < o,
                "theme field (pos {t}) must come before open row (pos {o})",
            ),
            // Open row absent entirely with this filter is fine -
            // even better - since `th` doesn't say "open"
            (Some(_), None) => {}
            _ => panic!("theme field missing"),
        }
    }

    #[tokio::test]
    async fn open_row_disappears_for_filters_that_dont_match_open() {
        // `config xyz` is a typo / unknown field. The list should
        // be empty (or near it) - NOT a lone "Open config.json"
        // pretending the typo found something
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config qzqzqz")).await;
        assert!(
            !out.iter().any(|c| c.id == "config::open"),
            "open row must not survive a filter it doesn't match",
        );
    }

    #[tokio::test]
    async fn open_row_returns_when_user_types_open() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config open")).await;
        assert!(
            out.iter().any(|c| c.id == "config::open"),
            "explicit 'open' filter must surface the open row",
        );
    }

    #[tokio::test]
    async fn filter_narrows_to_matching_fields() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config theme")).await;
        // "Open config.json" + theme-adjacent fields. The filter
        // matches against both field key AND its description,
        // so panel_opacity / panel_blur (whose descriptions
        // explicitly call out "theme") legitimately surface here
        // too - that's the discoverability we want
        assert!(out.iter().all(|c| {
            let id = c.id.as_str();
            let sub = c.subtitle.as_deref().unwrap_or("").to_lowercase();
            id == "config::open"
                || id.ends_with("::theme")
                || (id.starts_with("config::field::")
                    && (c.title.contains("theme") || sub.contains("theme")))
        }));
    }

    #[tokio::test]
    async fn field_edit_primary_emits_autocomplete_setinput() {
        let p = ConfigProvider::default();
        let eff = p
            .activate(&"config::field::theme".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::SetInput(s) => assert_eq!(s, "config set theme "),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn field_reset_action_emits_reset_setinput() {
        let p = ConfigProvider::default();
        let eff = p
            .activate(&"config::field::theme".to_string(), "reset")
            .await
            .unwrap();
        match eff {
            Effect::SetInput(s) => assert_eq!(s, "config reset theme"),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn field_open_action_emits_openpath() {
        let p = ConfigProvider::default();
        let eff = p
            .activate(&"config::field::theme".to_string(), "open-file")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::OpenPath(_)));
    }

    #[tokio::test]
    async fn set_preview_known_key_with_value() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config set theme nord")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("config::set::theme::nord"));
        assert!(out[0].title.contains("Set theme = nord"));
    }

    #[tokio::test]
    async fn set_preview_flags_invalid_enum_value() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config set theme bogus")).await;
        assert_eq!(out.len(), 1);
        let sub = out[0].subtitle.as_deref().unwrap_or("");
        assert!(sub.contains("⚠"), "got {sub:?}");
    }

    #[tokio::test]
    async fn set_preview_accepts_unknown_key_with_warning() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config set custom.key value")).await;
        assert_eq!(out.len(), 1);
        let sub = out[0].subtitle.as_deref().unwrap_or("");
        assert!(sub.contains("unknown key"));
    }

    #[tokio::test]
    async fn set_without_value_shows_explanation_plus_choices() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config set theme")).await;
        // Header row with explanation + current/default + allowed
        // values, then one row per theme enum value
        assert!(!out.is_empty());
        assert!(out[0].id.starts_with("config::help::"));
        let sub = out[0].subtitle.as_deref().unwrap_or("");
        assert!(sub.contains("current"));
        assert!(sub.contains("default"));
        assert!(sub.contains("expects"));
        // One row per enum value is clickable
        assert!(out.iter().any(|c| c.id == "config::set::theme::midnight"));
        assert!(out.iter().any(|c| c.id == "config::set::theme::nord"));
    }

    #[tokio::test]
    async fn set_with_partial_value_narrows_enum_suggestions() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config set theme mi")).await;
        // Confirm row for "mi" (invalid enum, flagged) plus prefix-
        // matching suggestions - only `midnight` in the theme set
        assert!(out.iter().any(|c| c.id == "config::set::theme::mi"));
        assert!(out.iter().any(|c| c.id == "config::set::theme::midnight"));
        assert!(!out.iter().any(|c| c.id == "config::set::theme::nord"));
    }

    #[tokio::test]
    async fn set_without_value_bool_offers_true_false() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config set clipboard_enabled")).await;
        assert!(out.iter().any(|c| c.id == "config::set::clipboard_enabled::true"));
        assert!(out.iter().any(|c| c.id == "config::set::clipboard_enabled::false"));
    }

    #[tokio::test]
    async fn set_without_value_unknown_key_still_helpful() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config set unknown_key")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("config::help::"));
        let sub = out[0].subtitle.as_deref().unwrap_or("");
        assert!(sub.contains("unknown key"));
    }

    #[tokio::test]
    async fn field_row_subtitle_mentions_reset_path() {
        // Regression: users were asking "how do I reset?" - the hint
        // must live in field row itself so it's discoverable
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config")).await;
        let theme_row = out.iter().find(|c| c.id.ends_with("::theme")).unwrap();
        let sub = theme_row.subtitle.as_deref().unwrap_or("");
        assert!(sub.contains("reset"), "got {sub:?}");
    }

    #[tokio::test]
    async fn reset_preview_row_for_known_key() {
        let p = ConfigProvider::default();
        let out = p.query(&Query::new("config reset theme")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "config::reset::theme");
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = ConfigProvider::default();
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_open_opens_config_path() {
        let p = ConfigProvider::default();
        let eff = p.activate(&"config::open".to_string(), "default").await.unwrap();
        match eff {
            Effect::OpenPath(path) => assert!(path.to_string_lossy().contains("config.json")),
            other => panic!("expected OpenPath, got {other:?}"),
        }
    }


    #[test]
    fn provider_enabled_key_parser() {
        assert_eq!(provider_enabled_key("providers.apps.enabled"), Some("apps".into()));
        assert_eq!(provider_enabled_key("providers.custom.enabled"), Some("custom".into()));
        assert_eq!(provider_enabled_key("providers.x.enabled"), Some("x".into()));
        // Negative cases
        assert_eq!(provider_enabled_key("providers.apps"), None);
        assert_eq!(provider_enabled_key("apps.enabled"), None);
        assert_eq!(provider_enabled_key("providers..enabled"), None);
        assert_eq!(provider_enabled_key("theme"), None);
    }

    #[tokio::test]
    async fn toggle_rows_emitted_per_registered_provider() {
        let gate = new_disabled_set();
        let p = ConfigProvider::new(gate, vec!["note".into(), "emoji".into(), "ai".into()]);
        let out = p.query(&Query::new("config")).await;
        // One toggle row per id (core ids like "apps" are filtered out
        // by ConfigProvider::new - always-on)
        assert!(out.iter().any(|c| c.id == "config::field::providers.note.enabled"));
        assert!(out.iter().any(|c| c.id == "config::field::providers.emoji.enabled"));
        assert!(out.iter().any(|c| c.id == "config::field::providers.ai.enabled"));
    }

    #[tokio::test]
    async fn always_on_providers_are_never_toggleable() {
        let gate = new_disabled_set();
        // Pass core ids through - ConfigProvider::new must strip them
        // from the toggleable list. "apps" is core by ALWAYS_ON
        let p = ConfigProvider::new(
            gate,
            vec!["apps".into(), "config".into(), "hint".into(), "note".into()],
        );
        let out = p.query(&Query::new("config")).await;
        assert!(!out.iter().any(|c| c.id.contains("providers.apps.enabled")));
        assert!(!out.iter().any(|c| c.id.contains("providers.config.enabled")));
        assert!(!out.iter().any(|c| c.id.contains("providers.hint.enabled")));
        assert!(out.iter().any(|c| c.id == "config::field::providers.note.enabled"));
    }

    #[tokio::test]
    async fn toggle_browse_bool_choices() {
        let gate = new_disabled_set();
        let p = ConfigProvider::new(gate, vec!["note".into()]);
        let out = p.query(&Query::new("config set providers.note.enabled")).await;
        // Header row + two bool choice rows
        assert!(out.iter().any(|c| c.id.starts_with("config::help::")));
        assert!(out.iter().any(|c| c.id == "config::set::providers.note.enabled::true"));
        assert!(out.iter().any(|c| c.id == "config::set::providers.note.enabled::false"));
    }

    #[tokio::test]
    async fn toggle_confirm_accepts_bool_aliases() {
        let gate = new_disabled_set();
        let p = ConfigProvider::new(gate, vec!["note".into()]);
        for truthy in ["true", "false", "on", "off", "1", "0"] {
            let q = format!("config set providers.note.enabled {truthy}");
            let out = p.query(&Query::new(&q)).await;
            let confirm = out.iter()
                .find(|c| c.id.starts_with("config::set::providers.note.enabled::"))
                .expect("confirm row present");
            assert!(!confirm.subtitle.as_deref().unwrap_or("").contains("⚠"),
                "{truthy} should not be flagged invalid");
        }
    }

    #[tokio::test]
    async fn toggle_confirm_flags_invalid_value() {
        let gate = new_disabled_set();
        let p = ConfigProvider::new(gate, vec!["note".into()]);
        let out = p.query(&Query::new("config set providers.note.enabled maybe")).await;
        let confirm = out.iter()
            .find(|c| c.id == "config::set::providers.note.enabled::maybe")
            .expect("confirm row");
        assert!(confirm.subtitle.as_deref().unwrap_or("").contains("⚠"));
    }

    use super::test_support::CONFIG_PATH_LOCK;

    #[test]
    fn apply_set_provider_toggle_writes_json_bool_not_string() {
        // REGRESSION: provider toggles fell through to the
        // "unknown key" branch and were persisted as strings
        // (`"false"` / `"true"`). The gate loader accepts those
        // now via `interpret_falsy`, but mixed-shape config files
        // (some entries strings, some bools) became hard to audit
        // and confused the round-trip. Pin canonical form
        //
        // Isolation: `GYORS_CONFIG_DIR` honours override on every OS
        // (the old `XDG_DATA_HOME` approach silently wrote into the
        // developer's real `~/Library/Application Support/Gyors/` on
        // macOS - `cargo test` was toggling notes off on every run)
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());

        apply_set("providers.note.enabled", "false").unwrap();
        let cfg = load_config();
        let written = get_dotted(&cfg, "providers.note.enabled")
            .expect("provider toggle written");
        assert_eq!(
            written,
            &json!(false),
            "must persist as JSON bool, got {written:?}"
        );

        apply_set("providers.note.enabled", "true").unwrap();
        let cfg = load_config();
        let written = get_dotted(&cfg, "providers.note.enabled").unwrap();
        assert_eq!(written, &json!(true));

        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[tokio::test]
    async fn theme_set_returns_apply_theme_effect() {
        // Regression: `config set theme <id>` used to write to disk
        // and emit Effect::Notification. Swift side never knew
        // it should re-render with the new theme, so panel kept
        // its old colors until relaunch. Now we emit
        // Effect::ApplyTheme so Swift `ThemeManager` can flip
        // live - same effect menu-bar picker fires
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());

        let p = ConfigProvider::default();
        let effect = p
            .activate(&"config::set::theme::sunset".to_string(), "default")
            .await
            .unwrap();
        match effect {
            Effect::ApplyTheme(id) => assert_eq!(id, "sunset"),
            other => panic!("expected ApplyTheme(sunset), got {other:?}"),
        }
        // And the on-disk write still happened - same canonical
        // shape menu-bar persistence uses, so a future relaunch
        // picks up same theme
        let cfg = load_config();
        assert_eq!(
            get_dotted(&cfg, "theme"),
            Some(&serde_json::json!("sunset")),
        );
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[tokio::test]
    async fn theme_reset_returns_apply_theme_with_system_default() {
        // Reset = "remove the key, fall back to defaults." For
        // theme that fallback is `system`, so we pre-emptively
        // emit ApplyTheme("system") instead of leaving the running
        // app on the just-removed value
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());

        // Seed a non-default so reset has something to undo
        apply_set("theme", "midnight").unwrap();
        let p = ConfigProvider::default();
        let effect = p
            .activate(&"config::reset::theme".to_string(), "default")
            .await
            .unwrap();
        match effect {
            Effect::ApplyTheme(id) => assert_eq!(id, "system"),
            other => panic!("expected ApplyTheme(system), got {other:?}"),
        }
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[tokio::test]
    async fn non_theme_set_still_returns_notification() {
        // Other keys keep the toast-on-save flow - the apply-theme
        // path is a per-key carve-out, not a wholesale change
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());

        let p = ConfigProvider::default();
        let effect = p
            .activate(
                &"config::set::ai.model::llama3.2".to_string(),
                "default",
            )
            .await
            .unwrap();
        assert!(
            matches!(effect, Effect::Notification { .. }),
            "expected toast for non-theme set, got {effect:?}",
        );
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[tokio::test]
    async fn toggle_activate_updates_gate_live() {
        // Regression: flipping a provider via config UI must
        // update the in-memory disabled set immediately so the
        // registry stops invoking it on next keystroke
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let gate = new_disabled_set();
        let p = ConfigProvider::new(gate.clone(), vec!["note".into()]);
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        let _ = p.activate(
            &"config::set::providers.note.enabled::false".to_string(),
            "default",
        ).await;
        assert!(gate.load().contains("note"), "gate should now hold 'note'");
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[test]
    fn gyors_config_dir_overrides_default_path() {
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Canary: ensures the env override actually redirects writes
        // on every platform - this is test that would have
        // caught the `cargo test`-clobbers-real-config regression
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        assert_eq!(
            config_path(),
            td.path().join("config.json"),
            "GYORS_CONFIG_DIR must win over the platform default"
        );
        apply_set("theme", "midnight").unwrap();
        let written = td.path().join("config.json");
        assert!(written.exists(), "write landed inside the override dir");
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[test]
    fn empty_gyors_config_dir_falls_back_to_default() {
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Empty string mustn't be treated as "use this dir" - would
        // make file land next to `cwd/config.json`
        std::env::set_var("GYORS_CONFIG_DIR", "");
        let path = config_path();
        std::env::remove_var("GYORS_CONFIG_DIR");
        assert!(
            path.to_string_lossy().contains("Gyors"),
            "empty override should fall back through to the Gyors default dir"
        );
    }


    fn write_config_with_repo_roots(td_path: &std::path::Path, raw: &str) {
        let cfg = serde_json::json!({ "repo_roots": raw });
        std::fs::write(
            td_path.join("config.json"),
            serde_json::to_string(&cfg).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn repo_roots_returns_none_when_key_absent() {
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        assert!(repo_roots_pref().is_none());
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[test]
    fn repo_roots_accepts_absolute_and_tilde_paths() {
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        write_config_with_repo_roots(td.path(), "~/Documents, /Users/foo/work, ~");
        let roots = repo_roots_pref().expect("got roots");
        assert_eq!(roots.len(), 3);
        let home = dirs::home_dir().unwrap();
        assert_eq!(roots[0], home.join("Documents"));
        assert_eq!(roots[1], PathBuf::from("/Users/foo/work"));
        assert_eq!(roots[2], home);
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[test]
    fn repo_roots_rejects_bare_names_and_relative_paths() {
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        // First two entries are bare names; third is `./` relative;
        // only the absolute path survives
        write_config_with_repo_roots(
            td.path(),
            "Documents, projects, ./local, /Users/foo/code",
        );
        let roots = repo_roots_pref().expect("at least one survivor");
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], PathBuf::from("/Users/foo/code"));
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[test]
    fn repo_roots_all_invalid_falls_back_to_none() {
        let _guard = CONFIG_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        write_config_with_repo_roots(td.path(), "Documents, projects, dev");
        assert!(
            repo_roots_pref().is_none(),
            "all-invalid list must fall back to default scanner roots"
        );
        std::env::remove_var("GYORS_CONFIG_DIR");
    }


    #[test]
    fn ensure_data_dir_secured_creates_dir_with_0700() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = test_support::CONFIG_PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        // Pre-condition: tempdir already exists (tempfile::tempdir),
        // but its perms are usually 0700 anyway. Force a known
        // non-0700 starting state so test really exercises the
        // chmod step
        std::fs::set_permissions(
            td.path(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let pre_mode = std::fs::metadata(td.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(pre_mode, 0o755, "test setup: expected 0755 before chmod");
        let dir = ensure_data_dir_secured().unwrap();
        assert_eq!(dir, td.path());
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "data dir must end up 0700 after helper");
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[test]
    fn ensure_data_dir_secured_creates_missing_dir() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = test_support::CONFIG_PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        // Point at a path that doesn't yet exist - the helper must
        // create it AND chmod it in one call
        let target = td.path().join("Gyors-not-yet");
        std::env::set_var("GYORS_CONFIG_DIR", &target);
        let dir = ensure_data_dir_secured().unwrap();
        assert_eq!(dir, target);
        assert!(target.exists(), "helper must mkdir -p");
        let mode = std::fs::metadata(&target)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[test]
    fn ensure_data_dir_secured_is_idempotent() {
        // Calling twice in a row must succeed both times. Real
        // launcher code does this from many entry points
        use std::os::unix::fs::PermissionsExt;
        let _guard = test_support::CONFIG_PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        ensure_data_dir_secured().unwrap();
        ensure_data_dir_secured().unwrap();
        let mode =
            std::fs::metadata(td.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[test]
    fn save_config_secures_root_dir_as_side_effect() {
        // save_config calls ensure_data_dir_secured internally so
        // every config write doubles as a chmod re-affirmation.
        // This catches a future contributor who removes call
        use std::os::unix::fs::PermissionsExt;
        let _guard = test_support::CONFIG_PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());
        std::fs::set_permissions(
            td.path(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        save_config(&json!({"theme": "neon"})).unwrap();
        let mode =
            std::fs::metadata(td.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "save_config must tighten the data dir to 0700"
        );
        std::env::remove_var("GYORS_CONFIG_DIR");
    }
}
