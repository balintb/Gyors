//! Snippets - short text fragments user can type a trigger to copy
//!
//! Config lives at `~/Library/Application Support/Gyors/snippets.toml`:
//!
//! ```toml
//! [[snippet]]
//! trigger = "sig"
//! name    = "Email signature"
//! text    = """
//! Your Name
//! you@example.com
//! """
//!
//! [[snippet]]
//! trigger = "greet"
//! text    = "Hi,\n\n"
//! ```
//!
//! Activate by typing:
//! - `snip [filter]` - lists matching snippets
//! - `;trigger` - direct prefix match on trigger
//! - `!trigger` - same
//!
//! Activation copies the snippet text to clipboard

use anyhow::Result;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const RESULT_LIMIT: usize = 20;

#[derive(Debug, Clone, Serialize)]
pub struct Snippet {
    pub trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub text: String,
}

pub struct SnippetsProvider {
    state: Arc<ArcSwap<Vec<Snippet>>>,
    _watcher: Option<RecommendedWatcher>,
}

impl SnippetsProvider {
    pub async fn new() -> Self {
        let initial = tokio::task::spawn_blocking(load_snippets)
            .await
            .unwrap_or_default();
        let state = Arc::new(ArcSwap::from(Arc::new(initial)));
        let watcher = spawn_watcher(Arc::clone(&state)).ok();
        Self { state, _watcher: watcher }
    }

    pub fn len(&self) -> usize {
        self.state.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.state.load().is_empty()
    }

    /// Snapshot of the live snippet set. Used by the Global Snippet
    /// Expander (Swift, CGEventTap) to build its trigger table
    /// without calling into provider on every keystroke
    pub fn snapshot(&self) -> Vec<Snippet> {
        self.state.load().as_ref().clone()
    }
}

#[async_trait]
impl Provider for SnippetsProvider {
    fn id(&self) -> &str {
        "snip"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(filter) = parse_snippet_query(query.pattern()) else {
            return vec![];
        };
        let filter_lower = filter.to_lowercase();
        let snippets = self.state.load();
        snippets
            .iter()
            .filter(|s| filter_lower.is_empty() || matches_filter(s, &filter_lower))
            .take(RESULT_LIMIT)
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> Result<Effect> {
        let trigger = id
            .strip_prefix("snip::")
            .ok_or_else(|| anyhow::anyhow!("invalid snip candidate id: {id}"))?;
        let snippets = self.state.load();
        let snippet = snippets
            .iter()
            .find(|s| s.trigger == trigger)
            .ok_or_else(|| anyhow::anyhow!("unknown snippet trigger: {trigger}"))?;
        Ok(Effect::CopyToClipboard(snippet.text.clone()))
    }
}

/// Parse user input into an effective snippet filter. Returns None
/// when user hasn't invoked the snippets provider
fn parse_snippet_query(pattern: &str) -> Option<String> {
    if let Some(rest) = pattern.strip_prefix("snip ") {
        return Some(rest.trim().to_string());
    }
    if let Some(rest) = pattern.strip_prefix("snippet ") {
        return Some(rest.trim().to_string());
    }
    if pattern == "snip" || pattern == "snippet" || pattern == "snippets" {
        return Some(String::new());
    }
    // `;<word>` / `!<word>` direct trigger shorthand - first char must be
    // alphabetic so we dont grab shell-ish inputs like `;(` or `!1`
    for prefix in &[';', '!'] {
        if let Some(rest) = pattern.strip_prefix(*prefix) {
            if rest.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

fn matches_filter(s: &Snippet, filter_lower: &str) -> bool {
    if s.trigger.to_lowercase().contains(filter_lower) {
        return true;
    }
    s.name
        .as_deref()
        .map(|n| n.to_lowercase().contains(filter_lower))
        .unwrap_or(false)
}

fn to_candidate(s: &Snippet) -> Candidate {
    let label = s.name.clone().unwrap_or_else(|| s.trigger.clone());
    let preview = preview_of(&s.text);
    Candidate {
        id: format!("snip::{}", s.trigger),
        title: label,
        subtitle: Some(format!(":{}:  {preview}", s.trigger)),
        icon: Icon::SfSymbol("text.quote".into()),
        kind: CandidateKind::Snippet,
        actions: vec![Action::primary("Copy to Clipboard")],
        search_text: format!("{} {}", s.trigger, s.name.as_deref().unwrap_or("")),
        bypass_rank: false,
    }
}

fn preview_of(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let truncated: String = first_line.chars().take(60).collect();
    if first_line.chars().count() > 60 || text.lines().count() > 1 {
        format!("{truncated}…")
    } else {
        truncated
    }
}


fn snippets_path() -> Option<PathBuf> {
    let base = dirs::data_local_dir()?.join("Gyors");
    Some(base.join("snippets.toml"))
}

pub fn load_snippets() -> Vec<Snippet> {
    let Some(path) = snippets_path() else {
        return vec![];
    };
    if !path.exists() {
        write_default_template(&path);
        return vec![];
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![];
    };
    match toml::from_str::<SnippetsFile>(&text) {
        Ok(file) => file
            .snippet
            .into_iter()
            .filter(|s| !s.trigger.trim().is_empty() && !s.text.is_empty())
            .map(|s| Snippet { trigger: s.trigger, name: s.name, text: s.text })
            .collect(),
        Err(e) => {
            tracing::warn!("snippets.toml is malformed: {e}");
            vec![]
        }
    }
}

fn write_default_template(path: &Path) {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let template = r#"# Gyors snippets. Edit here; changes are picked up on save.
# Activate via `snip <filter>` or the direct trigger shorthands `;sig` / `!sig`.

[[snippet]]
trigger = "sig"
name    = "Email signature"
text    = """
Your Name
you@example.com
"""

[[snippet]]
trigger = "greet"
name    = "Quick greeting"
text    = "Hi,\n\n"
"#;
    let _ = std::fs::write(path, template);
}

#[derive(Debug, Default, Deserialize)]
struct SnippetsFile {
    #[serde(default)]
    snippet: Vec<SnippetDef>,
}

#[derive(Debug, Deserialize)]
struct SnippetDef {
    trigger: String,
    name: Option<String>,
    text: String,
}

fn spawn_watcher(state: Arc<ArcSwap<Vec<Snippet>>>) -> Result<RecommendedWatcher> {
    let Some(path) = snippets_path() else {
        anyhow::bail!("no snippets path");
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })?;
    if let Some(parent) = path.parent() {
        if parent.exists() {
            let _ = watcher.watch(parent, RecursiveMode::NonRecursive);
        }
    }
    std::thread::spawn(move || loop {
        if rx.recv().is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        let fresh = load_snippets();
        tracing::debug!("snippets reindexed: {} entries", fresh.len());
        state.store(Arc::new(fresh));
    });
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_with(snippets: Vec<Snippet>) -> SnippetsProvider {
        SnippetsProvider {
            state: Arc::new(ArcSwap::from(Arc::new(snippets))),
            _watcher: None,
        }
    }

    fn sample() -> Vec<Snippet> {
        vec![
            Snippet {
                trigger: "sig".into(),
                name: Some("Email signature".into()),
                text: "Your Name\nyou@example.com".into(),
            },
            Snippet {
                trigger: "greet".into(),
                name: Some("Greeting".into()),
                text: "Hi,\n\n".into(),
            },
            Snippet {
                trigger: "todo".into(),
                name: None,
                text: "- [ ] ".into(),
            },
        ]
    }

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = provider_with(sample());
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_all() {
        let p = provider_with(sample());
        assert_eq!(p.query(&Query::new("snip")).await.len(), 3);
        assert_eq!(p.query(&Query::new("snippet")).await.len(), 3);
        assert_eq!(p.query(&Query::new("snippets")).await.len(), 3);
    }

    #[tokio::test]
    async fn filter_matches_trigger() {
        let p = provider_with(sample());
        let out = p.query(&Query::new("snip sig")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "snip::sig");
    }

    #[tokio::test]
    async fn filter_matches_name() {
        let p = provider_with(sample());
        let out = p.query(&Query::new("snip greeting")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "snip::greet");
    }

    #[tokio::test]
    async fn semicolon_shorthand() {
        let p = provider_with(sample());
        let out = p.query(&Query::new(";sig")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "snip::sig");
    }

    #[tokio::test]
    async fn bang_shorthand() {
        let p = provider_with(sample());
        let out = p.query(&Query::new("!greet")).await;
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn shorthand_requires_alphabetic_first_char() {
        let p = provider_with(sample());
        assert!(p.query(&Query::new(";1")).await.is_empty());
        assert!(p.query(&Query::new(";(")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_copies_text() {
        let p = provider_with(sample());
        let effect = p.activate(&"snip::sig".to_string(), "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert!(s.contains("Your Name")),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_unknown_trigger_errors() {
        let p = provider_with(sample());
        assert!(p.activate(&"snip::missing".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = provider_with(sample());
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn parse_snippet_query_variants() {
        assert_eq!(parse_snippet_query("snip"), Some(String::new()));
        assert_eq!(parse_snippet_query("snip foo"), Some("foo".into()));
        assert_eq!(parse_snippet_query(";sig"), Some("sig".into()));
        assert_eq!(parse_snippet_query("!sig"), Some("sig".into()));
        assert_eq!(parse_snippet_query(";1"), None);
        assert_eq!(parse_snippet_query("hello"), None);
        assert_eq!(parse_snippet_query(""), None);
    }

    #[test]
    fn preview_trims_and_truncates() {
        assert_eq!(preview_of("hi"), "hi");
        assert_eq!(preview_of("first line\nsecond"), "first line…");
        let long = "a".repeat(100);
        assert!(preview_of(&long).ends_with("…"));
    }
}
