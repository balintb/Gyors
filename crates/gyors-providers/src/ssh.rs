//! SSH host picker. Parses `~/.ssh/config` for `Host` entries and lets
//! user launch `ssh <name>` in Terminal
//!
//! Keyword: `ssh` / `ssh <filter>`

use anyhow::Result;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::path::PathBuf;
use std::sync::Arc;

const RESULT_LIMIT: usize = 30;

#[derive(Debug, Clone)]
pub struct SshHost {
    pub name: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<String>,
}

pub struct SshProvider {
    hosts: Arc<ArcSwap<Vec<SshHost>>>,
}

impl SshProvider {
    pub async fn new() -> Self {
        let hosts = tokio::task::spawn_blocking(|| scan_hosts(&config_path()))
            .await
            .unwrap_or_default();
        Self {
            hosts: Arc::new(ArcSwap::from(Arc::new(hosts))),
        }
    }

    pub fn len(&self) -> usize {
        self.hosts.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl Provider for SshProvider {
    fn id(&self) -> &str {
        "ssh"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(rest) = strip_keyword(pattern) else { return vec![]; };
        let filter = rest.trim().to_lowercase();
        let hosts = self.hosts.load();
        hosts
            .iter()
            .filter(|h| filter.is_empty() || host_matches(h, &filter))
            .take(RESULT_LIMIT)
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> Result<Effect> {
        let name = id
            .strip_prefix("ssh::")
            .ok_or_else(|| anyhow::anyhow!("invalid ssh candidate id: {id}"))?;
        // Emit a plain command. Swift handler turns it into a
        // temp `.command` file + `open -a Terminal` so we never go
        // through Apple Events / Automation (which Sequoia blocks
        // until user grants the Gyors -> Terminal permission)
        let cmd = shell_escape(name);
        Ok(Effect::OpenInTerminal(format!("ssh {cmd}")))
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    if s == "ssh" { return Some(""); }
    s.strip_prefix("ssh ")
}

fn host_matches(h: &SshHost, filter_lower: &str) -> bool {
    h.name.to_lowercase().contains(filter_lower)
        || h
            .hostname
            .as_deref()
            .map(|s| s.to_lowercase().contains(filter_lower))
            .unwrap_or(false)
}

fn to_candidate(h: &SshHost) -> Candidate {
    let subtitle_bits: Vec<String> = [
        h.user.clone().map(|u| format!("{u}@")),
        h.hostname.clone(),
        h.port.as_deref().map(|p| format!(":{p}")).clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let subtitle = if subtitle_bits.is_empty() {
        format!("ssh {}", h.name)
    } else {
        format!("ssh  ·  {}", subtitle_bits.join(""))
    };
    Candidate {
        id: format!("ssh::{}", h.name),
        title: h.name.clone(),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol("terminal.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Open in Terminal")],
        search_text: format!(
            "{} {}",
            h.name,
            h.hostname.clone().unwrap_or_default()
        ),
        // Bypass the fuzzy ranker - the `ssh` keyword was already
        // consumed, so nucleo would just filter these back out. This is
        // consistent with every other keyword-triggered provider
        // (notes, clipboard, snippets, ...)
        bypass_rank: true,
    }
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".ssh").join("config"))
        .unwrap_or_else(|| PathBuf::from(".ssh/config"))
}

pub fn scan_hosts(path: &std::path::Path) -> Vec<SshHost> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_config(&text)
}

/// Minimal OpenSSH config parser - enough for `Host` / `HostName` /
/// `User` / `Port` lookups. Handles line comments, multi-name `Host`
/// lines, and case-insensitive keywords. Wildcard-only blocks (e.g.
/// `Host *`) are skipped because they're not selectable destinations
pub fn parse_config(text: &str) -> Vec<SshHost> {
    let mut out: Vec<SshHost> = Vec::new();
    let mut current: Vec<SshHost> = Vec::new();

    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => {
                // `=` may come after keyword (`HostName = value`)
                // or be keyword/value separator itself; either way
                // peel it off before trimming quotes
                let v = v.trim().trim_start_matches('=').trim().trim_matches('"');
                (k.trim().to_lowercase(), v.to_string())
            }
            None => continue,
        };
        match key.as_str() {
            "host" => {
                out.extend(std::mem::take(&mut current));
                for name in value.split_whitespace() {
                    // Ignore wildcard-only patterns - they're default
                    // fall-throughs, not selectable hosts
                    if name.chars().all(|c| c == '*' || c == '?') {
                        continue;
                    }
                    if name.contains('*') || name.contains('?') {
                        continue; // skip pattern hosts for now
                    }
                    current.push(SshHost {
                        name: name.to_string(),
                        hostname: None,
                        user: None,
                        port: None,
                    });
                }
            }
            "hostname" => {
                for h in current.iter_mut() {
                    h.hostname = Some(value.clone());
                }
            }
            "user" => {
                for h in current.iter_mut() {
                    h.user = Some(value.clone());
                }
            }
            "port" => {
                for h in current.iter_mut() {
                    h.port = Some(value.clone());
                }
            }
            _ => {}
        }
    }
    out.extend(current);
    out
}

/// Minimal POSIX shell-escape - wraps in single quotes and escapes
/// any embedded single quote. Used for the `ssh <host>` command we
/// drop into a `.command` file run by `/bin/sh`. A malicious
/// `~/.ssh/config` host name (e.g. `evil; rm -rf $HOME`) stays a
/// literal argv to ssh, never breaks out of the argument
fn shell_escape(s: &str) -> String {
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn make(hosts: Vec<SshHost>) -> SshProvider {
        SshProvider { hosts: Arc::new(ArcSwap::from(Arc::new(hosts))) }
    }

    fn host(name: &str) -> SshHost {
        SshHost { name: name.into(), hostname: None, user: None, port: None }
    }

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = make(vec![host("home")]);
        assert!(p.query(&Query::new("home")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_all_hosts() {
        let p = make(vec![host("a"), host("b"), host("c")]);
        let out = p.query(&Query::new("ssh")).await;
        assert_eq!(out.len(), 3);
    }

    #[tokio::test]
    async fn filter_matches_name_substring() {
        let p = make(vec![host("prod-db"), host("staging-db"), host("home")]);
        let out = p.query(&Query::new("ssh db")).await;
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn filter_matches_hostname() {
        let mut h = host("alias");
        h.hostname = Some("example.com".into());
        let p = make(vec![h, host("other")]);
        let out = p.query(&Query::new("ssh example")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "alias");
    }

    #[tokio::test]
    async fn activate_emits_open_in_terminal_with_ssh_command() {
        let p = make(vec![host("prod-db")]);
        let eff = p.activate(&"ssh::prod-db".to_string(), "default").await.unwrap();
        match eff {
            Effect::OpenInTerminal(cmd) => {
                assert_eq!(cmd, "ssh 'prod-db'");
            }
            other => panic!("expected OpenInTerminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_quotes_hostname_with_special_chars() {
        // shell_escape wraps in single quotes - defends `;` injection
        // even though valid SSH host names dont have them. Belt and
        // braces; the OpenInTerminal command lands in a `.command`
        // file run via /bin/sh, so escaping discipline is same as
        // any other shell input
        let p = make(vec![host("evil$(whoami)")]);
        let eff = p
            .activate(&"ssh::evil$(whoami)".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::OpenInTerminal(cmd) => {
                assert!(cmd.contains("'evil$(whoami)'"));
                assert!(!cmd.contains("$(whoami)\""));
            }
            other => panic!("expected OpenInTerminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = make(vec![]);
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }


    #[test]
    fn parse_empty_config_is_empty() {
        assert!(parse_config("").is_empty());
        assert!(parse_config("# comment only\n\n").is_empty());
    }

    #[test]
    fn parse_single_host_block() {
        let text = "\
Host home
  HostName home.example.com
  User someone
  Port 2222
";
        let hosts = parse_config(text);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "home");
        assert_eq!(hosts[0].hostname.as_deref(), Some("home.example.com"));
        assert_eq!(hosts[0].user.as_deref(), Some("someone"));
        assert_eq!(hosts[0].port.as_deref(), Some("2222"));
    }

    #[test]
    fn parse_multi_name_host_line() {
        let text = "\
Host dev prod
  HostName shared.example.com
";
        let hosts = parse_config(text);
        assert_eq!(hosts.len(), 2);
        let names: Vec<_> = hosts.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"dev"));
        assert!(names.contains(&"prod"));
        for h in &hosts {
            assert_eq!(h.hostname.as_deref(), Some("shared.example.com"));
        }
    }

    #[test]
    fn parse_skips_wildcard_hosts() {
        let text = "\
Host *
  User default-user

Host *.internal
  User intern

Host real
  HostName r.example.com
";
        let hosts = parse_config(text);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "real");
    }

    #[test]
    fn parse_handles_comments_and_blank_lines() {
        let text = "\
# comment
Host home # trailing comment
  HostName example.com
  # inner
  User me

Host other
";
        let hosts = parse_config(text);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].name, "home");
        assert_eq!(hosts[0].hostname.as_deref(), Some("example.com"));
    }

    #[test]
    fn parse_handles_equals_separator_and_quotes() {
        let text = r#"
Host quoted
  HostName = "quoted.example.com"
  User = "me"
"#;
        let hosts = parse_config(text);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].hostname.as_deref(), Some("quoted.example.com"));
        assert_eq!(hosts[0].user.as_deref(), Some("me"));
    }

    #[test]
    fn scan_hosts_missing_file_is_empty() {
        assert!(scan_hosts(Path::new("/definitely/not/a/real/path")).is_empty());
    }

    #[test]
    fn shell_escape_wraps_in_single_quotes() {
        assert_eq!(shell_escape("plain"), "'plain'");
        // Embedded single quote - close, escape, reopen. Standard
        // POSIX trick
        assert_eq!(shell_escape("a'b"), r"'a'\''b'");
        // Backslash, double quote, semicolon - all stay literal
        // inside single quotes (the whole point)
        assert_eq!(shell_escape(r#"a\"b;c"#), r#"'a\"b;c'"#);
    }
}
