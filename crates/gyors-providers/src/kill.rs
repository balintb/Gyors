//! Kill-process provider. Activated by the `kill` keyword
//!
//! `kill chrome` shells out to `ps` and lists matching processes as
//! candidates. Default action sends `SIGTERM`; the `->` chain action
//! exposes a secondary "Force Kill" which sends `SIGKILL`

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct KillProvider;

const RESULT_LIMIT: usize = 15;

#[async_trait]
impl Provider for KillProvider {
    fn id(&self) -> &str {
        "kill"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let filter = match pattern.strip_prefix("kill ") {
            Some(f) => f.trim().to_string(),
            None => return vec![],
        };
        if filter.is_empty() {
            return vec![];
        }
        let procs = list_processes().await.unwrap_or_default();
        let filter_lower = filter.to_lowercase();
        procs
            .into_iter()
            .filter(|p| p.name.to_lowercase().contains(&filter_lower))
            .take(RESULT_LIMIT)
            .map(make_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> anyhow::Result<Effect> {
        let pid_str = id
            .strip_prefix("kill::")
            .ok_or_else(|| anyhow::anyhow!("invalid kill candidate id: {id}"))?;
        let pid: u32 = pid_str
            .parse()
            .map_err(|_| anyhow::anyhow!("bad pid: {pid_str}"))?;
        let signal = match action {
            "default" => "TERM",
            "force" => "KILL",
            other => anyhow::bail!("unknown action for kill: {other}"),
        };
        Ok(Effect::RunShell(format!("kill -{signal} {pid}")))
    }
}

#[derive(Debug, Clone)]
struct ProcessInfo {
    pid: u32,
    name: String,
    command: String,
}

fn make_candidate(p: ProcessInfo) -> Candidate {
    Candidate {
        id: format!("kill::{}", p.pid),
        title: format!("Kill {}", p.name),
        subtitle: Some(format!("pid {} · {}", p.pid, truncate(&p.command, 80))),
        icon: Icon::SfSymbol("xmark.octagon.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Terminate (SIGTERM)"),
            Action::new("force", "Force Kill (SIGKILL)"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

async fn list_processes() -> anyhow::Result<Vec<ProcessInfo>> {
    let output = tokio::process::Command::new("ps")
        .args(["-Ao", "pid=,ucomm=,command="])
        .output()
        .await?;
    if !output.status.success() {
        anyhow::bail!("ps failed: {}", output.status);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().filter_map(parse_ps_line).collect())
}

fn parse_ps_line(line: &str) -> Option<ProcessInfo> {
    let trimmed = line.trim_start();
    let pid_end = trimmed.find(char::is_whitespace)?;
    let pid: u32 = trimmed[..pid_end].parse().ok()?;
    let rest = trimmed[pid_end..].trim_start();
    let name_end = rest.find(char::is_whitespace)?;
    let name = rest[..name_end].to_string();
    let command = rest[name_end..].trim_start().to_string();
    if name.is_empty() {
        return None;
    }
    Some(ProcessInfo { pid, name, command })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.into()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_keyword_no_candidate() {
        let p = KillProvider;
        assert!(p.query(&Query::new("chrome")).await.is_empty());
    }

    #[tokio::test]
    async fn empty_filter_no_candidate() {
        let p = KillProvider;
        assert!(p.query(&Query::new("kill ")).await.is_empty());
        assert!(p.query(&Query::new("kill    ")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_default_yields_sigterm() {
        let p = KillProvider;
        let effect = p.activate(&"kill::1234".to_string(), "default").await.unwrap();
        match effect {
            Effect::RunShell(s) => assert_eq!(s, "kill -TERM 1234"),
            other => panic!("expected RunShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_force_yields_sigkill() {
        let p = KillProvider;
        let effect = p.activate(&"kill::9876".to_string(), "force").await.unwrap();
        match effect {
            Effect::RunShell(s) => assert_eq!(s, "kill -KILL 9876"),
            other => panic!("expected RunShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_unknown_action_errors() {
        let p = KillProvider;
        assert!(p.activate(&"kill::1".to_string(), "nuke").await.is_err());
    }

    #[tokio::test]
    async fn activate_bad_pid_errors() {
        let p = KillProvider;
        assert!(p.activate(&"kill::nope".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = KillProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn parse_ps_line_basic() {
        let line = "    1 launchd           /sbin/launchd";
        let info = parse_ps_line(line).unwrap();
        assert_eq!(info.pid, 1);
        assert_eq!(info.name, "launchd");
        assert_eq!(info.command, "/sbin/launchd");
    }

    #[test]
    fn parse_ps_line_command_with_args() {
        let line = "  12345 zsh               -zsh -i";
        let info = parse_ps_line(line).unwrap();
        assert_eq!(info.pid, 12345);
        assert_eq!(info.name, "zsh");
        assert_eq!(info.command, "-zsh -i");
    }

    #[test]
    fn parse_ps_line_missing_command_is_none() {
        assert!(parse_ps_line("").is_none());
        assert!(parse_ps_line("123").is_none());
        assert!(parse_ps_line("   ").is_none());
    }

    #[test]
    fn parse_ps_line_non_numeric_pid_is_none() {
        assert!(parse_ps_line("abc name cmd").is_none());
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn live_ps_smoke_test() {
        // On macOS there is always at least one process (launchd, pid 1)
        let procs = list_processes().await.expect("ps works");
        assert!(!procs.is_empty());
        assert!(procs.iter().any(|p| p.pid == 1));
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn live_query_matches_own_process() {
        // Test binary is running, so ps should list something that
        // matches a broad filter
        let p = KillProvider;
        let out = p.query(&Query::new("kill kernel")).await;
        // "kernel_task" always exists on macOS with pid 0
        assert!(!out.is_empty(), "expected at least one process matching 'kernel'");
        for c in &out {
            assert!(c.id.starts_with("kill::"));
            assert_eq!(c.actions.len(), 2);
        }
    }
}
