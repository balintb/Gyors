//! Screenshot provider - wraps macOS's `screencapture`
//!
//!   screen              -> interactive region to ~/Desktop
//!   screen clip / clipboard -> interactive region to clipboard
//!   screen win / window -> interactive window capture to ~/Desktop
//!   screen full / screen   -> full-screen capture to ~/Desktop

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct ScreenshotProvider;

#[derive(Debug, Clone, Copy)]
struct ShotCommand {
    id: &'static str,
    title: &'static str,
    subtitle: &'static str,
    keywords: &'static str,
    /// Screencapture invocation (single shell line)
    shell: &'static str,
}

const COMMANDS: &[ShotCommand] = &[
    ShotCommand {
        id: "region",
        title: "Screenshot: Region",
        subtitle: "Drag a region (saves to ~/Desktop)",
        keywords: "screenshot region area select drag",
        shell: "screencapture -i ~/Desktop/\"Screen Shot $(date +%Y-%m-%d\\ at\\ %H.%M.%S).png\"",
    },
    ShotCommand {
        id: "region-clipboard",
        title: "Screenshot: Region → Clipboard",
        subtitle: "Drag a region, copy to clipboard",
        keywords: "screenshot region clipboard copy paste",
        shell: "screencapture -ci",
    },
    ShotCommand {
        id: "window",
        title: "Screenshot: Window",
        subtitle: "Click a window (saves to ~/Desktop)",
        keywords: "screenshot window app",
        shell: "screencapture -iW ~/Desktop/\"Screen Shot $(date +%Y-%m-%d\\ at\\ %H.%M.%S).png\"",
    },
    ShotCommand {
        id: "window-clipboard",
        title: "Screenshot: Window → Clipboard",
        subtitle: "Click a window, copy to clipboard",
        keywords: "screenshot window clipboard",
        shell: "screencapture -ciW",
    },
    ShotCommand {
        id: "full",
        title: "Screenshot: Full Screen",
        subtitle: "Capture the entire screen (saves to ~/Desktop)",
        keywords: "screenshot full screen",
        shell: "screencapture ~/Desktop/\"Screen Shot $(date +%Y-%m-%d\\ at\\ %H.%M.%S).png\"",
    },
];

#[async_trait]
impl Provider for ScreenshotProvider {
    fn id(&self) -> &str {
        "shot"
    }

    async fn query(&self, _query: &Query) -> Vec<Candidate> {
        COMMANDS.iter().map(to_candidate).collect()
    }

    async fn activate(&self, candidate_id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let cmd_id = candidate_id
            .strip_prefix("shot::")
            .ok_or_else(|| anyhow::anyhow!("invalid shot candidate id: {candidate_id}"))?;
        let cmd = COMMANDS
            .iter()
            .find(|c| c.id == cmd_id)
            .ok_or_else(|| anyhow::anyhow!("unknown screenshot command: {cmd_id}"))?;
        Ok(Effect::RunShell(cmd.shell.to_string()))
    }
}

fn to_candidate(cmd: &ShotCommand) -> Candidate {
    Candidate {
        id: format!("shot::{}", cmd.id),
        title: cmd.title.to_string(),
        subtitle: Some(cmd.subtitle.to_string()),
        icon: Icon::SfSymbol("camera.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Capture")],
        search_text: format!("{} {}", cmd.title, cmd.keywords),
        bypass_rank: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn query_returns_all_commands() {
        let p = ScreenshotProvider;
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), COMMANDS.len());
    }

    #[tokio::test]
    async fn candidates_include_screenshot_in_search_text() {
        let p = ScreenshotProvider;
        let out = p.query(&Query::new("")).await;
        for c in out {
            assert!(c.search_text.contains("screenshot"));
        }
    }

    #[tokio::test]
    async fn activate_region_yields_screencapture_shell() {
        let p = ScreenshotProvider;
        let effect = p
            .activate(&"shot::region".to_string(), "default")
            .await
            .unwrap();
        match effect {
            Effect::RunShell(s) => {
                assert!(s.contains("screencapture"));
                assert!(s.contains("-i"));
            }
            other => panic!("expected RunShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_region_clipboard_uses_c_flag() {
        let p = ScreenshotProvider;
        let effect = p
            .activate(&"shot::region-clipboard".to_string(), "default")
            .await
            .unwrap();
        if let Effect::RunShell(s) = effect {
            assert!(s.contains("-ci"));
        }
    }

    #[tokio::test]
    async fn activate_window_uses_w_flag() {
        let p = ScreenshotProvider;
        let effect = p
            .activate(&"shot::window".to_string(), "default")
            .await
            .unwrap();
        if let Effect::RunShell(s) = effect {
            assert!(s.contains("-iW"));
        }
    }

    #[tokio::test]
    async fn activate_unknown_command_errors() {
        let p = ScreenshotProvider;
        assert!(p
            .activate(&"shot::bogus".to_string(), "default")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = ScreenshotProvider;
        assert!(p
            .activate(&"apps::Safari".to_string(), "default")
            .await
            .is_err());
    }

    #[test]
    fn command_ids_unique() {
        let mut ids: Vec<&str> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before);
    }
}
