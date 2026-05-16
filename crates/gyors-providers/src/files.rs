//! File search provider - shells out to `mdfind` for filename matches
//!
//! Phase-1 implementation. Future: replace with direct `MDQuery` C API calls
//! via a Swift shim to avoid subprocess overhead on every keystroke

use anyhow::Result;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::path::{Path, PathBuf};

const MIN_PATTERN_LEN: usize = 2;
const RESULT_LIMIT: usize = 10;

pub struct FilesProvider {
    search_root: PathBuf,
}

impl FilesProvider {
    pub fn new() -> Self {
        let root = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self { search_root: root }
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { search_root: root }
    }
}

impl Default for FilesProvider {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl Provider for FilesProvider {
    fn id(&self) -> &str { "files" }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        if pattern.len() < MIN_PATTERN_LEN {
            return vec![];
        }
        match run_mdfind(&self.search_root, pattern).await {
            Ok(stdout) => to_candidates(parse_mdfind_lines(&stdout), RESULT_LIMIT),
            Err(e) => {
                tracing::warn!("mdfind failed: {e}");
                vec![]
            }
        }
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        let path = id
            .strip_prefix("files::")
            .ok_or_else(|| anyhow::anyhow!("invalid files candidate id: {id}"))?;
        let path_buf = PathBuf::from(path);
        match action {
            "default" => Ok(Effect::OpenPath(path_buf)),
            "reveal" => Ok(Effect::RevealInFinder(path_buf)),
            "copy-path" => Ok(Effect::CopyToClipboard(path.to_string())),
            other => anyhow::bail!("unknown action for files: {other}"),
        }
    }
}

async fn run_mdfind(root: &Path, pattern: &str) -> Result<String> {
    let output = tokio::process::Command::new("mdfind")
        .arg("-onlyin")
        .arg(root)
        .arg("-name")
        .arg(pattern)
        .output()
        .await?;
    if !output.status.success() {
        return Err(anyhow::anyhow!("mdfind exited {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_mdfind_lines(stdout: &str) -> Vec<PathBuf> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn is_app_bundle(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("app")
}

fn to_candidates(paths: Vec<PathBuf>, limit: usize) -> Vec<Candidate> {
    paths
        .into_iter()
        .filter(|p| !is_app_bundle(p))
        .take(limit)
        .filter_map(make_candidate)
        .collect()
}

fn make_candidate(path: PathBuf) -> Option<Candidate> {
    let title = path.file_name()?.to_string_lossy().into_owned();
    let subtitle = path.parent().map(|p| p.to_string_lossy().into_owned());
    let path_str = path.to_string_lossy().into_owned();
    Some(Candidate {
        id: format!("files::{path_str}"),
        title: title.clone(),
        subtitle,
        icon: Icon::Path(path),
        kind: CandidateKind::File,
        actions: vec![
            Action::primary("Open"),
            Action::new("reveal", "Reveal in Finder"),
            Action::new("copy-path", "Copy Path"),
        ],
        search_text: title,
        bypass_rank: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // parse_mdfind_lines

    #[test]
    fn parse_two_lines() {
        let input = "/Users/me/foo.txt\n/Users/me/bar.md\n";
        let paths = parse_mdfind_lines(input);
        assert_eq!(paths, vec![
            PathBuf::from("/Users/me/foo.txt"),
            PathBuf::from("/Users/me/bar.md"),
        ]);
    }

    #[test]
    fn parse_skips_empty_and_whitespace_lines() {
        let input = "\n/a\n   \n/b\n\n";
        assert_eq!(
            parse_mdfind_lines(input),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn parse_preserves_spaces_in_names() {
        let input = "/Users/me/My Document.pdf\n";
        assert_eq!(
            parse_mdfind_lines(input),
            vec![PathBuf::from("/Users/me/My Document.pdf")]
        );
    }

    #[test]
    fn parse_empty_input_is_empty() {
        assert!(parse_mdfind_lines("").is_empty());
    }

    // is_app_bundle

    #[test]
    fn detects_app_bundle() {
        assert!(is_app_bundle(&PathBuf::from("/Applications/Safari.app")));
        assert!(!is_app_bundle(&PathBuf::from("/Users/me/doc.txt")));
        assert!(!is_app_bundle(&PathBuf::from("/Users/me/app")));
    }

    // to_candidates

    #[test]
    fn to_candidates_excludes_app_bundles() {
        let paths = vec![
            PathBuf::from("/Applications/Safari.app"),
            PathBuf::from("/Users/me/doc.txt"),
        ];
        let candidates = to_candidates(paths, 10);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].title, "doc.txt");
    }

    #[test]
    fn to_candidates_respects_limit() {
        let paths: Vec<_> = (0..20)
            .map(|i| PathBuf::from(format!("/tmp/f{i}.txt")))
            .collect();
        assert_eq!(to_candidates(paths, 5).len(), 5);
    }

    #[test]
    fn to_candidates_zero_limit_empty() {
        let paths = vec![PathBuf::from("/tmp/a")];
        assert_eq!(to_candidates(paths, 0).len(), 0);
    }

    // make_candidate

    #[test]
    fn make_candidate_sets_title_subtitle_and_id() {
        let c = make_candidate(PathBuf::from("/Users/me/doc.txt")).unwrap();
        assert_eq!(c.title, "doc.txt");
        assert_eq!(c.subtitle.as_deref(), Some("/Users/me"));
        assert_eq!(c.id, "files::/Users/me/doc.txt");
        assert_eq!(c.kind, CandidateKind::File);
        assert_eq!(c.search_text, "doc.txt");
        assert!(!c.bypass_rank);
    }

    #[test]
    fn make_candidate_top_level_file_has_root_subtitle() {
        let c = make_candidate(PathBuf::from("/root.txt")).unwrap();
        assert_eq!(c.title, "root.txt");
        assert_eq!(c.subtitle.as_deref(), Some("/"));
    }

    // Provider impl

    #[tokio::test]
    async fn short_query_returns_empty() {
        let p = FilesProvider::new();
        assert!(p.query(&Query::new("")).await.is_empty());
        assert!(p.query(&Query::new("a")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_produces_openpath() {
        let p = FilesProvider::new();
        let eff = p
            .activate(&"files::/tmp/a".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::OpenPath(p) => assert_eq!(p, PathBuf::from("/tmp/a")),
            other => panic!("expected OpenPath, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_rejects_foreign_id() {
        let p = FilesProvider::new();
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn live_mdfind_smoke_test() {
        // Integration: expects mdfind to return at least something when
        // searching for a very common filename in /. Runs only on macOS
        let p = FilesProvider::with_root(PathBuf::from("/"));
        // "Info.plist" is present in thousands of places on any Mac
        let out = p.query(&Query::new("Info.plist")).await;
        // Allow empty results on machines where Spotlight is disabled, but
        // at least confirm call doesn't panic or return garbage types
        for c in out {
            assert_eq!(c.kind, CandidateKind::File);
            assert!(c.id.starts_with("files::"));
        }
    }
}
