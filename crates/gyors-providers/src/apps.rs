//! MacOS application launcher provider
//!
//! Shallow scan of the standard app directories, Info.plist parsing for
//! display name + bundle id, dedupe by bundle id, FSEvents watcher for
//! re-indexing on change

use anyhow::Result;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub path: PathBuf,
    pub display_name: String,
    pub bundle_id: Option<String>,
}

pub struct AppsProvider {
    state: Arc<ArcSwap<Vec<AppEntry>>>,
    /// Pre-materialised candidate list. Rebuilt whenever the scanner
    /// replaces `state` (FSEvents watcher callback), never on the hot
    /// per-keystroke path. Queries clone-out the current snapshot
    candidates: Arc<ArcSwap<Vec<Candidate>>>,
    _watcher: Option<RecommendedWatcher>,
}

impl AppsProvider {
    pub async fn new() -> Result<Self> {
        let initial = tokio::task::spawn_blocking(scan_all).await?;
        let cands = build_candidates(&initial);
        let state = Arc::new(ArcSwap::from(Arc::new(initial)));
        let candidates = Arc::new(ArcSwap::from(Arc::new(cands)));
        let watcher = spawn_watcher(Arc::clone(&state), Arc::clone(&candidates)).ok();
        Ok(Self { state, candidates, _watcher: watcher })
    }

    pub fn apps(&self) -> Vec<AppEntry> {
        self.state.load().as_ref().clone()
    }

    pub fn len(&self) -> usize {
        self.state.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Build candidate list once from a slice of scanned apps. Called
/// at init and from the FSEvents callback - NEVER in a query path
fn build_candidates(entries: &[AppEntry]) -> Vec<Candidate> {
    entries
        .iter()
        .map(|a| Candidate {
            id: candidate_id_for(&a.path),
            title: a.display_name.clone(),
            subtitle: a.bundle_id.clone(),
            icon: Icon::BundleIcon(a.path.clone()),
            kind: CandidateKind::App,
            actions: vec![
                Action::primary("Open"),
                Action::new("reveal", "Show in Finder"),
            ],
            search_text: a.display_name.clone(),
            bypass_rank: false,
        })
        .collect()
}

#[async_trait]
impl Provider for AppsProvider {
    fn id(&self) -> &str { "apps" }

    async fn query(&self, _query: &Query) -> Vec<Candidate> {
        // Zero allocation for the Candidate fields - we clone the
        // already-built Vec rather than rebuilding from `AppEntry`.
        // Still pays one deep-copy per Candidate (String clones), but
        // that's half the cost of the prior `.map(|a| Candidate {...})`
        // which also paid the `format!` and `Action::primary` builds
        self.candidates.load().as_ref().clone()
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        let path = id
            .strip_prefix("apps::")
            .ok_or_else(|| anyhow::anyhow!("invalid apps candidate id: {id}"))?;
        let path_buf = PathBuf::from(path);
        match action {
            "default" => Ok(Effect::OpenPath(path_buf)),
            "reveal" => Ok(Effect::RevealInFinder(path_buf)),
            other => anyhow::bail!("unknown action for apps: {other}"),
        }
    }
}

fn candidate_id_for(path: &Path) -> CandidateId {
    format!("apps::{}", path.display())
}

fn scan_all() -> Vec<AppEntry> {
    scan_app_bundles()
}

/// Public scan of every `.app` bundle under canonical app
/// folders. Used by `AppsProvider` at startup *and* by `ConfigProvider`
/// to drive searchable picker for `App`-typed config fields
/// (e.g. `terminal_app`). Cheap on modern Macs (low ms), but if the
/// caller needs many lookups they should cache result themselves
pub fn scan_app_bundles() -> Vec<AppEntry> {
    let mut out = Vec::new();
    for dir in app_dirs() {
        if dir.exists() {
            scan_dir(&dir, &mut out, 0);
        }
    }
    dedupe(&mut out);
    out.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });
    out
}

fn app_dirs() -> Vec<PathBuf> {
    let mut v = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = dirs::home_dir() {
        v.push(home.join("Applications"));
    }
    v
}

fn scan_dir(dir: &Path, out: &mut Vec<AppEntry>, depth: usize) {
    const MAX_DEPTH: usize = 2;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_app_bundle = path.extension().and_then(|s| s.to_str()) == Some("app");
        if is_app_bundle {
            if let Some(app) = parse_bundle(&path) {
                out.push(app);
            }
            continue;
        }
        if depth < MAX_DEPTH {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    scan_dir(&path, out, depth + 1);
                }
            }
        }
    }
}

fn parse_bundle(path: &Path) -> Option<AppEntry> {
    let info_path = path.join("Contents/Info.plist");
    let value = plist::Value::from_file(&info_path).ok()?;
    let dict = value.as_dictionary()?;
    let display_name = dict
        .get("CFBundleDisplayName")
        .and_then(|v| v.as_string())
        .or_else(|| dict.get("CFBundleName").and_then(|v| v.as_string()))
        .map(String::from)
        .or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(String::from)
        })?;
    let bundle_id = dict
        .get("CFBundleIdentifier")
        .and_then(|v| v.as_string())
        .map(String::from);
    Some(AppEntry {
        path: path.to_path_buf(),
        display_name,
        bundle_id,
    })
}

fn dedupe(apps: &mut Vec<AppEntry>) {
    let mut seen = HashSet::new();
    apps.retain(|a| {
        let key = a
            .bundle_id
            .clone()
            .unwrap_or_else(|| a.path.to_string_lossy().into_owned());
        seen.insert(key)
    });
}

fn spawn_watcher(
    state: Arc<ArcSwap<Vec<AppEntry>>>,
    candidates: Arc<ArcSwap<Vec<Candidate>>>,
) -> Result<RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    for dir in app_dirs() {
        if dir.exists() {
            if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                tracing::warn!("watch {}: {e}", dir.display());
            }
        }
    }

    std::thread::spawn(move || loop {
        if rx.recv().is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
        while rx.try_recv().is_ok() {}
        let fresh = scan_all();
        tracing::debug!("apps reindexed: {} entries", fresh.len());
        // Rebuild candidate cache in watcher thread, off the
        // hot path. `ArcSwap::store` is atomic - readers either see
        // the old snapshot or the new one, never a torn state
        let fresh_cands = build_candidates(&fresh);
        state.store(Arc::new(fresh));
        candidates.store(Arc::new(fresh_cands));
    });

    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_app(dir: &Path, folder_name: &str, bundle_id: Option<&str>, display_name: Option<&str>) -> PathBuf {
        let app_path = dir.join(format!("{folder_name}.app"));
        let contents = app_path.join("Contents");
        fs::create_dir_all(&contents).unwrap();
        let mut dict = String::new();
        if let Some(name) = display_name {
            dict.push_str(&format!(
                "  <key>CFBundleDisplayName</key>\n  <string>{name}</string>\n"
            ));
        }
        if let Some(id) = bundle_id {
            dict.push_str(&format!(
                "  <key>CFBundleIdentifier</key>\n  <string>{id}</string>\n"
            ));
        }
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
{dict}</dict>
</plist>
"#
        );
        fs::write(contents.join("Info.plist"), plist).unwrap();
        app_path
    }

    // parse_bundle

    #[test]
    fn parse_bundle_reads_display_name_and_id() {
        let td = TempDir::new().unwrap();
        let path = write_app(td.path(), "Fake", Some("com.example.fake"), Some("Fake App"));
        let entry = parse_bundle(&path).expect("parsed");
        assert_eq!(entry.display_name, "Fake App");
        assert_eq!(entry.bundle_id.as_deref(), Some("com.example.fake"));
        assert_eq!(entry.path, path);
    }

    #[test]
    fn parse_bundle_falls_back_to_bundle_name_then_stem() {
        let td = TempDir::new().unwrap();
        // No CFBundleDisplayName - fall back to file stem since theres no CFBundleName either
        let path = write_app(td.path(), "Bare", None, None);
        let entry = parse_bundle(&path).expect("parsed");
        assert_eq!(entry.display_name, "Bare");
        assert_eq!(entry.bundle_id, None);
    }

    #[test]
    fn parse_bundle_missing_plist_is_none() {
        let td = TempDir::new().unwrap();
        let app_path = td.path().join("NoPlist.app");
        fs::create_dir_all(&app_path).unwrap();
        assert!(parse_bundle(&app_path).is_none());
    }

    #[test]
    fn parse_bundle_malformed_plist_is_none() {
        let td = TempDir::new().unwrap();
        let app_path = td.path().join("Broken.app");
        let contents = app_path.join("Contents");
        fs::create_dir_all(&contents).unwrap();
        fs::write(contents.join("Info.plist"), "this is not xml").unwrap();
        assert!(parse_bundle(&app_path).is_none());
    }

    // scan_dir

    #[test]
    fn scan_dir_finds_app_bundles() {
        let td = TempDir::new().unwrap();
        write_app(td.path(), "A1", Some("id.a1"), Some("A1"));
        write_app(td.path(), "A2", Some("id.a2"), Some("A2"));
        let mut out = Vec::new();
        scan_dir(td.path(), &mut out, 0);
        assert_eq!(out.len(), 2);
        let names: Vec<_> = out.iter().map(|e| e.display_name.clone()).collect();
        assert!(names.contains(&"A1".to_string()));
        assert!(names.contains(&"A2".to_string()));
    }

    #[test]
    fn scan_dir_does_not_descend_into_bundles() {
        let td = TempDir::new().unwrap();
        let outer = write_app(td.path(), "Outer", Some("id.outer"), Some("Outer"));
        // Place a fake inner app inside the outer .app - should NOT be found
        write_app(&outer.join("Contents"), "Inner", Some("id.inner"), Some("Inner"));
        let mut out = Vec::new();
        scan_dir(td.path(), &mut out, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].display_name, "Outer");
    }

    #[test]
    fn scan_dir_walks_utilities_subdir() {
        // The real /Applications/Utilities case: one level of nesting
        let td = TempDir::new().unwrap();
        let util = td.path().join("Utilities");
        fs::create_dir_all(&util).unwrap();
        write_app(&util, "Nested", Some("id.nested"), Some("Nested"));
        let mut out = Vec::new();
        scan_dir(td.path(), &mut out, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].display_name, "Nested");
    }

    #[test]
    fn scan_dir_handles_unreadable_dir() {
        let mut out = Vec::new();
        scan_dir(Path::new("/nonexistent-gyors-test-path-xyzzy"), &mut out, 0);
        assert!(out.is_empty());
    }

    // dedupe

    #[test]
    fn dedupe_removes_duplicates_by_bundle_id_keeping_first() {
        let mut apps = vec![
            AppEntry { path: "/a".into(), display_name: "A".into(), bundle_id: Some("x".into()) },
            AppEntry { path: "/b".into(), display_name: "B".into(), bundle_id: Some("x".into()) },
            AppEntry { path: "/c".into(), display_name: "C".into(), bundle_id: Some("y".into()) },
        ];
        dedupe(&mut apps);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].display_name, "A");
        assert_eq!(apps[1].display_name, "C");
    }

    #[test]
    fn dedupe_uses_path_when_no_bundle_id() {
        let mut apps = vec![
            AppEntry { path: "/a".into(), display_name: "A".into(), bundle_id: None },
            AppEntry { path: "/a".into(), display_name: "Aprime".into(), bundle_id: None },
            AppEntry { path: "/b".into(), display_name: "B".into(), bundle_id: None },
        ];
        dedupe(&mut apps);
        assert_eq!(apps.len(), 2);
    }

    #[test]
    fn dedupe_empty_is_noop() {
        let mut apps: Vec<AppEntry> = vec![];
        dedupe(&mut apps);
        assert!(apps.is_empty());
    }

    // candidate_id_for

    #[test]
    fn candidate_id_has_apps_prefix() {
        let id = candidate_id_for(&PathBuf::from("/Applications/Safari.app"));
        assert_eq!(id, "apps::/Applications/Safari.app");
    }

    // Provider impl

    fn provider_with(apps: Vec<AppEntry>) -> AppsProvider {
        let candidates = build_candidates(&apps);
        AppsProvider {
            state: Arc::new(ArcSwap::from(Arc::new(apps))),
            candidates: Arc::new(ArcSwap::from(Arc::new(candidates))),
            _watcher: None,
        }
    }

    #[tokio::test]
    async fn query_emits_candidate_per_app() {
        let p = provider_with(vec![
            AppEntry { path: "/A.app".into(), display_name: "A".into(), bundle_id: Some("id.a".into()) },
            AppEntry { path: "/B.app".into(), display_name: "B".into(), bundle_id: None },
        ]);
        let out = p.query(&Query::new("anything")).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, CandidateKind::App);
        assert_eq!(out[0].title, "A");
        assert_eq!(out[0].subtitle.as_deref(), Some("id.a"));
        assert_eq!(out[1].subtitle, None);
    }

    #[tokio::test]
    async fn activate_produces_openpath() {
        let p = provider_with(vec![]);
        let eff = p
            .activate(&"apps::/Applications/Safari.app".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::OpenPath(p) => assert_eq!(p, PathBuf::from("/Applications/Safari.app")),
            other => panic!("expected OpenPath, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_rejects_foreign_id() {
        let p = provider_with(vec![]);
        assert!(p.activate(&"calc::4".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn len_and_is_empty() {
        let empty = provider_with(vec![]);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let full = provider_with(vec![
            AppEntry { path: "/a".into(), display_name: "A".into(), bundle_id: None },
        ]);
        assert!(!full.is_empty());
        assert_eq!(full.len(), 1);
    }
}
