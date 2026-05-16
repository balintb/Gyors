//! MacOS Shortcuts integration - list and run Shortcuts defined in the
//! system Shortcuts.app
//!
//! Scans at startup via `shortcuts list` (macOS 12+). Activation runs
//! `shortcuts run "<name>"`. Results are fuzzy-filtered by name

use arc_swap::ArcSwap;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::sync::Arc;

const RESULT_LIMIT: usize = 20;

#[derive(Debug, Clone)]
pub struct Shortcut {
    pub name: String,
}

pub struct ShortcutsProvider {
    state: Arc<ArcSwap<Vec<Shortcut>>>,
}

impl ShortcutsProvider {
    /// Returns immediately with an empty shortcut set; the
    /// `shortcuts list` subprocess (~100ms cold) runs on a background
    /// blocking task and parsed list is `ArcSwap`-stored into
    /// `state` when ready. The `shortcut` keyword returns no rows
    /// during that window, which is fine - by the time user has
    /// typed prefix, the subprocess has long since returned
    pub fn new() -> Self {
        let state = Arc::new(ArcSwap::from(Arc::new(Vec::<Shortcut>::new())));
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            if let Ok(scanned) = tokio::task::spawn_blocking(load_shortcuts).await {
                st.store(Arc::new(scanned));
            }
        });
        Self { state }
    }

    pub fn len(&self) -> usize {
        self.state.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.state.load().is_empty()
    }
}

impl Default for ShortcutsProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ShortcutsProvider {
    fn id(&self) -> &str {
        "shortcut"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(filter) = parse_shortcuts_query(query.pattern()) else {
            return vec![];
        };
        let filter_lower = filter.to_lowercase();
        let shortcuts = self.state.load();
        shortcuts
            .iter()
            .filter(|s| filter_lower.is_empty() || s.name.to_lowercase().contains(&filter_lower))
            .take(RESULT_LIMIT)
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let name = id
            .strip_prefix("shortcut::")
            .ok_or_else(|| anyhow::anyhow!("invalid shortcut candidate id: {id}"))?;
        let quoted = shell_quote(name);
        Ok(Effect::RunShell(format!("shortcuts run {quoted}")))
    }
}

fn parse_shortcuts_query(pattern: &str) -> Option<String> {
    if let Some(rest) = pattern.strip_prefix("shortcut ") {
        return Some(rest.trim().to_string());
    }
    if let Some(rest) = pattern.strip_prefix("sc ") {
        return Some(rest.trim().to_string());
    }
    if pattern == "shortcut" || pattern == "shortcuts" || pattern == "sc" {
        return Some(String::new());
    }
    None
}

fn to_candidate(s: &Shortcut) -> Candidate {
    Candidate {
        id: format!("shortcut::{}", s.name),
        title: s.name.clone(),
        subtitle: Some("Run macOS Shortcut".into()),
        icon: Icon::SfSymbol("bolt.horizontal.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Run")],
        search_text: s.name.clone(),
        bypass_rank: false,
    }
}

fn load_shortcuts() -> Vec<Shortcut> {
    let output = match std::process::Command::new("shortcuts").args(["list"]).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!("shortcuts CLI unavailable: {e}");
            return vec![];
        }
    };
    if !output.status.success() {
        return vec![];
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut names: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect();
    names.sort_by_key(|a| a.to_lowercase());
    names.dedup();
    names.into_iter().map(|name| Shortcut { name }).collect()
}

fn shell_quote(s: &str) -> String {
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_with(shortcuts: Vec<Shortcut>) -> ShortcutsProvider {
        ShortcutsProvider {
            state: Arc::new(ArcSwap::from(Arc::new(shortcuts))),
        }
    }

    fn sample() -> Vec<Shortcut> {
        vec![
            Shortcut { name: "Open Downloads".into() },
            Shortcut { name: "Toggle Dark Mode".into() },
            Shortcut { name: "Clear Downloads".into() },
        ]
    }

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = provider_with(sample());
        assert!(p.query(&Query::new("Open Downloads")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_all() {
        let p = provider_with(sample());
        assert_eq!(p.query(&Query::new("shortcut")).await.len(), 3);
        assert_eq!(p.query(&Query::new("sc")).await.len(), 3);
        assert_eq!(p.query(&Query::new("shortcuts")).await.len(), 3);
    }

    #[tokio::test]
    async fn filter_case_insensitive() {
        let p = provider_with(sample());
        let out = p.query(&Query::new("sc DOWNLOADS")).await;
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn activate_runs_shortcuts_cli() {
        let p = provider_with(sample());
        let effect = p
            .activate(&"shortcut::Open Downloads".to_string(), "default")
            .await
            .unwrap();
        match effect {
            Effect::RunShell(cmd) => {
                assert!(cmd.contains("shortcuts run"));
                assert!(cmd.contains("Open Downloads"));
            }
            other => panic!("expected RunShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_escapes_single_quotes_in_name() {
        let p = provider_with(vec![Shortcut { name: "What's cooking".into() }]);
        let effect = p
            .activate(&"shortcut::What's cooking".to_string(), "default")
            .await
            .unwrap();
        if let Effect::RunShell(cmd) = effect {
            // Must be safe for /bin/sh: the single-quote inside the name is
            // escaped via '\'' wrapping
            assert!(cmd.contains(r"'\''"), "got {cmd}");
        } else {
            panic!("expected RunShell");
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = provider_with(sample());
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn parse_shortcuts_query_variants() {
        assert_eq!(parse_shortcuts_query("shortcut"), Some(String::new()));
        assert_eq!(parse_shortcuts_query("sc"), Some(String::new()));
        assert_eq!(parse_shortcuts_query("shortcut foo"), Some("foo".into()));
        assert_eq!(parse_shortcuts_query("sc Downloads"), Some("Downloads".into()));
        assert_eq!(parse_shortcuts_query("scratchy"), None);
        assert_eq!(parse_shortcuts_query(""), None);
    }

    #[test]
    fn shell_quote_cases() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("with space"), "'with space'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
