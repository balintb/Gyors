//! `plugins` launcher command
//!
//! Mirrors the shape of `ConfigProvider` - type `plugins` to see
//! every installed plugin, with per-row actions for Edit / Reveal /
//! Remove and header rows for opening `plugins.json` or the
//! process-plugin directory
//!
//! Why a separate provider (rather than rows inside ConfigProvider):
//! Plugins carry their own metadata (version, source_url, author)
//! and need richer actions than `config` offers. Splitting them
//! keeps `config` laser-focused on key/value pairs

use anyhow::Result;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use gyors_plugin_host::ShellPluginSpec;
use std::path::PathBuf;

pub struct PluginsProvider {
    /// Path to plugins.json. Held for action dispatch ("reveal",
    /// "edit", "remove") so Provider itself doesn't need to
    /// re-resolve `GYORS_CONFIG_DIR` on every activate call
    plugins_json: PathBuf,
    /// Path to the process-plugin directory. Likewise cached
    process_plugin_dir: PathBuf,
}

impl Default for PluginsProvider {
    fn default() -> Self {
        Self {
            plugins_json: gyors_plugin_host::default_shell_plugins_path(),
            process_plugin_dir: gyors_plugin_host::default_plugin_dir(),
        }
    }
}

impl PluginsProvider {
    pub fn new() -> Self {
        Self::default()
    }
}

const KEYWORDS: &[&str] = &["plugins", "plugin"];

fn strip_keyword(s: &str) -> Option<&str> {
    let trimmed = s.trim();
    for kw in KEYWORDS {
        if trimmed.eq_ignore_ascii_case(kw) {
            return Some("");
        }
        if trimmed.len() > kw.len() {
            let (head, rest) = trimmed.split_at(kw.len());
            if head.eq_ignore_ascii_case(kw) && rest.starts_with(char::is_whitespace) {
                return Some(rest.trim_start());
            }
        }
    }
    None
}

#[async_trait]
impl Provider for PluginsProvider {
    fn id(&self) -> &str {
        "plugins_ui"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(filter) = strip_keyword(query.pattern()) else {
            return Vec::new();
        };
        let filter_lower = filter.to_lowercase();
        let mut out: Vec<Candidate> = Vec::new();

        // Header rows - always present so discovery path is
        // always one keystroke away. Bypass-rank so they dont get
        // fuzzy-ranked below plugin entries
        out.push(header_install_url());
        out.push(header_open_plugins_json(&self.plugins_json));
        out.push(header_open_plugin_dir(&self.process_plugin_dir));

        // Shell plugins from plugins.json
        let shell = gyors_plugin_host::load_shell_plugins(&self.plugins_json);
        for plugin in shell {
            if !filter_lower.is_empty() && !plugin_matches_filter(&plugin.spec, &filter_lower) {
                continue;
            }
            out.push(shell_plugin_row(&plugin.spec, &self.plugins_json));
        }

        // Process plugins - discovered same way registry
        // does, so UI and routing agree on what exists
        let process_plugins = gyors_plugin_host::discover(&self.process_plugin_dir).await;
        for plugin in process_plugins {
            if !filter_lower.is_empty()
                && !plugin.info.id.to_lowercase().contains(&filter_lower)
                && !plugin.info.name.to_lowercase().contains(&filter_lower)
            {
                continue;
            }
            out.push(process_plugin_row(&plugin, &self.process_plugin_dir));
        }

        out
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        if id == "plugins_ui::install-url" {
            // Walk user into the install flow: set input
            // to a helpful hint that describes what they need to
            // paste. Actual install happens when they paste
            // a gyors://plugin/install URL anywhere else (URL
            // handler fires automatically)
            return Ok(Effect::OpenUrl(
                "https://github.com/balintb/Gyors".into(),
            ));
        }
        if id == "plugins_ui::open-plugins-json" {
            return Ok(Effect::OpenPath(self.plugins_json.clone()));
        }
        if id == "plugins_ui::open-plugin-dir" {
            return Ok(Effect::OpenPath(self.process_plugin_dir.clone()));
        }
        // Shell plugin rows: "plugins_ui::shell::<id>"
        if let Some(plug_id) = id.strip_prefix("plugins_ui::shell::") {
            return Ok(match action {
                "remove" => {
                    // Remove from plugins.json. Changes take effect
                    // on next launch; we surface a notification so
                    // user knows it's queued
                    let removed =
                        gyors_plugin_host::remove_shell_plugin(&self.plugins_json, plug_id)
                            .unwrap_or(false);
                    if removed {
                        Effect::Notification {
                            title: format!("Removed plugin `{plug_id}`"),
                            body: Some("Restart Gyors to fully unload.".into()),
                        }
                    } else {
                        Effect::None
                    }
                }
                "source" => {
                    let spec = gyors_plugin_host::find_shell_plugin(&self.plugins_json, plug_id);
                    if let Some(url) = spec.and_then(|s| s.source_url) {
                        Effect::OpenUrl(url)
                    } else {
                        Effect::Notification {
                            title: "No source URL".into(),
                            body: Some("This plugin doesn't declare a homepage.".into()),
                        }
                    }
                }
                // Default / edit -> jump into plugins.json so the
                // user can tweak spec directly
                _ => Effect::OpenPath(self.plugins_json.clone()),
            });
        }
        // Process plugin rows: "plugins_ui::process::<id>"
        if let Some(plug_id) = id.strip_prefix("plugins_ui::process::") {
            return Ok(match action {
                "reveal" => {
                    // The `RevealInFinder` effect expects a file
                    // path; reconstruct plugin's path inside
                    // the process plugin dir
                    let path = self.process_plugin_dir.join(plug_id);
                    Effect::RevealInFinder(path)
                }
                _ => Effect::OpenPath(self.process_plugin_dir.clone()),
            });
        }
        anyhow::bail!("unknown plugins_ui id: {id}")
    }
}

fn plugin_matches_filter(spec: &ShellPluginSpec, filter_lower: &str) -> bool {
    spec.id.to_lowercase().contains(filter_lower)
        || spec.name.to_lowercase().contains(filter_lower)
        || spec
            .keywords
            .iter()
            .any(|k| k.to_lowercase().contains(filter_lower))
}


fn header_install_url() -> Candidate {
    Candidate {
        id: "plugins_ui::install-url".into(),
        title: "Install plugin from URL…".into(),
        subtitle: Some(
            "Click a gyors://plugin/install… link anywhere, or open a .gyorsplugin file"
                .into(),
        ),
        icon: Icon::SfSymbol("square.and.arrow.down".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Open docs")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn header_open_plugins_json(path: &std::path::Path) -> Candidate {
    Candidate {
        id: "plugins_ui::open-plugins-json".into(),
        title: "Open plugins.json".into(),
        subtitle: Some(path.display().to_string()),
        icon: Icon::SfSymbol("doc.text".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Open")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn header_open_plugin_dir(path: &std::path::Path) -> Candidate {
    Candidate {
        id: "plugins_ui::open-plugin-dir".into(),
        title: "Open plugins folder".into(),
        subtitle: Some(path.display().to_string()),
        icon: Icon::SfSymbol("folder".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Open")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn shell_plugin_row(spec: &ShellPluginSpec, _plugins_json: &std::path::Path) -> Candidate {
    let icon_name = spec
        .icon
        .clone()
        .unwrap_or_else(|| "square.grid.2x2".into());
    let version_badge = spec
        .version
        .as_deref()
        .map(|v| format!(" v{v}"))
        .unwrap_or_default();
    let subtitle = {
        let keywords_pretty = spec.keywords.join(" · ");
        let author = spec
            .author
            .as_deref()
            .map(|a| format!(" · {a}"))
            .unwrap_or_default();
        let kind_tag = "shell";
        let source = if spec.source_url.is_some() {
            " · ↗ source"
        } else {
            ""
        };
        format!("{kind_tag} · {keywords_pretty}{author}{source}")
    };
    let mut actions = vec![
        Action::primary("Edit"),
        Action::new("remove", "Remove"),
    ];
    if spec.source_url.is_some() {
        actions.push(Action::new("source", "Open Source URL"));
    }
    Candidate {
        id: format!("plugins_ui::shell::{}", spec.id),
        title: format!("{}{version_badge}", spec.name),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol(icon_name),
        kind: CandidateKind::Custom("plugin".into()),
        actions,
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn process_plugin_row(
    plugin: &gyors_plugin_host::Plugin,
    _dir: &std::path::Path,
) -> Candidate {
    let kw = plugin.info.keywords.join(" · ");
    Candidate {
        id: format!("plugins_ui::process::{}", plugin.info.id),
        title: plugin.info.name.clone(),
        subtitle: Some(format!("process · {kw}")),
        icon: Icon::SfSymbol("terminal".into()),
        kind: CandidateKind::Custom("plugin".into()),
        actions: vec![
            Action::primary("Reveal in Finder"),
            Action::new("reveal", "Reveal in Finder"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gyors_plugin_host::upsert_shell_plugin;

    fn sample_spec(id: &str, keywords: &[&str]) -> ShellPluginSpec {
        ShellPluginSpec {
            id: id.into(),
            name: format!("{id} plugin"),
            description: String::new(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            command: "echo x".into(),
            on_activate: gyors_plugin_host::ShellActivation::Copy,
            icon: None,
            timeout_ms: 2000,
            version: Some("1.0.0".into()),
            source_url: Some("https://example.com/p".into()),
            author: Some("author".into()),
        }
    }

    fn env_with_plugins(specs: Vec<ShellPluginSpec>) -> (tempfile::TempDir, PluginsProvider) {
        let td = tempfile::tempdir().unwrap();
        let plugins_json = td.path().join("plugins.json");
        let plugin_dir = td.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        for s in specs {
            upsert_shell_plugin(&plugins_json, s).unwrap();
        }
        let provider = PluginsProvider {
            plugins_json,
            process_plugin_dir: plugin_dir,
        };
        (td, provider)
    }

    #[tokio::test]
    async fn strip_keyword_variants() {
        assert_eq!(strip_keyword("plugins"), Some(""));
        assert_eq!(strip_keyword("plugin"), Some(""));
        assert_eq!(strip_keyword("PLUGINS foo"), Some("foo"));
        assert_eq!(strip_keyword("plugins   weather"), Some("weather"));
        assert_eq!(strip_keyword("pluginish"), None);
        assert_eq!(strip_keyword("config"), None);
    }

    #[tokio::test]
    async fn bare_keyword_emits_headers_and_every_plugin() {
        let (_td, p) = env_with_plugins(vec![
            sample_spec("alpha", &["a"]),
            sample_spec("beta", &["b"]),
        ]);
        let rows = p.query(&Query::new("plugins")).await;
        let ids: Vec<&str> = rows.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"plugins_ui::install-url"));
        assert!(ids.contains(&"plugins_ui::open-plugins-json"));
        assert!(ids.contains(&"plugins_ui::open-plugin-dir"));
        assert!(ids.contains(&"plugins_ui::shell::alpha"));
        assert!(ids.contains(&"plugins_ui::shell::beta"));
    }

    #[tokio::test]
    async fn filter_narrows_by_id_name_or_keyword() {
        let (_td, p) = env_with_plugins(vec![
            sample_spec("weather", &["w", "wttr"]),
            sample_spec("fuck", &["fix"]),
        ]);
        let rows = p.query(&Query::new("plugins weather")).await;
        // Headers always pass (bypass_rank, always-on). Only the
        // weather plugin row should match among plugin entries
        let plugin_rows: Vec<&str> = rows
            .iter()
            .filter(|c| c.id.starts_with("plugins_ui::shell::"))
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(plugin_rows, vec!["plugins_ui::shell::weather"]);

        // Filter by keyword
        let rows2 = p.query(&Query::new("plugins wttr")).await;
        let by_kw: Vec<&str> = rows2
            .iter()
            .filter(|c| c.id.starts_with("plugins_ui::shell::"))
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(by_kw, vec!["plugins_ui::shell::weather"]);
    }

    #[tokio::test]
    async fn activate_remove_deletes_from_plugins_json() {
        let (_td, p) = env_with_plugins(vec![
            sample_spec("gone", &["g"]),
            sample_spec("stays", &["s"]),
        ]);
        let eff = p
            .activate(&"plugins_ui::shell::gone".to_string(), "remove")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::Notification { .. }));
        let remaining = gyors_plugin_host::load_shell_plugins(&p.plugins_json);
        let ids: Vec<&str> = remaining.iter().map(|s| s.spec.id.as_str()).collect();
        assert_eq!(ids, vec!["stays"]);
    }

    #[tokio::test]
    async fn activate_source_opens_url_when_present() {
        let (_td, p) = env_with_plugins(vec![sample_spec("withurl", &["u"])]);
        let eff = p
            .activate(&"plugins_ui::shell::withurl".to_string(), "source")
            .await
            .unwrap();
        match eff {
            Effect::OpenUrl(u) => assert_eq!(u, "https://example.com/p"),
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_source_notifies_when_missing() {
        let mut s = sample_spec("nourl", &["n"]);
        s.source_url = None;
        let (_td, p) = env_with_plugins(vec![s]);
        let eff = p
            .activate(&"plugins_ui::shell::nourl".to_string(), "source")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::Notification { .. }));
    }

    #[tokio::test]
    async fn activate_default_opens_plugins_json() {
        let (_td, p) = env_with_plugins(vec![sample_spec("xx", &["x"])]);
        let eff = p
            .activate(&"plugins_ui::shell::xx".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::OpenPath(path) => assert_eq!(path, p.plugins_json),
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_header_rows() {
        let (_td, p) = env_with_plugins(vec![]);
        let eff = p
            .activate(&"plugins_ui::open-plugins-json".to_string(), "default")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::OpenPath(_)));
        let eff = p
            .activate(&"plugins_ui::open-plugin-dir".to_string(), "default")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::OpenPath(_)));
    }
}
