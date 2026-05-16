//! Shell command provider
//!
//! Activated by the `>` prefix (`QueryMode::Shell`). User types
//! `>some shell command` and sees a single "Run: ..." candidate; activation
//! fires `Effect::RunShell`, which Swift executes via user's login
//! shell (`$SHELL -ic`), falling back to `/bin/sh -c` if `$SHELL` is unset
//!
//! No output is captured - this is fire-and-forget. Commands producing
//! output should be run in a terminal

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct ShellProvider;

#[async_trait]
impl Provider for ShellProvider {
    fn id(&self) -> &str {
        "shell"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let cmd = query.pattern().trim();
        if cmd.is_empty() {
            return vec![];
        }
        vec![make_candidate(cmd)]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let cmd = id
            .strip_prefix("shell::")
            .ok_or_else(|| anyhow::anyhow!("invalid shell candidate id: {id}"))?;
        Ok(Effect::RunShell(cmd.to_string()))
    }
}

fn make_candidate(cmd: &str) -> Candidate {
    Candidate {
        id: format!("shell::{cmd}"),
        title: format!("Run: {cmd}"),
        subtitle: Some("Execute via $SHELL".into()),
        icon: Icon::SfSymbol("terminal.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Run")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_query_yields_no_candidates() {
        let p = ShellProvider;
        assert!(p.query(&Query::new("")).await.is_empty());
        assert!(p.query(&Query::new("   ")).await.is_empty());
    }

    #[tokio::test]
    async fn single_candidate_for_any_command() {
        let p = ShellProvider;
        let out = p.query(&Query::new("ls -la")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Run: ls -la");
        assert_eq!(out[0].kind, CandidateKind::Action);
        assert!(out[0].bypass_rank);
        assert_eq!(out[0].actions.len(), 1);
        assert_eq!(out[0].actions[0].label, "Run");
    }

    #[tokio::test]
    async fn candidate_id_round_trips_through_activate() {
        let p = ShellProvider;
        let out = p.query(&Query::new("echo hi")).await;
        let id = out[0].id.clone();
        let eff = p.activate(&id, "default").await.unwrap();
        match eff {
            Effect::RunShell(s) => assert_eq!(s, "echo hi"),
            other => panic!("expected RunShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = ShellProvider;
        assert!(p
            .activate(&"apps::Safari".to_string(), "default")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn activate_malformed_id_errors() {
        let p = ShellProvider;
        assert!(p.activate(&"shellecho".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn preserves_whitespace_inside_command() {
        let p = ShellProvider;
        let out = p.query(&Query::new("grep -r  foo  bar")).await;
        // Internal multi-spaces are preserved - only outer whitespace is trimmed
        assert_eq!(out[0].title, "Run: grep -r  foo  bar");
    }
}
