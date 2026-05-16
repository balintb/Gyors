//! Recently used files across all apps. Keyword: `recent` / `rec`
//!
//! Uses Spotlight (`mdfind` with `kMDItemLastUsedDate`) to surface files
//! user has actually opened lately, ranked by most recent first.
//! Filters out system noise (xattr cache files, `.DS_Store`, etc.)

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::path::PathBuf;
use std::process::Command;

pub struct RecentProvider;

const RESULT_LIMIT: usize = 25;
/// How far back "recent" reaches. One week gives a comfortable working
/// set without dredging up stale opens
const WINDOW_DAYS: i64 = 14;

#[async_trait]
impl Provider for RecentProvider {
    fn id(&self) -> &str {
        "recent"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(filter) = strip_keyword(pattern) else { return vec![]; };
        let filter_lower = filter.trim().to_lowercase();

        let items = tokio::task::spawn_blocking(|| run_mdfind(WINDOW_DAYS, 200))
            .await
            .unwrap_or_default();

        items
            .into_iter()
            .filter(|path| !should_skip(path))
            .filter(|path| {
                if filter_lower.is_empty() { return true; }
                path.to_string_lossy().to_lowercase().contains(&filter_lower)
            })
            .take(RESULT_LIMIT)
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> anyhow::Result<Effect> {
        let path = id
            .strip_prefix("recent::")
            .ok_or_else(|| anyhow::anyhow!("invalid recent candidate id: {id}"))?;
        let p = PathBuf::from(path);
        match action {
            "default" => Ok(Effect::OpenPath(p)),
            "reveal" => Ok(Effect::RevealInFinder(p)),
            "copy-path" => Ok(Effect::CopyToClipboard(path.to_string())),
            other => anyhow::bail!("unknown action for recent: {other}"),
        }
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    if s == "recent" || s == "rec" { return Some(""); }
    s.strip_prefix("recent ").or_else(|| s.strip_prefix("rec "))
}

/// Run `mdfind` with a Spotlight query that returns files opened within
/// `days` of now, sorted by `kMDItemLastUsedDate` descending. Returns at
/// most `max` absolute paths
fn run_mdfind(days: i64, max: usize) -> Vec<PathBuf> {
    // `$time.today(-N)` is a Spotlight-specific magic helper that
    // resolves to a timestamp N days before now
    let query = format!(
        "kMDItemLastUsedDate >= $time.today(-{days}) && kMDItemKind != 'Volume'"
    );
    let Ok(out) = Command::new("/usr/bin/mdfind").args([&query]).output() else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    // `mdfind` doesn't honour `-sort` on kMDItemLastUsedDate so we gather
    // up-to-`max*4` candidates and sort ourselves via file mtime fallback
    // when the Spotlight index lacks a usage timestamp for some files
    let mut lines: Vec<PathBuf> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .take(max * 4)
        .map(PathBuf::from)
        .collect();
    lines.sort_by_key(|p| std::cmp::Reverse(mtime_secs(p)));
    lines.truncate(max);
    lines
}

fn mtime_secs(path: &std::path::Path) -> i64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Noise filter: system caches, hidden files, and folders we dont want
/// as "recent opens"
pub fn should_skip(path: &std::path::Path) -> bool {
    let name = match path.file_name().and_then(|s| s.to_str()) {
        Some(n) => n,
        None => return true,
    };
    if name.starts_with('.') { return true; }
    let path_str = path.to_string_lossy();
    // Library + app caches, Xcode build artefacts, etc. - these get
    // touched constantly and aren't "recent opens" in any user sense
    const NOISY_SEGMENTS: &[&str] = &[
        "/Library/Caches/",
        "/Library/Saved Application State/",
        "/Library/Application Support/",
        "/Library/Preferences/",
        "/DerivedData/",
        "/node_modules/",
        "/target/",
        "/.cargo/",
        "/.git/",
    ];
    NOISY_SEGMENTS.iter().any(|s| path_str.contains(s))
}

fn to_candidate(path: PathBuf) -> Candidate {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let subtitle = path.to_string_lossy().into_owned();
    let symbol = if path.is_dir() { "folder.fill" } else { "clock.arrow.circlepath" };
    Candidate {
        id: format!("recent::{}", subtitle),
        title: name,
        subtitle: Some(subtitle.clone()),
        icon: Icon::SfSymbol(symbol.into()),
        kind: CandidateKind::File,
        actions: vec![
            Action::primary("Open"),
            Action::new("reveal", "Reveal in Finder"),
            Action::new("copy-path", "Copy Path"),
        ],
        search_text: subtitle,
        bypass_rank: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = RecentProvider;
        assert!(p.query(&Query::new("some file")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_default_opens_path() {
        let p = RecentProvider;
        let eff = p.activate(&"recent::/tmp/x.pdf".to_string(), "default").await.unwrap();
        match eff {
            Effect::OpenPath(p) => assert_eq!(p, PathBuf::from("/tmp/x.pdf")),
            other => panic!("expected OpenPath, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_reveal_uses_reveal_in_finder() {
        let p = RecentProvider;
        let eff = p.activate(&"recent::/tmp/x.pdf".to_string(), "reveal").await.unwrap();
        assert!(matches!(eff, Effect::RevealInFinder(_)));
    }

    #[tokio::test]
    async fn activate_copy_path_returns_clipboard_effect() {
        let p = RecentProvider;
        let eff = p.activate(&"recent::/tmp/x.pdf".to_string(), "copy-path").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "/tmp/x.pdf"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = RecentProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_unknown_action_errors() {
        let p = RecentProvider;
        assert!(p
            .activate(&"recent::/tmp/x".to_string(), "bogus")
            .await
            .is_err());
    }


    #[test]
    fn skip_hidden_files() {
        assert!(should_skip(Path::new("/tmp/.DS_Store")));
        assert!(should_skip(Path::new("/home/user/.bashrc")));
    }

    #[test]
    fn skip_library_caches() {
        assert!(should_skip(Path::new("/Users/x/Library/Caches/foo/bar")));
        assert!(should_skip(Path::new("/Users/x/Library/Application Support/Foo/a")));
        assert!(should_skip(Path::new("/Users/x/Library/Saved Application State/a")));
    }

    #[test]
    fn skip_node_and_git_and_target() {
        assert!(should_skip(Path::new("/x/node_modules/y.js")));
        assert!(should_skip(Path::new("/x/target/debug/y")));
        assert!(should_skip(Path::new("/x/.git/HEAD")));
    }

    #[test]
    fn keep_user_docs() {
        assert!(!should_skip(Path::new("/Users/me/Documents/notes.md")));
        assert!(!should_skip(Path::new("/Users/me/Desktop/slides.pdf")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_mdfind_smoke_test() {
        // Just verify it doesn't panic on the current host; empty result
        // on a CI box without Spotlight is fine
        let _ = run_mdfind(7, 10);
    }
}
