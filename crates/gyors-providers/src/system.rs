//! System commands provider - Lock, Sleep, Empty Trash, Restart, Shut Down,
//! Log Out, Activity Monitor. All commands are safe: destructive ones
//! (Restart / Shut Down / Log Out) use AppleScript which prompts user
//! before actually doing anything

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct SystemProvider;

#[async_trait]
impl Provider for SystemProvider {
    fn id(&self) -> &str {
        "sys"
    }

    async fn query(&self, _query: &Query) -> Vec<Candidate> {
        COMMANDS.iter().map(to_candidate).collect()
    }

    async fn activate(&self, candidate_id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let cmd_id = candidate_id
            .strip_prefix("sys::")
            .ok_or_else(|| anyhow::anyhow!("invalid sys candidate id: {candidate_id}"))?;
        let cmd = COMMANDS
            .iter()
            .find(|c| c.id == cmd_id)
            .ok_or_else(|| anyhow::anyhow!("unknown system command: {cmd_id}"))?;
        Ok(match &cmd.action {
            SystemAction::Shell(s) => Effect::RunShell((*s).into()),
            SystemAction::AppleScript(s) => Effect::RunAppleScript((*s).into()),
            SystemAction::EmptyTrash => Effect::EmptyTrash,
        })
    }
}

fn to_candidate(cmd: &SystemCommand) -> Candidate {
    Candidate {
        id: format!("sys::{}", cmd.id),
        title: cmd.title.to_string(),
        subtitle: Some(cmd.subtitle.to_string()),
        icon: Icon::SfSymbol(cmd.symbol.to_string()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Run")],
        search_text: format!("{} {}", cmd.title, cmd.keywords),
        bypass_rank: false,
    }
}

#[derive(Debug, Clone, Copy)]
struct SystemCommand {
    id: &'static str,
    title: &'static str,
    subtitle: &'static str,
    keywords: &'static str,
    symbol: &'static str,
    action: SystemAction,
}

#[derive(Debug, Clone, Copy)]
enum SystemAction {
    Shell(&'static str),
    AppleScript(&'static str),
    EmptyTrash,
}

const COMMANDS: &[SystemCommand] = &[
    SystemCommand {
        id: "lock-screen",
        title: "Lock Screen",
        subtitle: "Turn off the display and require a password to unlock",
        keywords: "lock screen secure password",
        symbol: "lock.fill",
        action: SystemAction::Shell("pmset displaysleepnow"),
    },
    SystemCommand {
        id: "sleep",
        title: "Sleep",
        subtitle: "Put the Mac to sleep",
        keywords: "sleep suspend",
        symbol: "moon.zzz.fill",
        action: SystemAction::Shell("pmset sleepnow"),
    },
    SystemCommand {
        id: "empty-trash",
        title: "Empty Trash",
        subtitle: "Permanently delete items in the Trash",
        keywords: "empty trash bin delete clean",
        symbol: "trash.fill",
        action: SystemAction::EmptyTrash,
    },
    SystemCommand {
        id: "restart",
        title: "Restart…",
        subtitle: "Restart the computer (with confirmation)",
        keywords: "restart reboot",
        symbol: "arrow.clockwise.circle.fill",
        action: SystemAction::AppleScript(r#"tell application "System Events" to restart"#),
    },
    SystemCommand {
        id: "shutdown",
        title: "Shut Down…",
        subtitle: "Shut down the computer (with confirmation)",
        keywords: "shutdown shut down power off",
        symbol: "power.circle.fill",
        action: SystemAction::AppleScript(r#"tell application "System Events" to shut down"#),
    },
    SystemCommand {
        id: "logout",
        title: "Log Out…",
        subtitle: "Log out the current user (with confirmation)",
        keywords: "log out logout sign out",
        symbol: "rectangle.portrait.and.arrow.right.fill",
        action: SystemAction::AppleScript(r#"tell application "System Events" to log out"#),
    },
    SystemCommand {
        id: "activity-monitor",
        title: "Activity Monitor",
        subtitle: "Open Activity Monitor (to force quit apps)",
        keywords: "activity monitor force quit kill process",
        symbol: "chart.xyaxis.line",
        action: SystemAction::Shell(r#"open -a "Activity Monitor""#),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn query_returns_all_commands() {
        let p = SystemProvider;
        let out = p.query(&Query::new("anything")).await;
        assert_eq!(out.len(), COMMANDS.len());
    }

    #[tokio::test]
    async fn candidates_are_kind_action() {
        let p = SystemProvider;
        let out = p.query(&Query::new("")).await;
        for c in out {
            assert_eq!(c.kind, CandidateKind::Action);
            assert!(c.id.starts_with("sys::"));
            assert!(!c.bypass_rank);
        }
    }

    #[tokio::test]
    async fn search_text_includes_keywords_for_fuzzy_match() {
        let p = SystemProvider;
        let out = p.query(&Query::new("")).await;
        let trash = out.iter().find(|c| c.id == "sys::empty-trash").unwrap();
        assert!(trash.search_text.contains("trash"));
        assert!(trash.search_text.contains("empty"));
    }

    #[tokio::test]
    async fn activate_lock_yields_shell_effect() {
        let p = SystemProvider;
        let eff = p
            .activate(&"sys::lock-screen".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::RunShell(s) => assert!(s.contains("displaysleepnow")),
            other => panic!("expected RunShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_empty_trash_yields_empty_trash_effect() {
        let p = SystemProvider;
        let eff = p
            .activate(&"sys::empty-trash".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::EmptyTrash => {}
            other => panic!("expected EmptyTrash, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_unknown_command_errors() {
        let p = SystemProvider;
        assert!(p
            .activate(&"sys::bogus".to_string(), "default")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = SystemProvider;
        assert!(p
            .activate(&"apps::Safari".to_string(), "default")
            .await
            .is_err());
    }

    #[test]
    fn command_ids_are_unique() {
        let mut ids: Vec<&str> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort();
        let dedup_len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), dedup_len, "duplicate command id");
    }
}
