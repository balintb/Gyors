//! Window management - move / resize the current frontmost window
//!
//! Emits `Effect::ArrangeWindow(geometry_id)` which Swift shell
//! handles via the Accessibility API (`AXUIElement`). This avoids the
//! Automation (AppleEvent) TCC prompt that `tell application "System
//! Events" ...` would require - only one permission (Accessibility) is
//! needed

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct WindowManagementProvider;

#[derive(Debug, Clone, Copy)]
struct WmCommand {
    id: &'static str,
    title: &'static str,
    subtitle: &'static str,
    keywords: &'static str,
    symbol: &'static str,
}

const COMMANDS: &[WmCommand] = &[
    WmCommand { id: "left-half",    title: "Window: Left Half",            subtitle: "Resize to left half of screen",    keywords: "window left half wm",                   symbol: "rectangle.lefthalf.filled" },
    WmCommand { id: "right-half",   title: "Window: Right Half",           subtitle: "Resize to right half of screen",   keywords: "window right half wm",                  symbol: "rectangle.righthalf.filled" },
    WmCommand { id: "top-half",     title: "Window: Top Half",             subtitle: "Resize to top half of screen",     keywords: "window top half wm",                    symbol: "rectangle.tophalf.filled" },
    WmCommand { id: "bottom-half",  title: "Window: Bottom Half",          subtitle: "Resize to bottom half of screen",  keywords: "window bottom half wm",                 symbol: "rectangle.bottomhalf.filled" },
    WmCommand { id: "full",         title: "Window: Maximize",             subtitle: "Fill the visible screen",          keywords: "window full maximize wm",               symbol: "rectangle.fill" },
    WmCommand { id: "center",       title: "Window: Center",               subtitle: "Center on screen (60% size)",      keywords: "window center middle wm",               symbol: "rectangle.center.inset.filled" },
    WmCommand { id: "left-third",   title: "Window: Left Third",           subtitle: "Left third of screen",             keywords: "window left third wm",                  symbol: "rectangle.leftthird.inset.filled" },
    WmCommand { id: "center-third", title: "Window: Center Third",         subtitle: "Center third of screen",           keywords: "window center third wm",                symbol: "rectangle.center.inset.filled" },
    WmCommand { id: "right-third",  title: "Window: Right Third",          subtitle: "Right third of screen",            keywords: "window right third wm",                 symbol: "rectangle.rightthird.inset.filled" },
    WmCommand { id: "top-left",     title: "Window: Top-Left Quarter",     subtitle: "Top-left quarter of screen",       keywords: "window corner top left quarter wm",     symbol: "square.fill" },
    WmCommand { id: "top-right",    title: "Window: Top-Right Quarter",    subtitle: "Top-right quarter of screen",      keywords: "window corner top right quarter wm",    symbol: "square.fill" },
    WmCommand { id: "bottom-left",  title: "Window: Bottom-Left Quarter",  subtitle: "Bottom-left quarter of screen",    keywords: "window corner bottom left quarter wm",  symbol: "square.fill" },
    WmCommand { id: "bottom-right", title: "Window: Bottom-Right Quarter", subtitle: "Bottom-right quarter of screen",   keywords: "window corner bottom right quarter wm", symbol: "square.fill" },
];

impl WindowManagementProvider {
    /// Internal helper - yields the full command list. Lets us keep the
    /// tests and future re-enabling logic without changing any glue
    pub fn all_candidates() -> Vec<Candidate> {
        COMMANDS.iter().map(to_candidate).collect()
    }
}

#[async_trait]
impl Provider for WindowManagementProvider {
    fn id(&self) -> &str {
        "wm"
    }

    async fn query(&self, _query: &Query) -> Vec<Candidate> {
        // Emit every wm command and let orchestrator's fuzzy
        // ranker pick the matches. The `search_text` of each row
        // bundles title plus keywords (e.g. "Window: Left Half
        // window left half wm") so typing `wm`, `window`, `half`,
        // `left`, etc. all rank these rows
        //
        // Previous body returned `vec![]` because an
        // AX-focused-app race was occasionally surfacing the
        // commands without a target window. That race lives on the
        // ACTIVATION side (`Effect::ArrangeWindow` -> AXUIElement
        // ops in Swift shell) and is independently handled
        // there with retries; query path is safe to enable
        Self::all_candidates()
    }

    async fn activate(&self, candidate_id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let cmd_id = candidate_id
            .strip_prefix("wm::")
            .ok_or_else(|| anyhow::anyhow!("invalid wm candidate id: {candidate_id}"))?;
        let cmd = COMMANDS
            .iter()
            .find(|c| c.id == cmd_id)
            .ok_or_else(|| anyhow::anyhow!("unknown window command: {cmd_id}"))?;
        Ok(Effect::ArrangeWindow(cmd.id.to_string()))
    }
}

fn to_candidate(cmd: &WmCommand) -> Candidate {
    Candidate {
        id: format!("wm::{}", cmd.id),
        title: cmd.title.to_string(),
        subtitle: Some(cmd.subtitle.to_string()),
        icon: Icon::SfSymbol(cmd.symbol.to_string()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Apply")],
        search_text: format!("{} {}", cmd.title, cmd.keywords),
        bypass_rank: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn query_yields_every_command() {
        // Re-enabled 2026-05-13: provider emits all 13 commands
        // and orchestrator's fuzzy ranker handles relevance.
        // Without this user typing `wm` (or `window`, `half`,
        // `left`, etc.) sees zero rows - the palette pretends the
        // feature doesn't exist
        let p = WindowManagementProvider;
        let out = p.query(&Query::new("wm")).await;
        assert_eq!(out.len(), COMMANDS.len());
        assert!(out.iter().all(|c| c.id.starts_with("wm::")));
    }

    #[tokio::test]
    async fn query_search_text_includes_wm_keyword_so_fuzzy_ranker_hits() {
        // Regression for actual user-visible bug: `wm` typed
        // alone must match these rows. Orchestrator filters
        // by `search_text`; if `wm` isn't in that field, the
        // fuzzy ranker won't surface rows for the bare `wm`
        // query
        let out = WindowManagementProvider.query(&Query::new("wm")).await;
        for c in &out {
            assert!(
                c.search_text.contains("wm"),
                "search_text for {} missing `wm` token: {:?}",
                c.id,
                c.search_text,
            );
        }
    }

    #[test]
    fn internal_candidate_builder_yields_all_commands() {
        let all = WindowManagementProvider::all_candidates();
        assert_eq!(all.len(), COMMANDS.len());
        for c in all {
            assert_eq!(c.kind, CandidateKind::Action);
            assert!(c.id.starts_with("wm::"));
        }
    }

    #[tokio::test]
    async fn activate_yields_arrange_window_with_geometry_id() {
        let p = WindowManagementProvider;
        let effect = p.activate(&"wm::left-half".to_string(), "default").await.unwrap();
        match effect {
            Effect::ArrangeWindow(id) => assert_eq!(id, "left-half"),
            other => panic!("expected ArrangeWindow, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_unknown_command_errors() {
        let p = WindowManagementProvider;
        assert!(p.activate(&"wm::bogus".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = WindowManagementProvider;
        assert!(p.activate(&"apps::Safari".to_string(), "default").await.is_err());
    }

    #[test]
    fn command_ids_unique() {
        let mut ids: Vec<&str> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before);
    }

    #[test]
    fn all_expected_geometries_present() {
        let ids: Vec<&str> = COMMANDS.iter().map(|c| c.id).collect();
        for expected in [
            "left-half", "right-half", "top-half", "bottom-half",
            "full", "center",
            "left-third", "center-third", "right-third",
            "top-left", "top-right", "bottom-left", "bottom-right",
        ] {
            assert!(ids.contains(&expected), "missing geometry: {expected}");
        }
    }
}
