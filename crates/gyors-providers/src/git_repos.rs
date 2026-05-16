//! Git repo launcher
//!
//! Scans the directories listed in the `repo_roots` config key for
//! `.git` directories at startup. Defaults to
//! `~/Documents`, `~/Projects`, `~/dev` when the key is absent. The
//! `repo` (or `repos`) keyword surfaces matches
//!
//! Every candidate carries:
//!
//! * Open-in-editor / terminal / Finder
//! * GitHub-flavoured actions (Open, Issues, Pull Requests, Actions,
//!   Search) - surfaced only when the origin remote is resolvable
//! * Local-git inspectors (Status, Recent Commits, Branches) that
//!   render output inline via `Effect::ShowText`
//! * Copy Path / Copy Remote URL / Copy Branch
//!
//! Sub-modes:
//! * `repo <name> find <q>` - fuzzy-match `git ls-files` inside `<name>`
//!   so users can jump into a specific file without shelling out

use anyhow::Result;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct GitReposProvider {
    state: Arc<ArcSwap<Vec<GitRepo>>>,
}

#[derive(Debug, Clone)]
pub struct GitRepo {
    pub path: PathBuf,
    pub name: String,
    /// Parsed `origin` remote, or `None` if the repo has no remote
    /// (fresh `git init`, purely-local work). Populated once at scan
    /// time - re-reading `.git/config` on every keystroke would be a
    /// per-query tax for data that almost never changes
    pub remote: Option<Remote>,
}

/// Structured view of an `origin` remote URL. We normalise both the
/// HTTPS (`https://github.com/owner/repo.git`) and SSH
/// (`git@github.com:owner/repo.git`) forms into same shape so
/// action URLs dont need to branch on how the remote was set up
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub host: String,
    pub owner: String,
    pub name: String,
    /// Raw origin URL as it appeared in `.git/config`. Preserved for
    /// the "Copy Remote URL" action so users get the exact form they
    /// (or their team) configured, not our canonicalised version
    pub raw: String,
}

impl Remote {
    /// HTTPS base URL - `https://host/owner/repo`. All the GitHub-
    /// style deep links (issues, pulls, search) are built off this
    pub fn web_url(&self) -> String {
        format!("https://{}/{}/{}", self.host, self.owner, self.name)
    }

    /// Is the remote hosted on GitHub? Drives whether we surface the
    /// GitHub-specific actions (Issues / Pull Requests / Actions /
    /// Search). GitLab and Bitbucket have similar URL patterns but
    /// different path segments - plumbed through `forge` below
    pub fn is_github(&self) -> bool {
        matches!(self.forge(), Forge::GitHub)
    }

    pub fn forge(&self) -> Forge {
        let host = self.host.to_ascii_lowercase();
        if host == "github.com" || host.ends_with(".github.com") {
            Forge::GitHub
        } else if host == "gitlab.com" || host.ends_with(".gitlab.com") {
            Forge::GitLab
        } else if host == "bitbucket.org" {
            Forge::Bitbucket
        } else {
            Forge::Unknown
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    GitLab,
    Bitbucket,
    Unknown,
}

impl GitReposProvider {
    /// Returns immediately with an empty repo set; spawns the
    /// filesystem scan on a background blocking task and `ArcSwap`s
    /// results into `state` when ready. Cold-start cost in the
    /// caller's path drops from ~400ms (the scan dominated by
    /// `~/Documents` recursion) to a few microseconds. The `repo`
    /// keyword returns no rows during the scan window, but user
    /// can't reach that prefix until panel is live anyway, by
    /// which time the scan has typically finished
    pub fn new() -> Self {
        let state = Arc::new(ArcSwap::from(Arc::new(Vec::<GitRepo>::new())));
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            if let Ok(scanned) = tokio::task::spawn_blocking(scan_all).await {
                st.store(Arc::new(scanned));
            }
        });
        Self { state }
    }
}

impl Default for GitReposProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GitReposProvider {
    pub fn len(&self) -> usize {
        self.state.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn find_repo(&self, name: &str) -> Option<GitRepo> {
        let needle = name.to_lowercase();
        self.state
            .load()
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&needle))
            .cloned()
    }
}

#[async_trait]
impl Provider for GitReposProvider {
    fn id(&self) -> &str {
        "repo"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        // Accept `repo` and `repos` as triggers. `repos` reads more
        // naturally on bare-keyword lookups ("show me my repos") and
        // is cheap to alias
        let rest = match strip_keyword(pattern) {
            Some(r) => r,
            None => return vec![],
        };

        // Sub-mode: `repo <name> find <q>` - fuzzy search files
        // inside a specific repo. Lets user pivot from launcher
        // to a file-in-repo jump in two tokens
        if let Some((repo_name, q)) = parse_find_subcommand(rest) {
            return self.find_in_repo(&repo_name, q).await;
        }

        let filter = rest.trim().to_lowercase();
        let repos = self.state.load();
        repos
            .iter()
            .filter(|r| filter.is_empty() || matches_filter(r, &filter))
            .take(15)
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        // File-in-repo row - id shape `repo::file::<repo_path>::<rel>`
        if let Some(payload) = id.strip_prefix("repo::file::") {
            return activate_file(payload, action);
        }

        let encoded = id
            .strip_prefix("repo::")
            .ok_or_else(|| anyhow::anyhow!("invalid repo candidate id: {id}"))?;
        let path = PathBuf::from(encoded);
        let remote = self
            .state
            .load()
            .iter()
            .find(|r| r.path == path)
            .and_then(|r| r.remote.clone());

        match action {
            "default" | "editor" | "vscode" => Ok(Effect::RunShell(format!(
                "open -a 'Visual Studio Code' {}",
                shell_quote(encoded)
            ))),
            "finder" => Ok(Effect::RevealInFinder(path.clone())),
            "terminal" => Ok(Effect::RunShell(format!(
                "open -a Terminal {}",
                shell_quote(encoded)
            ))),
            "open-web" => url_effect(remote.as_ref(), |r| r.web_url()),
            "open-issues" => url_effect(remote.as_ref(), |r| match r.forge() {
                Forge::GitHub | Forge::GitLab => format!("{}/issues", r.web_url()),
                Forge::Bitbucket => format!("{}/issues", r.web_url()),
                Forge::Unknown => r.web_url(),
            }),
            "open-prs" => url_effect(remote.as_ref(), |r| match r.forge() {
                Forge::GitHub => format!("{}/pulls", r.web_url()),
                Forge::GitLab => format!("{}/-/merge_requests", r.web_url()),
                Forge::Bitbucket => format!("{}/pull-requests", r.web_url()),
                Forge::Unknown => r.web_url(),
            }),
            "open-actions" => url_effect(remote.as_ref(), |r| match r.forge() {
                Forge::GitHub => format!("{}/actions", r.web_url()),
                Forge::GitLab => format!("{}/-/pipelines", r.web_url()),
                _ => r.web_url(),
            }),
            "open-search" => url_effect(remote.as_ref(), |r| match r.forge() {
                // GitHub's search landing page has a native search box,
                // so opening there is the "right" search entry point
                // even though we dont pass a query string
                Forge::GitHub => format!("{}/search", r.web_url()),
                Forge::GitLab => format!("{}/-/search", r.web_url()),
                _ => r.web_url(),
            }),
            "copy-path" => Ok(Effect::CopyToClipboard(encoded.to_string())),
            "copy-remote-url" => Ok(Effect::CopyToClipboard(
                remote
                    .as_ref()
                    .map(|r| r.raw.clone())
                    .unwrap_or_else(|| "(no remote configured)".into()),
            )),
            "copy-branch" => {
                let b = run_git(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                Ok(Effect::CopyToClipboard(b))
            }
            "git-status" => show_git(&path, &["status", "--short", "--branch"], "git status"),
            "git-log" => show_git(
                &path,
                &["log", "--oneline", "--decorate", "-20"],
                "git log",
            ),
            "git-branch" => show_git(&path, &["branch", "--sort=-committerdate"], "git branch"),
            "find-prefill" => Ok(Effect::SetInput(format!(
                "repo {} find ",
                display_name(&path),
            ))),
            other => anyhow::bail!("unknown action for repo: {other}"),
        }
    }
}

/// Accept query's `rest` string (after `repo` / `repos`), return
/// `Some((<repo-name>, <query>))` iff the shape is `<name> find <q>`.
/// Everything else falls through to plain filter matching
fn parse_find_subcommand(rest: &str) -> Option<(String, &str)> {
    let (head, tail) = rest.split_once(" find ")?;
    let name = head.trim();
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), tail.trim_start()))
}

/// `repo` or `repos`, plus any separator whitespace. None if the
/// query is something else entirely
fn strip_keyword(pattern: &str) -> Option<&str> {
    for kw in ["repos", "repo"] {
        if pattern.eq_ignore_ascii_case(kw) {
            return Some("");
        }
        if pattern.len() > kw.len() {
            let (head, rest) = pattern.split_at(kw.len());
            if head.eq_ignore_ascii_case(kw) && rest.starts_with(char::is_whitespace) {
                return Some(rest.trim_start());
            }
        }
    }
    None
}

/// Does the filter hit this repo? Matches against the directory name
/// AND the `owner/name` form when a remote is present, so a user
/// searching by the GitHub-side name lands even if their local
/// checkout used a different folder
fn matches_filter(repo: &GitRepo, filter_lower: &str) -> bool {
    if repo.name.to_lowercase().contains(filter_lower) {
        return true;
    }
    if let Some(remote) = &repo.remote {
        let combined = format!("{}/{}", remote.owner, remote.name).to_lowercase();
        if combined.contains(filter_lower) {
            return true;
        }
    }
    false
}

fn url_effect(remote: Option<&Remote>, build: impl FnOnce(&Remote) -> String) -> Result<Effect> {
    match remote {
        Some(r) => Ok(Effect::OpenUrl(build(r))),
        None => anyhow::bail!("no remote configured for this repo"),
    }
}

fn show_git(path: &Path, args: &[&str], label_stem: &str) -> Result<Effect> {
    let out = run_git(path, args).unwrap_or_else(|e| format!("(error: {e})"));
    Ok(Effect::ShowText {
        text: out,
        label: format!("{label_stem} · {}", display_name(path)),
        language: None,
        editable_path: None,
    })
}

fn to_candidate(repo: &GitRepo) -> Candidate {
    let mut actions: Vec<Action> = Vec::with_capacity(14);
    // Primary: open in VS Code. That's what most devs reach for first;
    // Reveal in Finder stays as a secondary so nobody loses the
    // previous default
    actions.push(Action::primary("Open in VS Code"));
    actions.push(Action::new("terminal", "Open in Terminal"));
    actions.push(Action::new("finder", "Reveal in Finder"));

    if let Some(remote) = &repo.remote {
        match remote.forge() {
            Forge::GitHub => {
                actions.push(Action::new("open-web", "Open on GitHub"));
                actions.push(Action::new("open-search", "Search on GitHub"));
                actions.push(Action::new("open-prs", "Pull Requests"));
                actions.push(Action::new("open-issues", "Issues"));
                actions.push(Action::new("open-actions", "GitHub Actions"));
            }
            Forge::GitLab => {
                actions.push(Action::new("open-web", "Open on GitLab"));
                actions.push(Action::new("open-search", "Search on GitLab"));
                actions.push(Action::new("open-prs", "Merge Requests"));
                actions.push(Action::new("open-issues", "Issues"));
                actions.push(Action::new("open-actions", "Pipelines"));
            }
            Forge::Bitbucket => {
                actions.push(Action::new("open-web", "Open on Bitbucket"));
                actions.push(Action::new("open-prs", "Pull Requests"));
                actions.push(Action::new("open-issues", "Issues"));
            }
            Forge::Unknown => {
                actions.push(Action::new("open-web", "Open in Browser"));
            }
        }
    }

    actions.push(Action::new("find-prefill", "Find File in Repo…"));
    actions.push(Action::new("git-status", "Git Status"));
    actions.push(Action::new("git-log", "Recent Commits"));
    actions.push(Action::new("git-branch", "Branches"));
    actions.push(Action::new("copy-path", "Copy Path"));
    if repo.remote.is_some() {
        actions.push(Action::new("copy-remote-url", "Copy Remote URL"));
    }
    actions.push(Action::new("copy-branch", "Copy Current Branch"));

    let subtitle = match &repo.remote {
        Some(r) => format!("{}/{}  ·  {}", r.owner, r.name, repo.path.display()),
        None => repo.path.display().to_string(),
    };
    let icon = match repo.remote.as_ref().map(|r| r.forge()) {
        Some(Forge::GitHub) => Icon::SfSymbol("chevron.left.forwardslash.chevron.right".into()),
        Some(Forge::GitLab) => Icon::SfSymbol("network".into()),
        Some(Forge::Bitbucket) => Icon::SfSymbol("cloud".into()),
        _ => Icon::SfSymbol("folder.badge.gearshape".into()),
    };

    Candidate {
        id: format!("repo::{}", repo.path.display()),
        title: repo.name.clone(),
        subtitle: Some(subtitle),
        icon,
        kind: CandidateKind::Custom("repo".into()),
        actions,
        search_text: match &repo.remote {
            Some(r) => format!("{} {}/{}", repo.name, r.owner, r.name),
            None => repo.name.clone(),
        },
        bypass_rank: false,
    }
}


impl GitReposProvider {
    async fn find_in_repo(&self, repo_name: &str, query: &str) -> Vec<Candidate> {
        let Some(repo) = self.find_repo(repo_name) else {
            return vec![no_repo_candidate(repo_name)];
        };
        let files = tokio::task::spawn_blocking({
            let p = repo.path.clone();
            move || list_files(&p)
        })
        .await
        .unwrap_or_default();

        let qlower = query.to_lowercase();
        let mut hits: Vec<String> = files
            .into_iter()
            .filter(|f| qlower.is_empty() || f.to_lowercase().contains(&qlower))
            .take(30)
            .collect();
        hits.sort_by_key(|f| f.len()); // shortest match first - the classic fuzzy shortcut
        hits.into_iter()
            .map(|rel| file_candidate(&repo, &rel))
            .collect()
    }
}

fn list_files(repo: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .output();
    let Ok(out) = out else { return vec![] };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn file_candidate(repo: &GitRepo, relative: &str) -> Candidate {
    let abs = repo.path.join(relative);
    let mut actions = vec![
        Action::primary("Open in VS Code"),
        Action::new("reveal", "Reveal in Finder"),
        Action::new("copy-path", "Copy Path"),
    ];
    if repo.remote.is_some() {
        actions.push(Action::new("open-on-web", "View on GitHub"));
    }
    let id_payload = format!("{}::{}", repo.path.display(), relative);
    Candidate {
        id: format!("repo::file::{id_payload}"),
        title: relative.to_string(),
        subtitle: Some(format!("in {}  ·  {}", repo.name, abs.display())),
        icon: Icon::SfSymbol("doc.text".into()),
        kind: CandidateKind::Custom("repo-file".into()),
        actions,
        search_text: relative.to_string(),
        bypass_rank: true,
    }
}

fn activate_file(payload: &str, action: &str) -> Result<Effect> {
    let (repo_path, relative) = payload
        .split_once("::")
        .ok_or_else(|| anyhow::anyhow!("malformed repo file id: {payload}"))?;
    let abs = PathBuf::from(repo_path).join(relative);
    let abs_s = abs.to_string_lossy().to_string();
    match action {
        "default" | "editor" => Ok(Effect::RunShell(format!(
            "open -a 'Visual Studio Code' {}",
            shell_quote(&abs_s)
        ))),
        "reveal" => Ok(Effect::RevealInFinder(abs)),
        "copy-path" => Ok(Effect::CopyToClipboard(abs_s)),
        "open-on-web" => {
            // Best-effort: read remote + current branch so URL
            // points at file on the right branch
            let repo = PathBuf::from(repo_path);
            let remote = parse_origin(&repo)
                .ok_or_else(|| anyhow::anyhow!("no remote configured"))?;
            let branch = run_git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "HEAD".into());
            let url = match remote.forge() {
                Forge::GitHub => format!("{}/blob/{branch}/{relative}", remote.web_url()),
                Forge::GitLab => format!("{}/-/blob/{branch}/{relative}", remote.web_url()),
                Forge::Bitbucket => format!("{}/src/{branch}/{relative}", remote.web_url()),
                Forge::Unknown => remote.web_url(),
            };
            Ok(Effect::OpenUrl(url))
        }
        other => anyhow::bail!("unknown action for repo file: {other}"),
    }
}

fn no_repo_candidate(name: &str) -> Candidate {
    Candidate {
        id: "repo::__no-match__".into(),
        title: format!("No repo named {name:?}"),
        subtitle: Some("Type `repo` to see the full list".into()),
        icon: Icon::SfSymbol("exclamationmark.triangle".into()),
        kind: CandidateKind::Custom("repo-notice".into()),
        actions: vec![Action::primary("Dismiss")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Display name for action label. Uses the directory's base name
/// and falls back to the full path if we somehow get a pathological
/// input - better to print noise than to panic inside an activate
fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or(""))
        .to_string()
}

/// Run `git -C <path> <args>` and return its stdout. Timeouts are
/// implicit - git subcommands that inspect local state (status / log /
/// branch) return near-instantly even on huge repos
fn run_git(path: &Path, args: &[&str]) -> std::io::Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(format!(
            "git {}: exit {}\n{stderr}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Roots scanned for repos at startup. User can override via the
/// `repo_roots` config key (comma-separated, tildes expand). Falls
/// back to a short curated list of common code folders. Either way
/// we filter to roots that actually exist so a missing entry costs
/// nothing
fn scan_roots() -> Vec<PathBuf> {
    if let Some(paths) = crate::config::repo_roots_pref() {
        return paths.into_iter().filter(|p| p.exists()).collect();
    }
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return vec![],
    };
    // Default trio. Add more in `config.json` -> `repo_roots`
    ["Documents", "Projects", "dev"]
        .iter()
        .map(|s| home.join(s))
        .filter(|p| p.exists())
        .collect()
}

const MAX_SCAN_DEPTH: usize = 4;

fn scan_all() -> Vec<GitRepo> {
    let mut out = Vec::new();
    for root in scan_roots() {
        scan_dir(&root, &mut out, 0);
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

fn scan_dir(dir: &Path, out: &mut Vec<GitRepo>, depth: usize) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    // A .git sibling means this *is* the repo - dont descend further
    if dir.join(".git").exists() {
        if let Some(name) = dir.file_name().and_then(|s| s.to_str()) {
            let remote = parse_origin(dir);
            out.push(GitRepo {
                path: dir.to_path_buf(),
                name: name.to_string(),
                remote,
            });
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Ignore hidden/dot-folders + common noise
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "dist"
            || name == "build"
        {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.is_dir() {
                scan_dir(&path, out, depth + 1);
            }
        }
    }
}


/// Read `<repo>/.git/config` and extract the `[remote "origin"] url`.
/// Returns None if the repo has no origin (e.g. fresh `git init`) or
/// URL is in a shape we can't parse. We fall back -
/// no remote just means fewer actions on row, not a broken repo
pub fn parse_origin(repo: &Path) -> Option<Remote> {
    let cfg = std::fs::read_to_string(repo.join(".git").join("config")).ok()?;
    let url = extract_origin_url(&cfg)?;
    parse_remote_url(&url)
}

/// Minimal `.gitconfig` parser - we only need `[remote "origin"]` ->
/// `url`. A full INI parser would work but this is a one-screen file
/// in a well-known shape; keeping the logic local avoids a dep
fn extract_origin_url(cfg: &str) -> Option<String> {
    let mut in_origin = false;
    for line in cfg.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            in_origin = line.eq_ignore_ascii_case("[remote \"origin\"]");
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some(val) = line.strip_prefix("url") {
            return val.trim_start_matches(|c: char| c.is_whitespace() || c == '=')
                .trim()
                .to_string()
                .into();
        }
    }
    None
}

/// Parse `git@host:owner/repo[.git]` or `https://host/owner/repo[.git]`
/// (and the `ssh://git@host/...` / `git://host/...` variants) into a
/// `Remote`. Returns None on anything exotic - we'd rather drop the
/// remote actions than build a URL we can't guarantee is correct
pub fn parse_remote_url(url: &str) -> Option<Remote> {
    let raw = url.trim().to_string();
    let trimmed = raw.trim_end_matches('/').trim_end_matches(".git");

    // SSH shorthand: `git@host:owner/repo`. The colon is the
    // delimiter; after the colon is a plain path
    if let Some(rest) = trimmed.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            let (owner, name) = split_owner_repo(path)?;
            return Some(Remote {
                host: host.to_string(),
                owner: owner.to_string(),
                name: name.to_string(),
                raw,
            });
        }
    }

    for scheme in ["https://", "http://", "ssh://", "ssh://git@", "git://"] {
        if let Some(mut rest) = trimmed.strip_prefix(scheme) {
            if let Some(after_at) = rest.strip_prefix("git@") {
                rest = after_at;
            }
            let (host, path) = rest.split_once('/')?;
            let (owner, name) = split_owner_repo(path)?;
            return Some(Remote {
                host: host.to_string(),
                owner: owner.to_string(),
                name: name.to_string(),
                raw,
            });
        }
    }
    None
}

/// Split `owner/repo` out of a `path` that might have extra segments
/// (nested group on GitLab: `group/subgroup/repo`). We keep the LAST
/// two segments - GitLab groups render identically whether you ask
/// for `group/subgroup/repo` or just `group/subgroup/repo` in the UI
fn split_owner_repo(path: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [] | [_] => None,
        rest => {
            let owner = rest[rest.len() - 2];
            let name = rest[rest.len() - 1];
            Some((owner, name))
        }
    }
}

fn shell_quote(path: &str) -> String {
    // Simple POSIX-safe single-quote escaping
    let escaped = path.replace('\'', r"'\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_test_provider(repos: Vec<GitRepo>) -> GitReposProvider {
        GitReposProvider {
            state: Arc::new(ArcSwap::from(Arc::new(repos))),
        }
    }

    fn repo(name: &str, path: &str, remote: Option<Remote>) -> GitRepo {
        GitRepo {
            path: PathBuf::from(path),
            name: name.into(),
            remote,
        }
    }

    fn gh_remote(owner: &str, name: &str) -> Remote {
        Remote {
            host: "github.com".into(),
            owner: owner.into(),
            name: name.into(),
            raw: format!("git@github.com:{owner}/{name}.git"),
        }
    }

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = make_test_provider(vec![repo("b", "/a/b", None)]);
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_shows_everything() {
        let p = make_test_provider(vec![
            repo("foo", "/a/foo", None),
            repo("bar", "/a/bar", None),
        ]);
        assert_eq!(p.query(&Query::new("repo")).await.len(), 2);
        assert_eq!(p.query(&Query::new("repos")).await.len(), 2);
    }

    #[tokio::test]
    async fn filter_matches_directory_name() {
        let p = make_test_provider(vec![
            repo("gyors", "/a/gyors", None),
            repo("other", "/a/other", None),
        ]);
        let out = p.query(&Query::new("repo gyors")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "gyors");
    }

    #[tokio::test]
    async fn filter_is_case_insensitive() {
        let p = make_test_provider(vec![repo("Gyors", "/a/Gyors", None)]);
        assert_eq!(p.query(&Query::new("repo gyors")).await.len(), 1);
    }

    #[tokio::test]
    async fn filter_also_hits_github_owner_repo_form() {
        // User remembers the GitHub URL, not the local folder
        let p = make_test_provider(vec![repo(
            "weirdfolder",
            "/a/weirdfolder",
            Some(gh_remote("acme", "ignition")),
        )]);
        let out = p.query(&Query::new("repo acme/igni")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "weirdfolder");
    }

    #[tokio::test]
    async fn candidates_show_owner_repo_in_subtitle_when_remote_exists() {
        let p = make_test_provider(vec![repo(
            "ignition",
            "/a/ignition",
            Some(gh_remote("acme", "ignition")),
        )]);
        let out = p.query(&Query::new("repo")).await;
        let subtitle = out[0].subtitle.as_deref().unwrap_or("");
        assert!(subtitle.contains("acme/ignition"), "subtitle={subtitle}");
        assert!(subtitle.contains("/a/ignition"), "subtitle={subtitle}");
    }

    #[tokio::test]
    async fn candidates_without_remote_fall_back_to_path_subtitle() {
        let p = make_test_provider(vec![repo("lonely", "/a/lonely", None)]);
        let out = p.query(&Query::new("repo")).await;
        assert_eq!(out[0].subtitle.as_deref(), Some("/a/lonely"));
    }

    #[tokio::test]
    async fn github_candidates_surface_web_actions() {
        let p = make_test_provider(vec![repo(
            "ignition",
            "/a/ignition",
            Some(gh_remote("acme", "ignition")),
        )]);
        let out = p.query(&Query::new("repo")).await;
        let ids: Vec<_> = out[0].actions.iter().map(|a| a.id.clone()).collect();
        for expected in ["open-web", "open-search", "open-prs", "open-issues", "open-actions"] {
            assert!(ids.contains(&expected.to_string()), "missing {expected}: {ids:?}");
        }
    }

    #[tokio::test]
    async fn remoteless_candidates_hide_web_actions() {
        let p = make_test_provider(vec![repo("lonely", "/a/lonely", None)]);
        let out = p.query(&Query::new("repo")).await;
        let ids: Vec<_> = out[0].actions.iter().map(|a| a.id.clone()).collect();
        for forbidden in ["open-web", "open-search", "open-prs", "open-issues"] {
            assert!(
                !ids.contains(&forbidden.to_string()),
                "unexpected {forbidden} on remoteless repo: {ids:?}",
            );
        }
    }

    #[tokio::test]
    async fn default_action_opens_in_vscode() {
        let p = make_test_provider(vec![]);
        let effect = p
            .activate(&"repo::/a/x".to_string(), "default")
            .await
            .unwrap();
        if let Effect::RunShell(cmd) = effect {
            assert!(cmd.contains("Visual Studio Code"), "cmd={cmd}");
            assert!(cmd.contains("/a/x"));
        } else {
            panic!("expected RunShell, got {effect:?}");
        }
    }

    #[tokio::test]
    async fn open_web_emits_github_url_for_github_remote() {
        let p = make_test_provider(vec![repo(
            "ignition",
            "/a/ignition",
            Some(gh_remote("acme", "ignition")),
        )]);
        let out = p.query(&Query::new("repo")).await;
        let effect = p.activate(&out[0].id, "open-web").await.unwrap();
        match effect {
            Effect::OpenUrl(url) => {
                assert_eq!(url, "https://github.com/acme/ignition");
            }
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_search_points_at_github_search_page() {
        let p = make_test_provider(vec![repo(
            "r",
            "/a/r",
            Some(gh_remote("acme", "r")),
        )]);
        let out = p.query(&Query::new("repo")).await;
        let effect = p.activate(&out[0].id, "open-search").await.unwrap();
        match effect {
            Effect::OpenUrl(url) => {
                assert_eq!(url, "https://github.com/acme/r/search");
            }
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_prs_uses_forge_specific_path() {
        // GitHub -> /pulls, GitLab -> /-/merge_requests, Bitbucket -> /pull-requests
        for (host, expected) in [
            ("github.com", "https://github.com/acme/r/pulls"),
            ("gitlab.com", "https://gitlab.com/acme/r/-/merge_requests"),
            ("bitbucket.org", "https://bitbucket.org/acme/r/pull-requests"),
        ] {
            let remote = Remote {
                host: host.into(),
                owner: "acme".into(),
                name: "r".into(),
                raw: format!("git@{host}:acme/r.git"),
            };
            let p = make_test_provider(vec![repo("r", "/a/r", Some(remote))]);
            let out = p.query(&Query::new("repo")).await;
            let effect = p.activate(&out[0].id, "open-prs").await.unwrap();
            match effect {
                Effect::OpenUrl(url) => assert_eq!(url, expected, "host={host}"),
                other => panic!("expected OpenUrl, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn web_action_without_remote_errors() {
        // No remote -> no sensible URL, better to error than to open a
        // broken link. The UI shows error cleanly
        let p = make_test_provider(vec![repo("lonely", "/a/lonely", None)]);
        let out = p.query(&Query::new("repo")).await;
        let err = p.activate(&out[0].id, "open-web").await.unwrap_err();
        assert!(err.to_string().contains("no remote"), "err={err}");
    }

    #[tokio::test]
    async fn copy_remote_url_returns_raw_form() {
        // Raw URL (ssh or https) preserved - user copied what they set
        let p = make_test_provider(vec![repo(
            "r",
            "/a/r",
            Some(gh_remote("acme", "r")),
        )]);
        let out = p.query(&Query::new("repo")).await;
        let effect = p.activate(&out[0].id, "copy-remote-url").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert!(s.contains("git@github.com:acme/r")),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn find_prefill_sets_search_scaffolding() {
        let p = make_test_provider(vec![repo("gyors", "/a/gyors", None)]);
        let out = p.query(&Query::new("repo")).await;
        let effect = p.activate(&out[0].id, "find-prefill").await.unwrap();
        match effect {
            Effect::SetInput(s) => assert_eq!(s, "repo gyors find "),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn find_subcommand_shows_notice_for_unknown_repo() {
        let p = make_test_provider(vec![]);
        let out = p.query(&Query::new("repo ghost find ever")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("ghost"), "title={}", out[0].title);
    }

    #[tokio::test]
    async fn unknown_action_errors() {
        let p = make_test_provider(vec![]);
        assert!(p.activate(&"repo::/a/x".to_string(), "nope").await.is_err());
    }

    #[tokio::test]
    async fn foreign_prefix_errors() {
        let p = make_test_provider(vec![]);
        assert!(p.activate(&"apps::Safari".to_string(), "default").await.is_err());
    }


    #[test]
    fn parse_ssh_shorthand() {
        let r = parse_remote_url("git@github.com:acme/r.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "acme");
        assert_eq!(r.name, "r");
    }

    #[test]
    fn parse_https_url() {
        let r = parse_remote_url("https://github.com/acme/r.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "acme");
        assert_eq!(r.name, "r");
    }

    #[test]
    fn parse_https_url_without_dot_git() {
        let r = parse_remote_url("https://github.com/acme/r").unwrap();
        assert_eq!(r.name, "r");
    }

    #[test]
    fn parse_gitlab_nested_group_picks_last_two_segments() {
        // GitLab subgroups produce URLs with three path segments -
        // we preserve `owner/repo` semantics by using the last two,
        // which is what the web UI uses as the "repo" identifier
        let r = parse_remote_url("git@gitlab.com:group/subgroup/r.git").unwrap();
        assert_eq!(r.owner, "subgroup");
        assert_eq!(r.name, "r");
    }

    #[test]
    fn parse_ssh_scheme_with_user() {
        let r = parse_remote_url("ssh://git@github.com/acme/r.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "acme");
        assert_eq!(r.name, "r");
    }

    #[test]
    fn parse_invalid_remote_yields_none() {
        assert!(parse_remote_url("").is_none());
        assert!(parse_remote_url("not-a-url").is_none());
        assert!(parse_remote_url("git@host").is_none()); // no colon
    }

    #[test]
    fn extract_origin_url_from_gitconfig() {
        let cfg = r#"
[core]
    bare = false
[remote "origin"]
    url = git@github.com:acme/r.git
    fetch = +refs/heads/*:refs/remotes/origin/*
[branch "main"]
    remote = origin
"#;
        assert_eq!(
            extract_origin_url(cfg).as_deref(),
            Some("git@github.com:acme/r.git"),
        );
    }

    #[test]
    fn extract_origin_ignores_other_remotes() {
        let cfg = r#"
[remote "upstream"]
    url = git@github.com:other/r.git
[remote "origin"]
    url = git@github.com:mine/r.git
"#;
        assert_eq!(
            extract_origin_url(cfg).as_deref(),
            Some("git@github.com:mine/r.git"),
        );
    }

    #[test]
    fn remote_web_url_is_canonical_https() {
        let r = gh_remote("acme", "ignition");
        assert_eq!(r.web_url(), "https://github.com/acme/ignition");
    }

    #[test]
    fn forge_detection_covers_main_hosts() {
        let ghe = Remote {
            host: "enterprise.github.com".into(),
            owner: "o".into(),
            name: "r".into(),
            raw: "".into(),
        };
        assert_eq!(ghe.forge(), Forge::GitHub);
        let gl = Remote {
            host: "gitlab.com".into(),
            owner: "o".into(),
            name: "r".into(),
            raw: "".into(),
        };
        assert_eq!(gl.forge(), Forge::GitLab);
        let bb = Remote {
            host: "bitbucket.org".into(),
            owner: "o".into(),
            name: "r".into(),
            raw: "".into(),
        };
        assert_eq!(bb.forge(), Forge::Bitbucket);
        let unk = Remote {
            host: "git.example.com".into(),
            owner: "o".into(),
            name: "r".into(),
            raw: "".into(),
        };
        assert_eq!(unk.forge(), Forge::Unknown);
    }


    #[test]
    fn parse_origin_reads_config_file() {
        let td = TempDir::new().unwrap();
        let repo = td.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            repo.join(".git/config"),
            r#"[remote "origin"]
    url = git@github.com:acme/ignition.git
"#,
        )
        .unwrap();
        let remote = parse_origin(&repo).unwrap();
        assert_eq!(remote.host, "github.com");
        assert_eq!(remote.owner, "acme");
        assert_eq!(remote.name, "ignition");
    }

    #[test]
    fn parse_origin_missing_config_returns_none() {
        let td = TempDir::new().unwrap();
        let repo = td.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        assert!(parse_origin(&repo).is_none());
    }

    #[test]
    fn parse_origin_remoteless_config_returns_none() {
        let td = TempDir::new().unwrap();
        let repo = td.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".git/config"), "[core]\n    bare = false\n").unwrap();
        assert!(parse_origin(&repo).is_none());
    }

    #[test]
    fn scan_dir_detects_git_repo() {
        let td = TempDir::new().unwrap();
        let repo = td.path().join("myrepo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let mut out = Vec::new();
        scan_dir(td.path(), &mut out, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "myrepo");
    }

    #[test]
    fn scan_dir_populates_remote_when_present() {
        let td = TempDir::new().unwrap();
        let repo = td.path().join("gyors");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            repo.join(".git/config"),
            r#"[remote "origin"]
    url = https://github.com/balintb/Gyors.git
"#,
        )
        .unwrap();
        let mut out = Vec::new();
        scan_dir(td.path(), &mut out, 0);
        assert_eq!(out.len(), 1);
        let remote = out[0].remote.as_ref().unwrap();
        assert_eq!(remote.owner, "balintb");
        assert_eq!(remote.name, "Gyors");
    }

    #[test]
    fn scan_dir_skips_node_modules_and_target() {
        let td = TempDir::new().unwrap();
        fs::create_dir_all(td.path().join("node_modules/pkg/.git")).unwrap();
        fs::create_dir_all(td.path().join("target/crate/.git")).unwrap();
        fs::create_dir_all(td.path().join("build/x/.git")).unwrap();
        let mut out = Vec::new();
        scan_dir(td.path(), &mut out, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_dir_skips_hidden_siblings() {
        let td = TempDir::new().unwrap();
        fs::create_dir_all(td.path().join(".hidden/.git")).unwrap();
        let mut out = Vec::new();
        scan_dir(td.path(), &mut out, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn scan_dir_does_not_descend_into_repo() {
        // If a repo contains an inner repo, we should only surface the outer
        // one (mirrors how most devs think of their projects)
        let td = TempDir::new().unwrap();
        let outer = td.path().join("outer");
        fs::create_dir_all(outer.join(".git")).unwrap();
        fs::create_dir_all(outer.join("inner/.git")).unwrap();
        let mut out = Vec::new();
        scan_dir(td.path(), &mut out, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "outer");
    }

    #[test]
    fn shell_quote_simple_path() {
        assert_eq!(shell_quote("/Users/me/code"), "'/Users/me/code'");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("/a/b'c"), r"'/a/b'\''c'");
    }


    #[tokio::test]
    async fn file_row_activation_opens_in_editor() {
        let payload = "/a/gyors::src/main.rs";
        let effect = activate_file(payload, "default").unwrap();
        if let Effect::RunShell(cmd) = effect {
            assert!(cmd.contains("Visual Studio Code"));
            assert!(cmd.contains("/a/gyors/src/main.rs"));
        } else {
            panic!("expected RunShell, got {effect:?}");
        }
    }

    #[tokio::test]
    async fn file_row_copy_path_has_absolute_form() {
        let payload = "/a/gyors::src/main.rs";
        let effect = activate_file(payload, "copy-path").unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "/a/gyors/src/main.rs"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn file_row_unknown_action_errors() {
        assert!(activate_file("/a/r::file", "nope").is_err());
    }

    #[test]
    fn parse_find_subcommand_recognises_shape() {
        assert_eq!(
            parse_find_subcommand("gyors find main.rs"),
            Some(("gyors".into(), "main.rs")),
        );
        assert_eq!(
            parse_find_subcommand("gyors find "),
            Some(("gyors".into(), "")),
        );
        assert_eq!(parse_find_subcommand("gyors"), None);
        assert_eq!(parse_find_subcommand("find me"), None);
    }

    #[test]
    fn parse_find_subcommand_does_not_mistake_find_in_name_for_sub_keyword() {
        // A repo named `finder-theme` shouldn't get sliced up just
        // because `find` appears in it. The separator is ` find `
        // (space-delimited), so a name-with-`find` stays intact
        assert!(parse_find_subcommand("findertheme").is_none());
        // With a literal ` find ` separator it DOES split - that's
        // the intended UX
        assert_eq!(
            parse_find_subcommand("findertheme find foo.swift"),
            Some(("findertheme".into(), "foo.swift")),
        );
    }

    #[test]
    fn parse_remote_url_handles_trailing_slash() {
        let r = parse_remote_url("https://github.com/acme/r/").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.name, "r");
    }

    #[test]
    fn parse_remote_url_accepts_enterprise_github_host() {
        let r = parse_remote_url("git@git.corp.example.com:platform/api.git").unwrap();
        assert_eq!(r.host, "git.corp.example.com");
        assert_eq!(r.owner, "platform");
        assert_eq!(r.name, "api");
        assert_eq!(r.forge(), Forge::Unknown);
    }

    #[tokio::test]
    async fn open_actions_on_gitlab_routes_to_pipelines() {
        let remote = Remote {
            host: "gitlab.com".into(),
            owner: "g".into(),
            name: "r".into(),
            raw: "git@gitlab.com:g/r.git".into(),
        };
        let p = make_test_provider(vec![repo("r", "/a/r", Some(remote))]);
        let out = p.query(&Query::new("repo")).await;
        let effect = p.activate(&out[0].id, "open-actions").await.unwrap();
        match effect {
            Effect::OpenUrl(url) => assert_eq!(url, "https://gitlab.com/g/r/-/pipelines"),
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn find_sub_mode_recovers_unknown_repo_with_helpful_row() {
        // Dont silently return 0 results - that looks broken. A
        // single "No repo named X" row tells user what went wrong
        // with a path forward ("Type `repo` to see the full list")
        let p = make_test_provider(vec![]);
        let out = p.query(&Query::new("repos ghost find hi")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].subtitle.as_deref().unwrap_or("").contains("Type"));
    }

    #[tokio::test]
    async fn copy_remote_url_without_remote_reports_gracefully() {
        // No remote -> Copy still fires an action, but clipboard
        // value is a human-readable placeholder so user isn't
        // confused by a silent empty paste
        let p = make_test_provider(vec![repo("lonely", "/a/lonely", None)]);
        let out = p.query(&Query::new("repo")).await;
        // Remoteless repos shouldn't even offer action, but the
        // dispatch path should be defensive anyway - test it directly
        let effect = p.activate(&out[0].id, "copy-remote-url").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert!(s.contains("no remote"), "s={s}"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }
}
