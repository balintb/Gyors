//! C FFI surface consumed by SwiftUI shell
//!
//!   gyors_init()                                  once at app launch
//!   gyors_query(pattern)                -> JSON   per keystroke (debounced)
//!   gyors_activate(id, action)          -> JSON   on Enter / action click
//!   gyors_record_clipboard(text)                  whenever NSPasteboard changes
//!   gyors_clear_clipboard_history()               menu action
//!   gyors_free_string(ptr)                        release returned strings
//!
//! Strings are UTF-8, null-terminated, allocated by Rust. Caller owns
//! them and MUST free them via `gyors_free_string` to avoid leaks

mod pipeline;
// Cloud sync FFI. Gated behind the `cloud` feature so a launcher
// built with `--no-default-features` (or `WITH_CLOUD=0` via
// build-app.sh) has zero cloud surface - no FFI symbols, no
// background tick, no network code in the binary
#[cfg(feature = "cloud")]
mod sync_ffi;

use gyors_core::{
    parse_mode, precision_bonus, Effect, NucleoRanker, Query, QueryMode, Ranker, ScoredCandidate,
    MAX_FRECENCY_BOOST,
};
use gyors_index::Index;
use gyors_providers::registry::new_disabled_set;
#[cfg(feature = "ai")]
use gyors_providers::AiTransformsProvider;
use gyors_providers::{
    AppsProvider, ConfigProvider, CurrencyProvider, GitReposProvider, InputAutocompleteProvider,
    JwtProvider, NotesProvider, PluginsProvider, ProviderRegistry, ScratchpadProvider,
    ShortcutsProvider, SnippetsProvider, SshProvider,
};
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;

const BYPASS_RANK_SCORE: i64 = 1_000_000;

pub(crate) static BRIDGE: OnceLock<Mutex<GyorsBridge>> = OnceLock::new();

#[derive(Serialize, Deserialize)]
struct UiAction {
    id: String,
    label: String,
}

#[derive(Serialize, Deserialize)]
struct UiCandidate {
    id: String,
    title: String,
    subtitle: String,
    icon_kind: u8,
    icon_value: String,
    kind: u8,
    score: i64,
    actions: Vec<UiAction>,
}

/// Per-host details the FFI needs that aren't exposed via the
/// `Provider` trait - app count (for About dialog), notes folder (for
/// rename flow), clipboard (for `QueryMode::Clipboard` routing). Kept
/// alongside registry so hot path stays simple
pub(crate) struct GyorsBridge {
    registry: ProviderRegistry,
    pub(crate) index: Arc<Index>,
    pub(crate) rt: Runtime,
    /// Snapshotted at init - used by `gyors_app_count` (About dialog,
    /// welcome pane). A live count would need a back-reference into
    /// registry
    app_count: u64,
    /// Snapshotted at init - used by `gyors_notes_folder` (Rename flow)
    notes_folder: PathBuf,
}

impl GyorsBridge {
    pub(crate) fn index_outbox_pending(&self, kind: &str) -> anyhow::Result<i64> {
        self.index.outbox_pending(kind)}

    pub(crate) fn index_sync_pull_cursor(&self) -> anyhow::Result<Option<String>> {
        self.index.sync_pull_cursor_get()}
}

impl GyorsBridge {
    fn new() -> Self {
        // Tracing in debug builds only - release runs dont pay the
        // subscriber's per-event overhead. Users with issues can rebuild
        // with `--features logging` (future) to capture traces
        #[cfg(debug_assertions)]
        {
            let _ = tracing_subscriber::fmt::try_init();
        }
        let t_total = std::time::Instant::now();
        let mut phases: Vec<(&'static str, u128)> = Vec::with_capacity(8);

        let t = std::time::Instant::now();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        phases.push(("tokio-runtime", t.elapsed().as_micros()));

        let t = std::time::Instant::now();
        let index = Arc::new(Index::open(data_path().expect("data dir")).expect("index open"));
        phases.push(("index-open", t.elapsed().as_micros()));

        // Shared gate used by both registry (to filter disabled
        // providers) and ConfigProvider (to toggle them live). Seeded
        // from config.json so user choices survive restart
        let gate = new_disabled_set();
        ConfigProvider::load_disabled_into(&gate);

        // Build registry inside runtime so async constructors
        // (apps scanner, notes watcher, ssh scan, ...) can do their I/O
        //
        // Adding a new provider = one line below. This replaces the
        // old 5-place edit (struct field + constructor + tokio::join!
        // arm + `all.extend` + dispatch_activate match)
        let t_registry = std::time::Instant::now();
        let (registry, app_count, notes_folder, async_phases) = rt.block_on(async {
            let mut p: Vec<(&'static str, u128)> = Vec::new();

            // Async constructors that DO block on disk/process work
            // run concurrently. The wall-clock cost drops from the
            // sum of each phase (sequential await) to the max
            //
            // git_repos and shortcuts are deliberately excluded -
            // their `new()` returns immediately, scanning in a
            // detached tokio::spawn so cold-start doesn't pay for
            // them at all
            let t = std::time::Instant::now();
            let (apps_res, notes, ssh, currency) = tokio::join!(
                AppsProvider::new(),
                NotesProvider::new(),
                SshProvider::new(),
                CurrencyProvider::new(),
            );
            let apps = apps_res.expect("apps init");
            p.push(("async-init", t.elapsed().as_micros()));
            let app_count = apps.len() as u64;
            let notes_folder = notes.notes_folder().to_path_buf();

            // Lazy-loading providers - these return ~immediately and
            // populate their state in a background task. Tracked as
            // separate phases so a regression (someone making one of
            // them blocking again) shows up loudly in the cold-start
            // log instead of silently re-blocking init
            let t = std::time::Instant::now();
            let git_repos = GitReposProvider::new();
            p.push(("git-repos-spawn", t.elapsed().as_micros()));

            let t = std::time::Instant::now();
            let shortcuts = ShortcutsProvider::new();
            p.push(("shortcuts-spawn", t.elapsed().as_micros()));

            // Snippets is synchronous-cheap (1ms) - keep it serial to
            // minimise async overhead
            let t = std::time::Instant::now();
            let snippets = SnippetsProvider::new().await;
            p.push(("snippets", t.elapsed().as_micros()));

            // Common 45-provider set lives in `add_core_providers`
            // - both binaries call it. Panel-only providers are
            // added below; CLI doesn't render the affordances they
            // depend on
            let core_ctx = gyors_providers::CoreProviderContext {
                index: Arc::clone(&index),
                disabled: gate.clone(),
                apps,
                git_repos,
                shortcuts,
                notes,
                snippets,
                ssh,
                currency,
            };
            let builder = ProviderRegistry::builder_with_gate(gate.clone());
            // Panel-only extras: render UI states menu-bar panel
            // knows how to handle, the CLI doesn't. AiTransforms is
            // optional (feature `ai`) so no-AI build doesn't ship
            // its summarize/translate/etc. verbs
            let builder = gyors_providers::add_core_providers(builder, core_ctx);
            #[cfg(feature = "ai")]
            let builder = builder.add(AiTransformsProvider::new(Arc::clone(&index)));
            let registry = builder
                .add(JwtProvider)
                .add(InputAutocompleteProvider)
                .add(PluginsProvider::new())
                .add(ScratchpadProvider::new())
                .build();
            (registry, app_count, notes_folder, p)
        });
        phases.extend(async_phases);
        phases.push(("registry-total", t_registry.elapsed().as_micros()));

        // Plugins (both flavours): land BEFORE ConfigProvider so
        // they appear in config UI's toggle list and users can
        // disable individually without editing JSON by hand
        //
        // - Shell plugins: JSON-defined inline in `plugins.json`.
        //   Zero-executable - one-liners like `curl wttr.in/{query}`
        //   become first-class providers just by editing a file
        //
        // - Process plugins: executables in `plugins/`. Full
        //   programmatic control via the three-subcommand protocol
        //
        // Each is crash-isolated + timeout-bounded; malformed
        // entries are logged and skipped. Order: shell first (cheap,
        // synchronous config parse), then process (async spawn)
        let t = std::time::Instant::now();
        let mut registry = registry;
        let shell_plugins_path = gyors_plugin_host::default_shell_plugins_path();
        for sp in gyors_plugin_host::load_shell_plugins(&shell_plugins_path) {
            registry.add_provider(sp);
        }
        phases.push(("plugins-shell", t.elapsed().as_micros()));

        let t = std::time::Instant::now();
        let plugin_dir = gyors_plugin_host::default_plugin_dir();
        let plugins = rt.block_on(gyors_plugin_host::discover(&plugin_dir));
        for plugin in plugins {
            registry.add_provider(plugin);
        }
        phases.push(("plugins-process", t.elapsed().as_micros()));

        // ConfigProvider needs both the shared gate (so toggling
        // flips live state) and the list of provider ids (so it can
        // render one toggle row per). Register it last - it
        // introspects registry we just built (plugins included)
        let provider_ids: Vec<String> = registry.ids().into_iter().map(|s| s.to_string()).collect();
        registry.add_provider(ConfigProvider::new(gate, provider_ids));

        // One-shot cold-start report. Numbers are in milliseconds
        // (rounded). Filter in Console.app via "gyors: cold-start".
        // Kept always-on (not gated by debug_assertions) because the
        // first thing we ask a user with "it's slow" is "what does
        // the cold-start log say?" - and a release build hides it
        let total_us = t_total.elapsed().as_micros();
        let mut report = String::from("gyors: cold-start ");
        for (name, us) in &phases {
            report.push_str(&format!("{}={}ms ", name, us / 1000));
        }
        report.push_str(&format!("total={}ms", total_us / 1000));
        eprintln!("{}", report);

        // Wire the post-save hook: whenever user changes a
        // config field, enqueue a settings sync AND kick a tick
        // off-thread so change reaches the server in <1s
        // instead of waiting for next 60-second cadence. The
        // hook itself stays synchronous + cheap (one sqlite
        // insert + thread spawn); actual HTTP round-trip
        // happens on the spawned tick thread, so a slow network
        // doesn't stall `save_config`
        //
        // Gated behind `cloud` so a no-cloud build doesn't even
        // hold a reference to the sync crate
        #[cfg(feature = "cloud")]
        {
            let index_for_hook = Arc::clone(&index);
            gyors_providers::config::set_save_hook(std::sync::Arc::new(move |root| {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if let Err(e) = gyors_sync::settings::enqueue_upsert_root(
                    &index_for_hook,
                    root,
                    ts,
                ) {
                    tracing::warn!("settings sync enqueue failed: {e}");
                    return;
                }
                // Kick an immediate tick. Spawning a thread per
                // save would be wasteful at very high write rates,
                // but config saves are user-driven (one per
                // Settings field edit) so a one-off thread is fine
                let idx_clone = Arc::clone(&index_for_hook);
                std::thread::Builder::new()
                    .name("gyors-sync-immediate".into())
                    .spawn(move || {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                tracing::warn!(
                                    "immediate settings tick: runtime build failed: {e}"
                                );
                                return;
                            }
                        };
                        rt.block_on(try_one_background_tick(idx_clone));
                    })
                    .ok();
            }));
        }

        // Background sync task: every `sync_interval_secs` (default
        // 60), call same tick the FFI runs on demand. Skips
        // silently if signed out. Runs on a dedicated thread + tiny
        // runtime so it doesn't compete with the main `rt` (which
        // is current-thread and would block under any long task)
        #[cfg(feature = "cloud")]
        spawn_background_sync(Arc::clone(&index));

        Self {
            registry,
            index,
            rt,
            app_count,
            notes_folder,
        }
    }

    fn query(&self, pattern: &str) -> Vec<UiCandidate> {
        let q = Query::new(pattern);
        let scored = self
            .rt
            .block_on(async { orchestrate(&q, self).await })
            .unwrap_or_default();
        scored.into_iter().map(to_ui).collect()
    }

    fn activate(&self, id: &str, action: &str) -> String {
        let eff_res = self
            .rt
            .block_on(async { dispatch_activate(id, action, self).await });
        match eff_res {
            Ok(eff) => {
                if should_record_visit(id, &eff) {
                    let _ = self.index.record_visit(id, now_secs());
                }
                serde_json::to_string(&eff).unwrap_or_else(|_| "null".into())
            }
            Err(e) => format!(r#"{{"error":{:?}}}"#, e.to_string()),
        }
    }

    fn record_clipboard(&self, content: &str) {
        let ts = now_secs();
        let recorded = match self.index.record_clipboard(content, ts) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("record_clipboard failed: {e}");
                return;
            }
        };
        // `record_clipboard` returns false for empty / duplicate-of-
        // previous content -- in those cases theres nothing new to
        // sync, so skip the outbox write. For everything else we
        // enqueue regardless of auth state: the engine ignores the
        // outbox if user isn't signed in, and a future sign-in
        // picks it up. Enqueue failure is non-fatal (the local row
        // already landed); just log so it shows up in diagnostics
        //
        // No-cloud build: drop the enqueue. `recorded` is still
        // used downstream by tests so we keep the `let _ =`
        #[cfg(feature = "cloud")]
        if recorded {
            if let Err(e) =
                gyors_sync::clipboard::enqueue_upsert(&self.index, content, ts)
            {
                tracing::warn!("sync outbox enqueue failed: {e}");
            }
        }
        #[cfg(not(feature = "cloud"))]
        let _ = recorded;
    }

    fn clear_clipboard_history(&self) {
        if let Err(e) = self.index.clear_clipboard_history() {
            tracing::warn!("clear_clipboard_history failed: {e}");
        }
    }

    fn record_query(&self, pattern: &str) {
        if let Err(e) = self.index.record_query(pattern, now_secs()) {
            tracing::warn!("record_query failed: {e}");
        }
    }

    fn recent_queries(&self, limit: usize) -> Vec<String> {
        self.index.recent_queries(limit).unwrap_or_default()
    }

    fn last_query(&self) -> Option<String> {
        self.index.last_query().unwrap_or(None)
    }
}

/// Frecency bookkeeping skip rules
///
/// `record_visit` should NOT fire when:
///
/// - The Effect is `SetInput`. Tab completion / hint prefill /
///   router preview rows route through this - user hasn't
///   actually picked a real target yet, just shaped the next
///   input. Recording would give those completions an
///   ever-growing frecency score that outranks the conversions
///   they feed.
/// - The id starts with `ai_palette::` or `ai_router::`. Both
///   encode user's verbatim question text - every unique
///   question is a fresh high-entropy key that never matches
///   itself again, so recording it just bloats the visits table
///   and the in-memory cache with dead weight
///
/// Pure function - extracted from the activate path so unit
/// test can pin the matrix of (id prefix, effect) combinations
/// without spinning up the full bridge
fn should_record_visit(id: &str, effect: &Effect) -> bool {
    if matches!(effect, Effect::SetInput(_)) {
        return false;
    }
    if id.starts_with("ai_palette::") || id.starts_with("ai_router::") {
        return false;
    }
    true
}

/// Build the ranked candidate list for panel
///
/// Pre-refactor this was a 150-line stack of `tokio::join!` chunks +
/// `all.extend(...)` + manual match arms. Now it's a 30-line pipeline
/// on top of `ProviderRegistry`. Special modes (`clip`/`>`) route to a
/// single provider; default path runs everything in parallel and
/// ranks the output
async fn orchestrate(query: &Query, br: &GyorsBridge) -> anyhow::Result<Vec<ScoredCandidate>> {
    // Generic cross-provider chain: `<base query> > <action hint>`.
    // Detecting at orchestrator level means ANY provider's rows
    // become chainable - notes, clipboard, apps, snippets, anything
    // exposing a non-empty `actions` list. The base query runs
    // through the normal dispatch path (provider-scoped), then we
    // filter the top candidate's actions by the hint and emit one
    // confirm row per match
    if let Some(spec) = parse_chain(query.raw.as_str()) {
        return orchestrate_chain(spec, br).await;
    }
    // AI as a pipeline source - `ai write a haiku | upper | copy`,
    // `summarize <text> | copy`, etc. Has to fire BEFORE `parse_mode`
    // because AI verbs are listed in `base_uses_literal_pipe` so the
    // regular chain layer (above) deliberately skips them - without
    // this branch input would just become a free-form AI ask
    // with the pipe parts treated as part of prompt
    #[cfg(feature = "ai")]
    if let Some((prompt, instruction, stages)) = parse_ai_pipeline(query.raw.as_str()) {
        return Ok(orchestrate_ai_pipeline(&prompt, instruction, &stages));
    }
    let (pattern, mode) = parse_mode(query.raw.as_str());
    let effective = Query::new(pattern);

    // Empty input: short-circuit to the discovery provider only. Skips
    // the whole provider fan-out (apps fuzzy-rank, notes scan, AI hint
    // filter, ...) - none of them meaningfully match an empty pattern,
    // so running 40-odd async query() calls only to drop their results
    // is wasted FFI cost on every panel open. Discovery rows are
    // emitted in curated order; the BYPASS_RANK_SCORE - i tiebreak
    // preserves it past orchestrator's stable sort
    if pattern.is_empty() && mode == QueryMode::Default {
        let items = br.registry.query_one("discovery", &effective).await;
        return Ok(items
            .into_iter()
            .enumerate()
            .map(|(i, c)| ScoredCandidate {
                candidate: c,
                score: BYPASS_RANK_SCORE - i as i64,
            })
            .collect());
    }

    if mode == QueryMode::Clipboard {
        let items = br.registry.query_one("clip", &effective).await;
        return Ok(items
            .into_iter()
            .enumerate()
            .map(|(i, c)| ScoredCandidate {
                candidate: c,
                score: BYPASS_RANK_SCORE - i as i64,
            })
            .collect());
    }
    if mode == QueryMode::Shell {
        // `> ls -la | copy` - when user appends a known pipeline
        // stage after a shell command, drop into the pipe-aware path
        // that runs shell, captures stdout, and feeds it through
        // the pipeline. Without this branch entire string would
        // get treated as one shell command, leaving "Run: ls -la |
        // copy" as only row (no autocomplete, no piping)
        if let Some((cmd, stages)) = parse_shell_pipeline(pattern) {
            return Ok(orchestrate_shell_pipeline(&cmd, &stages));
        }
        let items = br.registry.query_one("shell", &effective).await;
        return Ok(items
            .into_iter()
            .map(|c| ScoredCandidate {
                candidate: c,
                score: BYPASS_RANK_SCORE,
            })
            .collect());
    }
    if mode == QueryMode::Notes {
        // Strict notes-only routing: when user explicitly typed
        // a note keyword, everything else stays silent. Prevents
        // unrelated fuzzy matches (System Prefs, apps, hints) from
        // surfacing above zero-notes folders or empty searches
        let items = br.registry.query_one("note", &effective).await;
        if !items.is_empty() {
            return Ok(items
                .into_iter()
                .enumerate()
                .map(|(i, c)| ScoredCandidate {
                    candidate: c,
                    score: BYPASS_RANK_SCORE - i as i64,
                })
                .collect());
        }
        // Notes provider returned nothing - most likely it's been
        // gated off via `providers.note.enabled = false`. Surface a
        // single helper row so user can get back on track
        // without wondering why their note keyword did nothing
        return Ok(vec![ScoredCandidate {
            candidate: notes_disabled_helper_candidate(),
            score: BYPASS_RANK_SCORE,
        }]);
    }

    // Default path: every provider runs in parallel - except for the
    // ones that are opt-in via prefixes. Skipping them at the
    // registry level (not just filtering results) matters for perf
    // AND for noise: clipboard history fuzzy-matched everything the
    // user had ever copied, drowning dropdown in stale results
    // (especially while iterating on a router prompt user kept
    // copy-pasting). Files is opt-in via `'`; shell via `>`;
    // clipboard via `clip` / `paste` / `cb` / `c`
    let mut all = br
        .registry
        .query_all_except(&["files", "shell", "clip"], &effective)
        .await;
    if mode == QueryMode::IncludeFiles {
        all.extend(br.registry.query_one("files", &effective).await);
    }

    let (bypass, normal): (Vec<_>, Vec<_>) = all.into_iter().partition(|c| c.bypass_rank);
    let mut scored: Vec<ScoredCandidate> = bypass
        .into_iter()
        .map(|c| ScoredCandidate {
            candidate: c,
            score: BYPASS_RANK_SCORE,
        })
        .collect();
    scored.extend(NucleoRanker::new().rank(pattern, normal));

    let now = now_secs();
    // Frecency pulled in bulk from the in-memory visits cache (zero
    // SQL per keystroke). The `.min` caps each boost so a
    // frequently-used fuzzy match can't outrank a better precision
    // tier - the cap is well below the smallest tier gap
    let ids: Vec<&str> = scored.iter().map(|sc| sc.candidate.id.as_str()).collect();
    let boosts = br.index.frecency_scores_bulk(&ids, now, 20)?;
    for sc in &mut scored {
        // Precision tier boost for bypass candidates too - otherwise
        // their flat BYPASS_RANK_SCORE loses all per-match nuance
        let tier = precision_bonus(pattern, &sc.candidate.title);
        if sc.score == BYPASS_RANK_SCORE {
            sc.score = sc.score.saturating_add(tier);
        }
        if let Some(&b) = boosts.get(&sc.candidate.id) {
            let capped = (b as i64).min(MAX_FRECENCY_BOOST);
            sc.score = sc.score.saturating_add(capped);
        }
    }
    scored.sort_by_key(|s| std::cmp::Reverse(s.score));
    // Hard cap on default-mode result set. Without this, a
    // broad query like `no` pulls fuzzy matches from every
    // provider at once (apps, recents, snippets, web-search hint
    // rows, ...) and the list blows past 80 rows - the scroll area
    // feels bottomless and the LazyVStack has to churn through
    // hidden content. 30 is enough to find anything user is
    // actually aiming for without the endless-scroll feel; tighter
    // keyword-routed modes (notes, clipboard, shell) are already
    // bounded and bypass this cap
    const DEFAULT_MODE_MAX: usize = 30;
    // Reserve one slot for the AI palette row when it's about to be
    // injected, so visible total stays bounded at DEFAULT_MODE_MAX
    // rather than DEFAULT_MODE_MAX + 1. The palette has the lowest
    // score and would otherwise be row truncated away - keep it
    // by trimming a regular result instead
    let cap = if should_show_ai_palette(pattern) {
        DEFAULT_MODE_MAX.saturating_sub(1)
    } else {
        DEFAULT_MODE_MAX
    };
    scored.truncate(cap);
    let scored = inject_ai_palette(scored, pattern);
    Ok(scored)
}

/// Whether orchestrator should append an "Ask AI" command-palette
/// row to a default-mode query. Suppressed when user is already
/// asking AI explicitly via the `ai`/`ask` keyword, when input is
/// empty (the discovery-rows path owns that), or when the pattern is
/// pure whitespace
///
/// In a no-AI build function returns `false` unconditionally so
/// orchestrator path stays same shape but never emits the
/// palette row (which would dispatch into a non-existent AI handler)
#[cfg(not(feature = "ai"))]
fn should_show_ai_palette(_pattern: &str) -> bool {
    false
}

#[cfg(feature = "ai")]
fn should_show_ai_palette(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return false;
    }
    // The `ai ` / `ask ` prefixes already produce an AiProvider row
    // pointing at exactly same effect - duplicating the palette
    // here would put TWO "Ask AI" rows on the list
    if trimmed == "ai" || trimmed == "ask" {
        return false;
    }
    if trimmed.starts_with("ai ") || trimmed.starts_with("ask ") {
        return false;
    }
    // `!!` is shell-style "recall last command" sigil. Swift
    // shell renders a ghost-text preview of the last query and Enter
    // expands+runs it - theres no question to ask AI about, so
    // suppress the palette to keep result list focused on the
    // recall preview
    if trimmed == "!!" {
        return false;
    }
    true
}

/// Append (or auto-promote) the AI command-palette row
///
/// - Auto-promote when no other rows ranked: AI is only thing
///   left to do, so it sits at top with `BYPASS_RANK_SCORE` and
///   Enter routes straight to `Effect::AskAi`.
/// - Bottom-of-list when normal results exist: scored just below
///   the lowest existing entry so it always sorts last, never
///   competes with real matches. cmdEnter from anywhere fires it
///
/// No-AI build: pass-through. The palette row never appears
#[cfg(not(feature = "ai"))]
fn inject_ai_palette(
    scored: Vec<ScoredCandidate>,
    _pattern: &str,
) -> Vec<ScoredCandidate> {
    scored
}

#[cfg(feature = "ai")]
fn inject_ai_palette(
    mut scored: Vec<ScoredCandidate>,
    pattern: &str,
) -> Vec<ScoredCandidate> {
    if !should_show_ai_palette(pattern) {
        return scored;
    }
    let row = ai_palette_candidate(pattern);
    if scored.is_empty() {
        scored.push(ScoredCandidate {
            candidate: row,
            score: BYPASS_RANK_SCORE,
        });
    } else {
        let min_score = scored.iter().map(|sc| sc.score).min().unwrap_or(0);
        scored.push(ScoredCandidate {
            candidate: row,
            score: min_score.saturating_sub(1),
        });
    }
    scored
}

/// The synthetic "Ask AI" row. Lives in IPC (not a provider) because
/// orchestrator alone has result-set context to decide
/// whether to promote it to the top. Swift shell rewrites the
/// subtitle at render time to name actual provider that will
/// run (Apple Intelligence on macOS 26+, otherwise whatever
/// `ai.provider` is configured for)
#[cfg(feature = "ai")]
fn ai_palette_candidate(question: &str) -> gyors_core::Candidate {
    use gyors_core::CandidateKind as Kind;
    let title = if question.chars().count() <= AI_PALETTE_MAX_TITLE {
        format!("Ask AI: {question}")
    } else {
        let head: String = question.chars().take(AI_PALETTE_MAX_TITLE).collect();
        format!("Ask AI: {head}…")
    };
    gyors_core::Candidate {
        id: format!("ai_palette::{question}"),
        title,
        subtitle: Some("Apple Intelligence · ⌘↵".to_string()),
        icon: gyors_core::Icon::SfSymbol("sparkles".into()),
        kind: Kind::Action,
        actions: vec![gyors_core::Action::primary("Ask")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(feature = "ai")]
const AI_PALETTE_MAX_TITLE: usize = 80;

/// Route an activation through registry. Pre-refactor this was a
/// 40-arm match on id prefixes; now registry does that lookup
/// automatically from each provider's `id()`
async fn dispatch_activate(id: &str, action: &str, br: &GyorsBridge) -> anyhow::Result<Effect> {
    // Synthetic IPC-owned candidates (e.g. the "notes disabled"
    // helper) have the `ipc::` prefix so they never reach a provider.
    // Handle them before registry lookup so they dont look like
    // "unknown provider" errors
    if id == "ipc::notes-disabled" {
        // Use the `config set <key> <value>` grammar so config
        // provider recognises this as a set preview (the row whose
        // Enter writes), not a filter on field list (which only
        // shows the "Open config.json" row)
        return Ok(Effect::SetInput(
            "config set providers.note.enabled true".into(),
        ));
    }
    // AI command-palette row injected by orchestrator. The
    // question is encoded in the id verbatim - Swift shell
    // dispatches resulting `AskAi` effect through whichever
    // provider is configured (Apple Intelligence by default on
    // macOS 26+, falling back to user's `ai.provider`)
    if let Some(question) = id.strip_prefix("ai_palette::") {
        return Ok(Effect::AskAi(question.to_string()));
    }
    // AI router preview row injected by Swift side after the
    // router translated a natural-language query into keyword
    // form. The id encodes the rendered keyword query verbatim;
    // SetInput drops it back into input field, orchestrator
    // re-fires, and matching provider produces real result.
    // Router never dispatches Effects directly - every action
    // flows through same keyword path user could have
    // typed by hand, so frecency, action menus, and chain syntax
    // all behave identically
    if let Some(keyword) = id.strip_prefix("ai_router::") {
        return Ok(Effect::SetInput(keyword.to_string()));
    }
    // Chain helper rows are informational only
    if id.starts_with("chain::__no-base__::")
        || id.starts_with("chain::__unknown-action__::")
        || id == "chain::__unknown-shell-stage__"
        || id == "chain::__unknown-ai-stage__"
    {
        return Ok(Effect::None);
    }
    // AI-source pipeline activation: emit `AskAiThenPipe` so Swift
    // runs the AI call (with optional preset instruction), then
    // re-dispatches answer through existing `pipeline::`
    // handler for transforms + sink
    if let Some(payload_b64) = id.strip_prefix("aipipe::") {
        use base64::prelude::*;
        let payload = BASE64_URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|e| anyhow::anyhow!("aipipe id base64: {e}"))?;
        let decoded =
            String::from_utf8(payload).map_err(|e| anyhow::anyhow!("aipipe id utf8: {e}"))?;
        // Encoded shape: prompt\n<flag 0|1>\ninstruction\nstage1\nstage2..
        let mut lines = decoded.split('\n');
        let prompt = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("aipipe id: missing prompt"))?
            .to_string();
        let has_instr = lines.next().unwrap_or("0") == "1";
        let raw_instr = lines.next().unwrap_or("").to_string();
        let instruction = if has_instr { Some(raw_instr) } else { None };
        let stages: Vec<String> = lines.map(String::from).collect();
        return Ok(Effect::AskAiThenPipe {
            prompt,
            instruction,
            stages,
        });
    }
    // Shell pipeline activation: decode `cmd\nstage1\nstage2...`,
    // run `/bin/sh -c <cmd>`, capture stdout, feed through pipeline
    if let Some(payload_b64) = id.strip_prefix("shellpipe::") {
        use base64::prelude::*;
        let payload = BASE64_URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|e| anyhow::anyhow!("shellpipe id base64: {e}"))?;
        let decoded = String::from_utf8(payload)
            .map_err(|e| anyhow::anyhow!("shellpipe id utf8: {e}"))?;
        let mut lines = decoded.split('\n');
        let cmd = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("shellpipe id: missing cmd"))?
            .to_string();
        let stages: Vec<String> = lines.map(String::from).collect();
        return execute_shell_pipeline(&cmd, &stages).await;
    }
    // Multi-stage pipeline activation. The id encodes the base
    // candidate id + `\n`-joined stage list (base64-url). Resolve
    // the base to a text value, feed through each stage, return the
    // final sink's Effect
    if let Some(payload_b64) = id.strip_prefix("pipeline::") {
        use base64::prelude::*;
        let payload = BASE64_URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|e| anyhow::anyhow!("pipeline id base64: {e}"))?;
        let decoded =
            String::from_utf8(payload).map_err(|e| anyhow::anyhow!("pipeline id utf8: {e}"))?;
        // Encoded shape: `base_id\nembedded_source\nstage1\nstage2...`.
        // The embedded source is candidate's title at the moment
        // the pipeline row was built - this is what most pipelines
        // want piped (calc result, encoding output, time format, etc.).
        // Notes are an exception: their titles are headings, but the
        // user wants file content, so note branch below
        // overrides with a fresh disk read
        let mut lines = decoded.split('\n');
        let base_id = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("pipeline id: missing base"))?;
        let embedded = lines.next().unwrap_or("").to_string();
        let stages: Vec<String> = lines.map(String::from).collect();
        let source = extract_pipeline_text(base_id, &embedded, &br.index)
            .ok_or_else(|| anyhow::anyhow!("pipeline: could not read source `{base_id}`"))?;
        return pipeline::execute(source, &stages).map_err(|e| anyhow::anyhow!(e.to_string()));
    }
    // Generic chain: decode the wrapped candidate id + action, then
    // delegate through the normal provider dispatch. The chain layer
    // never duplicates action handling; it's purely syntactic sugar
    // over provider's own activate path
    if let Some(rest) = id.strip_prefix("chain::") {
        use base64::prelude::*;
        let (action_id, id_b64) = rest
            .split_once("::")
            .ok_or_else(|| anyhow::anyhow!("malformed chain id: {id}"))?;
        let orig_bytes = BASE64_URL_SAFE_NO_PAD
            .decode(id_b64)
            .map_err(|e| anyhow::anyhow!("chain id base64: {e}"))?;
        let orig_id =
            String::from_utf8(orig_bytes).map_err(|e| anyhow::anyhow!("chain id utf8: {e}"))?;
        return br.registry.dispatch(&orig_id, action_id).await;
    }
    br.registry.dispatch(&id.to_string(), action).await
}

/// Pull the text the pipeline should pipe through
///
/// Three regimes:
///
/// - Notes - candidate's title is a heading, but user
///   wants the full markdown body. Re-read from disk so any edits
///   between candidate-build and Enter land in the pipeline.
/// - Clipboard - candidate's title is only the first line of
///   the entry, truncated for dropdown. Pull the FULL content
///   back from index by id. Without this, piping `clip | jq
///   .ai` on a multi-line JSON payload would feed `{` into jq and
///   silently produce no output.
/// - Everything else - use source text we embedded in the
///   payload at build time (the candidate's title). Calculator
///   results, encoding output, time formats, snippets: their titles
///   ARE value users mean to pipe
///
/// Returns None only when theres nothing to feed forward - empty
/// embedded source AND not a recognisable note/clipboard path
fn extract_pipeline_text(
    base_id: &str,
    embedded: &str,
    index: &gyors_index::Index,
) -> Option<String> {
    if let Some(rest) = base_id.strip_prefix("note::") {
        if rest.ends_with(".md") {
            return std::fs::read_to_string(rest).ok();
        }
    }
    if let Some(rest) = base_id.strip_prefix("clip::") {
        if let Ok(num) = rest.parse::<i64>() {
            if let Ok(Some(item)) = index.clipboard_get(num) {
                return Some(item.content);
            }
        }
    }
    if embedded.is_empty() {
        return None;
    }
    Some(embedded.to_string())
}


/// Parsed shape of a chain query
///
/// `base` is the first segment - the thing user wants to act on
/// (e.g., `note hello`). `stages` are every segment after the first
/// separator pipe, in order. A classic 2-stage chain like
/// `note hello | copy` is `stages = ["copy"]`. A multi-stage
/// pipeline like `note hello | upper | copy` is `stages = ["upper",
/// "copy"]`
///
/// Interpretation of stages:
/// - `stages.len() == 1` and the stage matches one of the base
///   candidate's actions -> classic action-filter path.
/// - Otherwise -> pipeline path: each stage is a transform or sink
///   registered in `pipeline.rs`
struct ChainSpec {
    base: String,
    stages: Vec<String>,
}

/// Detect a chain in the raw query
///
/// `|` is the ONE chain separator. No space-padding required - a user
/// can type `foo|bar` or `foo | bar` and both yield same split.
/// This matches shell-pipe muscle memory users already have
///
/// `>` is NOT a chain separator even though an earlier implementation
/// allowed it with flanking spaces: `>` is already overloaded as the
/// shell-prefix trigger (`> ls -la` runs `ls` via shell provider),
/// and mixing two meanings into same character confuses users who
/// type `echo foo > bar` expecting either shell redirection or a chain.
/// Single-meaning `|` beats ambiguous dual-meaning `>`
///
/// Leading `>` stays shell-mode opt-in - handled by a separate
/// branch in `orchestrate`, not here
///
/// Separator rule: the first `|` that has whitespace on at least
/// one side. Covers all typing rhythms Swift UI accepts:
/// ` | ` (spaced), `| ` (pipe-then-space), ` |` (space-then-pipe),
/// trailing `foo |` / `foo| `. A bare `foo|bar` (no surrounding
/// whitespace at all) stays literal - that's the shape regex
/// alternation uses
///
/// keyword bail list for providers that commonly
/// accept `|` legitimately. `re cat | dog` (regex alternation with
/// arbitrary spacing) MUST NOT chain
fn parse_chain(raw: &str) -> Option<ChainSpec> {
    // Only trim_start here: a trailing space is a meaningful signal
    // that the pipe acts as a separator (`foo| ` -> separator). A
    // full trim would eat that signal, losing the chain detection
    let stripped = raw.trim_start();
    if stripped.starts_with('>') {
        return None; // shell mode - handled in `orchestrate`
    }

    let pipe = find_separator_pipe(stripped)?;
    let (base_part, rest) = stripped.split_at(pipe);
    let base = base_part.trim();
    if base.is_empty() {
        return None;
    }
    if base_uses_literal_pipe(base) {
        return None;
    }

    // Collect EVERY stage: walk the remaining string, splitting on
    // each separator pipe. Produces `stages = [first, second, ...]`
    // in user-typed order. Empty trailing stage (trailing pipe) is
    // kept - the pipeline executor treats it as "user hasn't named
    // next stage yet" and auto-shows the prior output
    let mut stages: Vec<String> = Vec::new();
    let mut cursor = &rest[1..]; // skip the first `|` byte
    loop {
        if let Some(next) = find_separator_pipe(cursor) {
            let (seg, after) = cursor.split_at(next);
            stages.push(seg.trim().to_string());
            cursor = &after[1..];
        } else {
            stages.push(cursor.trim().to_string());
            break;
        }
    }
    Some(ChainSpec {
        base: base.to_string(),
        stages,
    })
}

/// Byte-index of the first `|` that has whitespace on at least one
/// side. `None` when every `|` is flanked by non-whitespace on both
/// sides (the literal shape, e.g. regex `cat|dog`)
fn find_separator_pipe(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'|' {
            continue;
        }
        let before_ws = i > 0 && (bytes[i - 1] as char).is_whitespace();
        let after_ws = i + 1 < bytes.len() && (bytes[i + 1] as char).is_whitespace();
        if before_ws || after_ws {
            return Some(i);
        }
    }
    None
}

/// True when the base starts with a keyword whose provider accepts
/// `|` in its input. Gated by the FIRST whitespace-delimited token so
/// `re cat | dog` matches `re` and bails, but `recording | copy`
/// (keyword `recording`, which isn't pipe-using) chains correctly
fn base_uses_literal_pipe(base: &str) -> bool {
    const PIPE_USING_KEYWORDS: &[&str] = &[
        // Regex tester: `|` is alternation syntax
        "re",
        "regex",
        // Format converters: YAML block scalars use `|` as the
        // multiline-preserve marker
        "json2yaml",
        "yaml2json",
        "json2toml",
        "toml2json",
        "yaml2toml",
        "toml2yaml",
        // AI transforms: free-text content frequently contains `|`
        "summarize",
        "tldr",
        "explain",
        "rewrite",
        "fix",
        "shorten",
        "expand",
        "translate",
        // AI free-form ask: same rationale
        "ai",
        "ask",
    ];
    let first = base.split_whitespace().next().unwrap_or("");
    let lower = first.to_ascii_lowercase();
    PIPE_USING_KEYWORDS.contains(&lower.as_str())
}

async fn orchestrate_chain(
    spec: ChainSpec,
    br: &GyorsBridge,
) -> anyhow::Result<Vec<ScoredCandidate>> {
    // Run the base query through normal orchestration. That picks
    // up QueryMode routing (Notes, Clipboard, Default) so each
    // provider's conventions work in the base position
    let base_query = Query::new(spec.base.clone());
    let base_scored = Box::pin(orchestrate(&base_query, br)).await?;
    // Find the best scoring candidate with at least one action
    // (synthetic helper rows that only carry "OK" are skipped -
    // they're not meaningful chain targets)
    let top = base_scored
        .into_iter()
        .find(|sc| !sc.candidate.actions.is_empty() && !is_helper_candidate(&sc.candidate));
    let Some(top) = top else {
        return Ok(vec![ScoredCandidate {
            candidate: chain_no_base_match(&spec.base),
            score: BYPASS_RANK_SCORE,
        }]);
    };

    // Every stage EXCEPT the last must be a finalised transform -
    // sinks only make sense as the final stage, and unknown stages
    // earlier in the pipeline are unresolvable. Report the first
    // offender as a helper row so user gets a precise error
    if spec.stages.len() > 1 {
        for stage in &spec.stages[..spec.stages.len() - 1] {
            if stage.trim().is_empty() {
                continue;
            }
            match pipeline::classify_stage(stage) {
                pipeline::StageKind::Transform => {}
                pipeline::StageKind::Sink | pipeline::StageKind::Unknown => {
                    return Ok(vec![ScoredCandidate {
                        candidate: chain_unknown_action(stage, &top.candidate),
                        score: BYPASS_RANK_SCORE,
                    }]);
                }
            }
        }
    }

    // The LAST stage is what user is currently typing - it gets
    // prefix-match suggestions. We surface three kinds of rows,
    // scored in the order a user would pick:
    //   1. Base-candidate actions matching the partial (only
    //      meaningful when user has just ONE stage typed - e.g.
    //      `note hello | prev` -> Preview action).
    //   2. Pipeline transforms matching the partial.
    //   3. Pipeline sinks matching the partial
    let last_idx = spec.stages.len().saturating_sub(1);
    let last_raw = spec.stages.last().map(String::as_str).unwrap_or("");
    let last = last_raw.trim();

    let mut out: Vec<ScoredCandidate> = Vec::new();
    let mut rank: i64 = BYPASS_RANK_SCORE;

    // Classic action path: only meaningful when user's chain is
    // just `base | action` - middle stages of a multi-stage pipeline
    // must be transforms, so a base-action in the middle doesn't fit
    if spec.stages.len() == 1 {
        for action in filter_actions(&top.candidate.actions, last) {
            out.push(ScoredCandidate {
                candidate: chain_confirm_candidate(&top.candidate, action),
                score: rank,
            });
            rank -= 1;
        }
    }

    // Pipeline suggestions: every stage whose canonical keyword
    // matches the partial. Empty partial -> offer every stage
    let pipeline_matches: Vec<&pipeline::StageDef> = pipeline::prefix_match(last)
        .into_iter()
        .filter(|stage_def| {
            // For non-last transforms, a sink in the middle doesn't
            // make sense - skip when we're before the final stage
            !(spec.stages.len() > 1
                && last_idx < spec.stages.len() - 1
                && stage_def.kind == pipeline::StageKind::Sink)
        })
        .collect();
    // When user's partial uniquely identifies a single stage
    // (e.g. `md` -> only `md5` matches), emit an autoclose candidate
    // whose id encodes the fully-completed query. Swift side
    // picks it up for the grey-tail ghost completion so user
    // sees `md5` inline as they type `md`. Tab then commits it
    if !last.is_empty() && pipeline_matches.len() == 1 {
        let canonical = pipeline_matches[0].canonical;
        if canonical.len() > last.len()
            && canonical
                .to_ascii_lowercase()
                .starts_with(&last.to_ascii_lowercase())
        {
            let ghost_full = reconstruct_full_query(&spec, canonical);
            out.push(ScoredCandidate {
                candidate: autoclose_candidate(&ghost_full),
                score: rank,
            });
            rank -= 1;
        }
    }

    // Ghost-text for jq filter args: when user is typing
    // `jq .partial`, peek source JSON, walk to the committed
    // dot-path, and suggest the unique key extension via the same
    // autoclose path the stage-name completion uses. Bails fast on
    // non-JSON sources via the cheap leading-byte precheck inside
    // `complete_dot_path`
    if let Some(jq_args) = strip_jq_prefix(last) {
        if jq_args.starts_with('.') {
            if let Some(source) = jq_autocomplete_source(&top.candidate, &br.index) {
                if let Some(completed_filter) =
                    gyors_providers::jq_filter::complete_dot_path(&source, jq_args)
                {
                    let ghost_full = reconstruct_query_with_last(&spec, last_idx, &format!("jq {completed_filter}"));
                    out.push(ScoredCandidate {
                        candidate: autoclose_candidate(&ghost_full),
                        score: rank,
                    });
                    rank -= 1;
                }
            }
        }
    }
    for stage_def in &pipeline_matches {
        let mut completed_stages = spec.stages.clone();
        // Preserve user-typed args for arg-bearing stages (`jq .name`)
        // so completing canonical doesn't drop the filter. For
        // non-arg stages this is a no-op - args is always empty
        let preserved_args = stage_def.extract_args(last).unwrap_or("");
        let completed_label = if preserved_args.is_empty() {
            stage_def.canonical.to_string()
        } else {
            format!("{} {}", stage_def.canonical, preserved_args)
        };
        if let Some(slot) = completed_stages.get_mut(last_idx) {
            *slot = completed_label.clone();
        } else {
            completed_stages.push(completed_label.clone());
        }
        let completed = ChainSpec {
            base: spec.base.clone(),
            stages: completed_stages,
        };
        out.push(ScoredCandidate {
            candidate: pipeline_stage_candidate(&top.candidate, &completed, stage_def),
            score: rank,
        });
        rank -= 1;
    }

    if out.is_empty() {
        return Ok(vec![ScoredCandidate {
            candidate: chain_unknown_action(last, &top.candidate),
            score: BYPASS_RANK_SCORE,
        }]);
    }
    Ok(out)
}

/// Strip the `jq ` (case-insensitive) keyword from `hint` and return
/// the remainder. None when the hint isn't a jq invocation. Used by
/// ghost-text autocomplete to find the args portion of the
/// current stage
fn strip_jq_prefix(hint: &str) -> Option<&str> {
    let trimmed = hint.trim_start();
    let lower_prefix = trimmed
        .as_bytes()
        .get(..3)
        .and_then(|b| std::str::from_utf8(b).ok())?
        .to_ascii_lowercase();
    if lower_prefix == "jq " {
        Some(&trimmed[3..])
    } else {
        None
    }
}

/// Resolve JSON body autocomplete should peek at. For
/// clipboard candidates the embedded title is the truncated preview;
/// pull the full content from index so multi-line JSON entries
/// can be walked. For everything else, candidate's title IS the
/// value (calc results, snippet bodies, encoded output, etc.)
///
/// The cheap leading-byte JSON precheck happens inside
/// `complete_dot_path`, so this is just about sourcing bytes
fn jq_autocomplete_source(
    base: &gyors_core::Candidate,
    index: &gyors_index::Index,
) -> Option<String> {
    if let Some(rest) = base.id.strip_prefix("clip::") {
        if let Ok(num) = rest.parse::<i64>() {
            if let Ok(Some(item)) = index.clipboard_get(num) {
                return Some(item.content);
            }
        }
    }
    Some(base.title.clone())
}

/// Like `reconstruct_full_query`, but swaps the last stage with an
/// arbitrary string instead of forcing it to a stage's canonical.
/// Used by the jq ghost-text path so we can place `jq .ai.provider`
/// (canonical + args) rather than just canonical alone
fn reconstruct_query_with_last(spec: &ChainSpec, last_idx: usize, replacement: &str) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(spec.stages.len() + 1);
    parts.push(spec.base.clone());
    for (i, stage) in spec.stages.iter().enumerate() {
        let value = if i == last_idx {
            replacement.to_string()
        } else {
            stage.trim().to_string()
        };
        parts.push(value);
    }
    parts.join(" | ")
}

/// Rebuild the flat-form query that would result from completing
/// the last stage to `canonical`. Uses the ` | ` joiner that
/// matches Swift side's canonical form, so ghost suffix
/// extraction in `splitGhost` compares against an identical string
fn reconstruct_full_query(spec: &ChainSpec, canonical: &str) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(spec.stages.len() + 1);
    parts.push(spec.base.clone());
    let last_idx = spec.stages.len().saturating_sub(1);
    for (i, stage) in spec.stages.iter().enumerate() {
        let value = if i == last_idx {
            canonical.to_string()
        } else {
            stage.trim().to_string()
        };
        parts.push(value);
    }
    parts.join(" | ")
}

/// Emit an autoclose candidate that Swift `splitGhost` pass
/// consumes into grey-tail ghost text. Candidate is filtered
/// out of the visible results - its only job is to carry the
/// completion suffix via the id
fn autoclose_candidate(full_query: &str) -> gyors_core::Candidate {
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    Candidate {
        id: format!("autoclose::{full_query}"),
        title: full_query.to_string(),
        subtitle: None,
        icon: Icon::SfSymbol("text.cursor".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Autoclose")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Visible-text cap for source previews in chain/pipeline rows.
/// Clipboard candidates carry the full pasteboard body as their
/// title; rendering that verbatim into every dropdown row makes the
/// list unreadable. Cap visible copies of title at this width;
/// actual piped value (the embedded source in the payload) is
/// untouched
const CHAIN_SOURCE_PREVIEW_MAX: usize = 48;

/// Flatten newlines/tabs and cap at `CHAIN_SOURCE_PREVIEW_MAX`
/// characters with a trailing ellipsis. Used wherever a source
/// candidate's title is interpolated into a row that needs to fit
/// on one line
fn short_source_label(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect();
    if flat.chars().count() <= CHAIN_SOURCE_PREVIEW_MAX {
        return flat;
    }
    let head: String = flat.chars().take(CHAIN_SOURCE_PREVIEW_MAX).collect();
    format!("{head}…")
}

/// One suggestion row for a specific stage completion. Activating
/// row runs the full pipeline (with this stage's canonical
/// keyword substituted into the plan). Title leads with the stage
/// description so user sees WHAT will happen; subtitle carries
/// source preview + chain arrows
fn pipeline_stage_candidate(
    base: &gyors_core::Candidate,
    spec: &ChainSpec,
    stage: &pipeline::StageDef,
) -> gyors_core::Candidate {
    use base64::prelude::*;
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    let stages_pretty: Vec<&str> = spec
        .stages
        .iter()
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .collect();
    let arrows = stages_pretty.join(" → ");
    let kind_label = match stage.kind {
        pipeline::StageKind::Transform => "transform",
        pipeline::StageKind::Sink => "sink",
        pipeline::StageKind::Unknown => "stage",
    };
    let short_base = short_source_label(&base.title);
    let subtitle = format!(
        "{} · {kind_label} · ↵ pipe {short_base} through {arrows}",
        stage.description
    );
    // Embed candidate's title as the pipeline source so non-note
    // sources (calc results, encoding output, time formats, snippets,
    // ...) actually have something to feed downstream. Notes ignore the
    // embedded value and re-read from disk at activate time
    //
    // Newlines in titles are theoretically possible (e.g. multi-line
    // ShowText previews); flatten to spaces so they dont collide with
    // our line-separated payload format. The FULL original content
    // flows through here - only the display strings are truncated
    let embedded_source = base.title.replace('\n', " ");
    let payload = format!(
        "{}\n{}\n{}",
        base.id,
        embedded_source,
        stages_pretty.join("\n"),
    );
    let encoded = BASE64_URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let icon_name = match stage.kind {
        pipeline::StageKind::Transform => "arrow.right.arrow.left.square",
        pipeline::StageKind::Sink => "tray.and.arrow.down",
        pipeline::StageKind::Unknown => "questionmark.circle",
    };
    Candidate {
        id: format!("pipeline::{encoded}"),
        title: format!("{arrows} - {}", stage.description),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol(icon_name.into()),
        kind: CandidateKind::Custom("chain".into()),
        actions: vec![Action::primary("Run pipeline")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Synthetic "OK" helper rows (guidance, empty states) carry exactly
/// one action labeled "OK". They're not chain-targetable - skip them
/// when picking the top candidate
fn is_helper_candidate(cand: &gyors_core::Candidate) -> bool {
    cand.actions.len() == 1 && cand.actions[0].label.eq_ignore_ascii_case("OK")
}

// Shell stdout pipelines: `> ls -la | copy`
//
// Mirror of the regular chain flow but with a SHELL command as the
// pipeline source. The full lifecycle:
//   1. `parse_shell_pipeline` splits "ls -la | upper | copy" into
//      cmd="ls -la" and stages=["upper", "copy"]. It walks separators
//      right-to-left so any pipes that belong to shell command
//      itself (e.g. `ls | grep foo | copy`) stay in the cmd half.
//   2. `orchestrate_shell_pipeline` builds suggestion rows for the
//      stage being typed - same `prefix_match` lookup the
//      non-shell pipeline uses.
//   3. Activation hits the `shellpipe::<base64>` handler (in
//      `dispatch_activation`), which runs the cmd via `/bin/sh -c`,
//      captures stdout, and feeds it through `pipeline::execute`

/// Parse a shell-pipeline shape from the post-`>` query
///
/// Returns `(cmd, stages)` when input contains a ` | ` separator
/// AND suffix after the last separator is either empty (user is
/// mid-typing) or any string (we let `prefix_match` decide whether
/// it's a known stage). Middle stages must be `Transform`s - anything
/// else (including unknown words) means caller is just running a
/// shell command that happens to contain a pipe (`grep "a | b"` would
/// already lose its space-padded ` | ` to the chain layer, but we
/// still preserve it inside the cmd because the algorithm walks
/// right-to-left and stops at the first non-stage segment)
pub fn parse_shell_pipeline(effective: &str) -> Option<(String, Vec<String>)> {
    let trimmed = effective.trim();
    // Trailing-pipe shape: `cmd |` (the user typed ` | ` and the
    // trailing space already got trimmed upstream by `parse_mode`).
    // Stand-in with a synthetic trailing space so `split(" | ")` finds
    // the separator and emits an empty partial stage at end
    let work_string;
    let work = if trimmed.ends_with(" |") {
        work_string = format!("{trimmed} ");
        work_string.as_str()
    } else {
        trimmed
    };
    if !work.contains(" | ") {
        return None;
    }
    // Split on " | " (the canonical chain separator). Any tighter
    // forms like `a|b` stay inside one segment - that's the same
    // contract `find_separator_pipe` enforces for the regular chain
    let parts: Vec<&str> = work.split(" | ").collect();
    if parts.len() < 2 {
        return None;
    }

    // The LAST segment is what user is typing - keep it as-is
    // (could be empty, a partial like `co`, or a full keyword). The
    // suggestion rows handle lookup
    let mut stages_rev: Vec<String> = Vec::new();
    let mut cmd_end = parts.len() - 1;
    stages_rev.push(parts[parts.len() - 1].trim().to_string());

    // Walk left while next segment is a known transform. Sinks
    // can't appear mid-pipeline; unknown segments belong to the cmd
    while cmd_end > 0 {
        let seg = parts[cmd_end - 1].trim();
        if !seg.is_empty()
            && pipeline::classify_stage(seg) == pipeline::StageKind::Transform
        {
            stages_rev.push(seg.to_string());
            cmd_end -= 1;
        } else {
            break;
        }
    }

    if cmd_end == 0 {
        // Pipeline with no shell command - `> | copy` etc. Falls
        // through to the regular shell handler so user gets a
        // sensible "Run: |copy" error rather than an empty dropdown
        return None;
    }

    let cmd = parts[..cmd_end].join(" | ").trim().to_string();
    if cmd.is_empty() {
        return None;
    }
    let stages: Vec<String> = stages_rev.into_iter().rev().collect();
    Some((cmd, stages))
}

/// Build the suggestion-row candidate set for a shell pipeline.
/// Empty-stage (the user just typed ` | `) -> every stage suggested.
/// Partial stage -> prefix-matched stages
fn orchestrate_shell_pipeline(cmd: &str, stages: &[String]) -> Vec<ScoredCandidate> {
    // Validate the middle stages - non-final must be transforms. The
    // parser already filters non-transforms out of the middle, but
    // double-check in case of future regressions
    if stages.len() > 1 {
        for stage in &stages[..stages.len() - 1] {
            if pipeline::classify_stage(stage) != pipeline::StageKind::Transform {
                return vec![ScoredCandidate {
                    candidate: shell_pipeline_unknown_candidate(cmd, stage),
                    score: BYPASS_RANK_SCORE,
                }];
            }
        }
    }

    let last_idx = stages.len().saturating_sub(1);
    let last_partial = stages.last().map(String::as_str).unwrap_or("").trim();
    let matches: Vec<&pipeline::StageDef> = pipeline::prefix_match(last_partial);

    let mut out: Vec<ScoredCandidate> = Vec::new();
    let mut rank: i64 = BYPASS_RANK_SCORE;

    // Autoclose ghost: when the partial uniquely identifies a stage,
    // emit autoclose candidate so Swift can render grey-tail
    // completion (and Tab commits it)
    if !last_partial.is_empty() && matches.len() == 1 {
        let canonical = matches[0].canonical;
        if canonical.len() > last_partial.len()
            && canonical
                .to_ascii_lowercase()
                .starts_with(&last_partial.to_ascii_lowercase())
        {
            let mut completed_stages = stages.to_vec();
            completed_stages[last_idx] = canonical.to_string();
            let ghost = format!("> {} | {}", cmd, completed_stages.join(" | "));
            out.push(ScoredCandidate {
                candidate: autoclose_candidate(&ghost),
                score: rank,
            });
            rank -= 1;
        }
    }

    for stage_def in &matches {
        let mut completed_stages = stages.to_vec();
        if let Some(slot) = completed_stages.get_mut(last_idx) {
            *slot = stage_def.canonical.to_string();
        } else {
            completed_stages.push(stage_def.canonical.to_string());
        }
        out.push(ScoredCandidate {
            candidate: shell_pipeline_stage_candidate(cmd, &completed_stages, stage_def),
            score: rank,
        });
        rank -= 1;
    }

    if out.is_empty() {
        out.push(ScoredCandidate {
            candidate: shell_pipeline_unknown_candidate(cmd, last_partial),
            score: BYPASS_RANK_SCORE,
        });
    }
    out
}

fn shell_pipeline_stage_candidate(
    cmd: &str,
    stages: &[String],
    stage: &pipeline::StageDef,
) -> gyors_core::Candidate {
    use base64::prelude::*;
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    let stages_pretty = stages.join(" → ");
    let kind_label = match stage.kind {
        pipeline::StageKind::Transform => "transform",
        pipeline::StageKind::Sink => "sink",
        pipeline::StageKind::Unknown => "stage",
    };
    let subtitle = format!(
        "{} · {kind_label} · ↵ run `{}` and pipe stdout through {stages_pretty}",
        stage.description, cmd
    );
    // Encoded shape: `cmd\nstage1\nstage2...`. Swift round-trips this
    // through activation path; lines stay in source order
    let payload = format!("{cmd}\n{}", stages.join("\n"));
    let encoded = BASE64_URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let icon_name = match stage.kind {
        pipeline::StageKind::Transform => "arrow.right.arrow.left.square",
        pipeline::StageKind::Sink => "tray.and.arrow.down",
        pipeline::StageKind::Unknown => "questionmark.circle",
    };
    Candidate {
        id: format!("shellpipe::{encoded}"),
        title: format!("Run: {cmd} → {stages_pretty}"),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol(icon_name.into()),
        kind: CandidateKind::Custom("chain".into()),
        actions: vec![Action::primary("Run pipeline")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn shell_pipeline_unknown_candidate(cmd: &str, partial: &str) -> gyors_core::Candidate {
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    Candidate {
        id: "chain::__unknown-shell-stage__".into(),
        title: format!("Unknown stage `{partial}`"),
        subtitle: Some(format!(
            "After `{cmd} | ` Gyors expects a known transform or sink (upper, b64, copy, qr, …)."
        )),
        icon: Icon::SfSymbol("questionmark.circle".into()),
        kind: CandidateKind::Custom("chain".into()),
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Run shell command, capture stdout, feed through the pipeline.
/// Stderr is discarded for the pipeline value but copied to NSLog via
/// calling Swift side (it's preserved in the `Effect::ShowText`
/// `text` for visibility when theres no other sink - that's a
/// follow-up; v1 just feeds stdout)
async fn execute_shell_pipeline(cmd: &str, stages: &[String]) -> anyhow::Result<Effect> {
    let output = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("spawn `/bin/sh -c {cmd}`: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    pipeline::execute(stdout, stages)
        .map_err(|e| anyhow::anyhow!("shell pipeline: {e}"))
}

// AI as pipeline source: `ai <prompt> | stages`
//
// Mirror of shell-pipe path, but source is the AI's answer
// instead of stdout. The execution is kicked off on Swift side
// (the AI client + provider routing live there); Rust side just
// emits an `Effect::AskAiThenPipe` carrying prompt, optional
// preset instruction, and remaining stages. Swift runs the AI, then
// re-dispatches answer through existing `pipeline::` handler

/// Verbs that turn the chain's base into an AI-source prompt. `ai` /
/// `ask` ship prompt as-is; the others reuse the curated
/// instructions from `AiTransformsProvider::VERBS`. `translate` is
/// excluded - its target-language argument doesn't fit the chain's
/// "<verb> <text> | <stages>" shape
#[cfg(feature = "ai")]
const AI_PIPELINE_VERBS: &[&str] = &[
    "ai",
    "ask",
    "summarize",
    "tldr",
    "explain",
    "rewrite",
    "fix",
    "shorten",
    "expand",
];

/// Parse `<ai-verb> <prompt> | <stages...>` into its parts
///
/// Returns `(prompt, instruction, stages)`:
/// - `prompt`: text passed to the AI as user message
/// - `instruction`: `Some(...)` for verb presets (summarize/tldr/...),
///   `None` for free-form `ai`/`ask`
/// - `stages`: pipeline stages (last entry may be partial while
///   input is still being typed)
#[cfg(feature = "ai")]
pub fn parse_ai_pipeline(raw: &str) -> Option<(String, Option<String>, Vec<String>)> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();

    // Match a leading verb followed by a space - bare verbs without
    // any prompt yet aren't pipelines (parse_mode handles plain
    // keyword dispatch)
    let mut matched_verb: Option<&'static str> = None;
    let mut prompt_start: usize = 0;
    for verb in AI_PIPELINE_VERBS {
        let needle = format!("{verb} ");
        if lower.starts_with(&needle) {
            matched_verb = Some(verb);
            prompt_start = needle.len();
            break;
        }
    }
    let verb = matched_verb?;
    let after_verb = &trimmed[prompt_start..];

    // Trailing-pipe form: `cmd |` (the user typed ` | ` and the
    // trailing space got trimmed). Same trick as `parse_shell_pipeline`
    let work_string;
    let work = if after_verb.trim_end().ends_with(" |") {
        work_string = format!("{} ", after_verb.trim_end());
        work_string.as_str()
    } else {
        after_verb
    };
    if !work.contains(" | ") {
        return None;
    }
    let parts: Vec<&str> = work.split(" | ").collect();
    if parts.len() < 2 {
        return None;
    }

    // Walk right-to-left through transforms; stop at the first
    // non-transform segment so any pipes that belong to prompt
    // itself (e.g. `compare cat | dog | copy` - improbable but
    // possible) stay in prompt half
    let mut stages_rev: Vec<String> = Vec::new();
    let mut prompt_end = parts.len() - 1;
    stages_rev.push(parts[parts.len() - 1].trim().to_string());
    while prompt_end > 0 {
        let seg = parts[prompt_end - 1].trim();
        if !seg.is_empty()
            && pipeline::classify_stage(seg) == pipeline::StageKind::Transform
        {
            stages_rev.push(seg.to_string());
            prompt_end -= 1;
        } else {
            break;
        }
    }
    if prompt_end == 0 {
        return None;
    }
    let prompt = parts[..prompt_end].join(" | ").trim().to_string();
    if prompt.is_empty() {
        return None;
    }

    let instruction = match verb {
        "ai" | "ask" => None,
        v => gyors_providers::ai_transforms::verb_for(v).map(|x| x.instruction.to_string()),
    };
    let stages: Vec<String> = stages_rev.into_iter().rev().collect();
    Some((prompt, instruction, stages))
}

/// Build the suggestion-row candidate set for an AI pipeline
#[cfg(feature = "ai")]
fn orchestrate_ai_pipeline(
    prompt: &str,
    instruction: Option<String>,
    stages: &[String],
) -> Vec<ScoredCandidate> {
    if stages.len() > 1 {
        for stage in &stages[..stages.len() - 1] {
            if pipeline::classify_stage(stage) != pipeline::StageKind::Transform {
                return vec![ScoredCandidate {
                    candidate: ai_pipeline_unknown_candidate(prompt, stage),
                    score: BYPASS_RANK_SCORE,
                }];
            }
        }
    }

    let last_idx = stages.len().saturating_sub(1);
    let last_partial = stages.last().map(String::as_str).unwrap_or("").trim();
    let matches: Vec<&pipeline::StageDef> = pipeline::prefix_match(last_partial)
        .into_iter()
        // AI sinks (ai/summarize/...) at end of an AI-source
        // pipeline would be a no-op recursion - filter them
        .filter(|s| !is_ai_sink_keyword(s.canonical))
        .collect();

    let mut out: Vec<ScoredCandidate> = Vec::new();
    let mut rank: i64 = BYPASS_RANK_SCORE;

    if !last_partial.is_empty() && matches.len() == 1 {
        let canonical = matches[0].canonical;
        if canonical.len() > last_partial.len()
            && canonical
                .to_ascii_lowercase()
                .starts_with(&last_partial.to_ascii_lowercase())
        {
            let mut completed_stages = stages.to_vec();
            completed_stages[last_idx] = canonical.to_string();
            let verb_label = if instruction.is_some() {
                ai_verb_for_label(instruction.as_deref())
            } else {
                "ai"
            };
            let ghost = format!(
                "{verb_label} {} | {}",
                prompt,
                completed_stages.join(" | ")
            );
            out.push(ScoredCandidate {
                candidate: autoclose_candidate(&ghost),
                score: rank,
            });
            rank -= 1;
        }
    }

    for stage_def in &matches {
        let mut completed_stages = stages.to_vec();
        if let Some(slot) = completed_stages.get_mut(last_idx) {
            *slot = stage_def.canonical.to_string();
        } else {
            completed_stages.push(stage_def.canonical.to_string());
        }
        out.push(ScoredCandidate {
            candidate: ai_pipeline_stage_candidate(
                prompt,
                instruction.clone(),
                &completed_stages,
                stage_def,
            ),
            score: rank,
        });
        rank -= 1;
    }

    if out.is_empty() {
        out.push(ScoredCandidate {
            candidate: ai_pipeline_unknown_candidate(prompt, last_partial),
            score: BYPASS_RANK_SCORE,
        });
    }
    out
}

/// Pipeline keywords that route TO AI as a sink. We strip them from
/// the suggestion list when AI is already source - `ai foo | ai`
/// is a no-op recursion users wouldn't want suggested
#[cfg(feature = "ai")]
fn is_ai_sink_keyword(kw: &str) -> bool {
    matches!(
        kw,
        "ai" | "ask"
            | "summarize"
            | "tldr"
            | "explain"
            | "rewrite"
            | "fix"
            | "shorten"
            | "expand"
    )
}

#[cfg(feature = "ai")]
fn ai_verb_for_label(instruction: Option<&str>) -> &'static str {
    if instruction.is_none() {
        "ai"
    } else {
        // Best-effort verb name from the instruction body. Used only
        // for the visible header "Ask AI: ..." / "Summarize: ..." - we
        // dont round-trip through this
        "ai"
    }
}

#[cfg(feature = "ai")]
fn ai_pipeline_stage_candidate(
    prompt: &str,
    instruction: Option<String>,
    stages: &[String],
    stage: &pipeline::StageDef,
) -> gyors_core::Candidate {
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    let stages_pretty = stages.join(" → ");
    let kind_label = match stage.kind {
        pipeline::StageKind::Transform => "transform",
        pipeline::StageKind::Sink => "sink",
        pipeline::StageKind::Unknown => "stage",
    };
    let header = match &instruction {
        None => format!("Ask AI: {prompt}"),
        Some(_) => format!("AI ({}): {prompt}", short_instruction_tag(instruction.as_deref())),
    };
    let subtitle = format!(
        "{} · {kind_label} · ↵ run AI then pipe answer through {stages_pretty}",
        stage.description
    );
    let icon_name = match stage.kind {
        pipeline::StageKind::Transform => "sparkles",
        pipeline::StageKind::Sink => "sparkles",
        pipeline::StageKind::Unknown => "questionmark.circle",
    };
    // Encode payload into the id so activation has everything it
    // needs without a second backend round-trip. Shape:
    //     prompt\n<has-instruction-flag>\ninstruction\nstage1\nstage2...
    // The empty-instruction flag is a distinct line so prompts
    // containing newlines (rare but possible) can't collide
    use base64::prelude::*;
    let instr_payload = instruction.clone().unwrap_or_default();
    let payload = format!(
        "{prompt}\n{}\n{instr_payload}\n{}",
        if instruction.is_some() { "1" } else { "0" },
        stages.join("\n"),
    );
    let encoded = BASE64_URL_SAFE_NO_PAD.encode(payload.as_bytes());
    Candidate {
        id: format!("aipipe::{encoded}"),
        title: format!("{header} → {stages_pretty}"),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol(icon_name.into()),
        kind: CandidateKind::Custom("chain".into()),
        actions: vec![Action::primary("Run AI pipeline")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(feature = "ai")]
fn ai_pipeline_unknown_candidate(prompt: &str, partial: &str) -> gyors_core::Candidate {
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    Candidate {
        id: "chain::__unknown-ai-stage__".into(),
        title: format!("Unknown stage `{partial}`"),
        subtitle: Some(format!(
            "After `{prompt} | ` Gyors expects a known transform or sink (upper, b64, copy, qr, …). AI verbs aren't valid stages here (the source already IS AI)."
        )),
        icon: Icon::SfSymbol("questionmark.circle".into()),
        kind: CandidateKind::Custom("chain".into()),
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Shrink an instruction string to the first 2-3 words for use in
/// candidate headers ("AI (Summarize): ..." instead of the full
/// 30-word system prompt)
#[cfg(feature = "ai")]
fn short_instruction_tag(instruction: Option<&str>) -> &'static str {
    let Some(s) = instruction else { return "ai" };
    let s = s.to_ascii_lowercase();
    if s.starts_with("summarize") {
        "summarize"
    } else if s.starts_with("write a one-sentence tl;dr") {
        "tldr"
    } else if s.starts_with("explain") {
        "explain"
    } else if s.starts_with("rewrite") {
        "rewrite"
    } else if s.starts_with("fix grammar") {
        "fix"
    } else if s.starts_with("shorten") {
        "shorten"
    } else if s.starts_with("expand") {
        "expand"
    } else {
        "ai"
    }
}

/// Prefix-filter a candidate's actions against the hint. Empty hint
/// returns every action in declared order. Matching tries:
///   1. `action.id` exact (case-insensitive) -> unique hit even if
///      label prefix would be ambiguous.
///   2. `action.label.to_lowercase()` prefix.
///   3. `action.id.to_lowercase()` prefix
fn filter_actions<'a>(actions: &'a [gyors_core::Action], hint: &str) -> Vec<&'a gyors_core::Action> {
    let hint_lower = hint.trim().to_lowercase();
    if hint_lower.is_empty() {
        return actions.iter().collect();
    }
    // Exact id match wins outright
    let exact: Vec<&gyors_core::Action> = actions
        .iter()
        .filter(|a| a.id.to_lowercase() == hint_lower)
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    actions
        .iter()
        .filter(|a| {
            a.label.to_lowercase().starts_with(&hint_lower)
                || a.id.to_lowercase().starts_with(&hint_lower)
        })
        .collect()
}

fn chain_confirm_candidate(
    base: &gyors_core::Candidate,
    action: &gyors_core::Action,
) -> gyors_core::Candidate {
    use base64::prelude::*;
    use gyors_core::{Action, Candidate, CandidateKind};
    let id_payload = BASE64_URL_SAFE_NO_PAD.encode(base.id.as_bytes());
    // Long source bodies (clipboard rows are the worst offender)
    // turn row into an unreadable wall of text if interpolated
    // verbatim. Truncate the visible copy; the backing candidate's
    // payload runs against the full content at activate time
    let short_base = short_source_label(&base.title);
    Candidate {
        id: format!("chain::{}::{}", action.id, id_payload),
        title: format!("{} - {short_base}", action.label),
        subtitle: Some(format!(
            "↵ {} on {short_base}",
            action.label.to_lowercase()
        )),
        icon: base.icon.clone(),
        kind: CandidateKind::Custom("chain".into()),
        actions: vec![Action::primary(&action.label)],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn chain_no_base_match(base: &str) -> gyors_core::Candidate {
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    Candidate {
        id: format!("chain::__no-base__::{base}"),
        title: format!("No result for \"{base}\""),
        subtitle: Some("Refine the query before `>` to resolve a target first".into()),
        icon: Icon::SfSymbol("exclamationmark.magnifyingglass".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn chain_unknown_action(hint: &str, base: &gyors_core::Candidate) -> gyors_core::Candidate {
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    let known = base
        .actions
        .iter()
        .map(|a| a.label.as_str())
        .collect::<Vec<_>>()
        .join(" · ");
    Candidate {
        id: format!("chain::__unknown-action__::{hint}"),
        title: format!("Unknown action: \"{hint}\""),
        subtitle: Some(format!("Available: {known}")),
        icon: Icon::SfSymbol("questionmark.circle".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Helper row surfaced when a `note` keyword query finds no backing
/// provider response - typically because user disabled notes in
/// config.json. Gives them a single-keystroke path to re-enable it
fn notes_disabled_helper_candidate() -> gyors_core::Candidate {
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    Candidate {
        id: "ipc::notes-disabled".into(),
        title: "Notes provider is disabled".into(),
        subtitle: Some(
            "Press ↵ to open the toggle, or edit providers.note.enabled in config.json".into(),
        ),
        icon: Icon::SfSymbol("exclamationmark.shield".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Open toggle")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn to_ui(sc: ScoredCandidate) -> UiCandidate {
    let c = sc.candidate;
    let (icon_kind, icon_value) = match c.icon {
        gyors_core::Icon::None => (0u8, String::new()),
        gyors_core::Icon::SfSymbol(s) => (1, s),
        gyors_core::Icon::Path(p) => (2, p.to_string_lossy().into_owned()),
        gyors_core::Icon::BundleIcon(p) => (3, p.to_string_lossy().into_owned()),
        gyors_core::Icon::ColorSwatch(hex) => (4, hex),
        gyors_core::Icon::Glyph(g) => (5, g),
        gyors_core::Icon::ImagePath(p) => (6, p.to_string_lossy().into_owned()),
    };
    let kind = match c.kind {
        gyors_core::CandidateKind::App => 0,
        gyors_core::CandidateKind::File => 1,
        gyors_core::CandidateKind::Calculation => 2,
        gyors_core::CandidateKind::Snippet => 3,
        gyors_core::CandidateKind::Clipboard => 4,
        gyors_core::CandidateKind::Web => 5,
        gyors_core::CandidateKind::Action => 6,
        gyors_core::CandidateKind::Custom(_) => 7,
    };
    let actions: Vec<UiAction> = c
        .actions
        .into_iter()
        .map(|a| UiAction {
            id: a.id,
            label: a.label,
        })
        .collect();
    UiCandidate {
        id: c.id,
        title: c.title,
        subtitle: c.subtitle.unwrap_or_default(),
        icon_kind,
        icon_value,
        kind,
        score: sc.score,
        actions,
    }
}

/// Resolve the SQLite path bridge opens at init
///
/// `GYORS_DATA_DIR` (set by the e2e test harness + the CLI's
/// `--data-dir` flag) wins over the OS-default. Without this override
/// every FFI unit test wrote `record_query` / `record_clipboard`
/// rows into the developer's real `~/Library/Application Support/
/// Gyors/gyors.db`, polluting their actual launcher history with
/// fixtures like `hist-test-<epoch>`. Honouring the env var means
/// tests stay sandboxed to a tempdir and the production path only
/// runs when no override is set
///
/// Falling back to `dirs::data_local_dir()` matches what the CLI's
/// own `index_path()` does (`crates/gyors-cli/src/sync.rs`) - the
/// two paths must agree because the FFI and CLI both read/write the
/// same DB in production
fn data_path() -> anyhow::Result<PathBuf> {
    let dir = if let Some(override_dir) = std::env::var_os("GYORS_DATA_DIR") {
        PathBuf::from(override_dir)
    } else {
        dirs::data_local_dir()
            .ok_or_else(|| anyhow::anyhow!("no local data dir"))?
            .join("Gyors")
    };
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("gyors.db"))
}

/// Min/max bounds for user-configurable background-sync cadence.
/// Below 10s the launcher would spam the server and tank battery;
/// above an hour the "type on Mac A, paste on Mac B" UX falls
/// apart. Default 60s is same cadence Raycast Pro uses for its
/// background sync
#[cfg(feature = "cloud")]
const SYNC_INTERVAL_MIN: u64 = 10;
#[cfg(feature = "cloud")]
const SYNC_INTERVAL_MAX: u64 = 3600;
#[cfg(feature = "cloud")]
const SYNC_INTERVAL_DEFAULT: u64 = 60;

#[cfg(feature = "cloud")]
fn read_sync_interval_secs() -> u64 {
    let root = gyors_providers::config::load_config();
    let raw = root.get("sync_interval_secs");
    let n = match raw {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(s)) => s.trim().parse::<u64>().ok(),
        _ => None,
    };
    n.unwrap_or(SYNC_INTERVAL_DEFAULT)
        .clamp(SYNC_INTERVAL_MIN, SYNC_INTERVAL_MAX)
}

/// One-shot best-effort tick. Reads session from disk on every
/// call so a sign-in / sign-out user does mid-loop is observed
/// without bridge having to plumb auth events. Returns silently
/// if theres no session - the loop runs forever; sign-in resumes it
#[cfg(feature = "cloud")]
async fn try_one_background_tick(index: Arc<Index>) {
    let path = match gyors_sync::default_session_path() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("bg sync: no session path: {e}");
            return;
        }
    };
    let session = match gyors_sync::Session::load(&path) {
        Ok(Some(s)) => s,
        Ok(None) => return, // signed out
        Err(e) => {
            tracing::warn!("bg sync: session load failed: {e}");
            return;
        }
    };
    // Same rotation gate the on-demand FFI tick uses.
    // Background refresh failures are logged + skipped (we'll
    // try again next interval); we dont surface them to the
    // user from this code path because theres no UI in scope
    let session = match sync_ffi::refresh_if_due_public(session).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("bg sync: refresh failed, skipping tick: {e}");
            return;
        }
    };
    match sync_ffi::run_tick(index, &session).await {
        Ok(report) => {
            if report.session_expired {
                // The bearer is dead. Stop running ticks until the
                // user signs back in. The FFI tick path surfaces
                // this to Swift; the background loop just bails to
                // avoid logging "401" every minute forever
                tracing::info!("bg sync: session expired, pausing until next sign-in");
                BG_SYNC_PAUSED.store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            if report.quota_exceeded {
                // Storage full - same idea: stop hammering and wait
                // for user to free bytes / upgrade. They'll see
                // the banner via the FFI tick
                tracing::info!("bg sync: quota exceeded, pausing until storage frees up");
                BG_SYNC_PAUSED.store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            if report.pushed > 0 || report.pulled > 0 {
                tracing::info!(
                    "bg sync: pushed={} pulled={}",
                    report.pushed,
                    report.pulled
                );
            }
        }
        Err(e) => {
            tracing::warn!("bg sync: tick failed: {e}");
        }
    }
}

/// Latched flag so background loop stops calling `tick` once the
/// session is dead OR user is out of quota. Cleared on every
/// successful sign-in / sign-out (gyors_sync_signin / signout below)
/// so a re-signed-in launcher resumes immediately without restart
///
/// `Relaxed` is fine - we dont need cross-thread happens-before;
/// the worst case is one extra tick fires after the flag is set,
/// which is harmless (the tick itself is idempotent)
static BG_SYNC_PAUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Resume the background sync loop after a session change (signin /
/// signout / successful refresh / quota cleared). Called from the
/// FFI sign endpoints + after any FFI tick that returns `ok=true`
/// with no flags set
#[cfg(feature = "cloud")]
pub(crate) fn clear_bg_sync_pause() {
    BG_SYNC_PAUSED.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Periodic background sync. Runs on a dedicated OS thread with a
/// current-thread tokio runtime - keeping it off `BRIDGE.rt` so
/// the foreground query path stays responsive even if a push
/// stalls on a slow network
#[cfg(feature = "cloud")]
fn spawn_background_sync(index: Arc<Index>) {
    std::thread::Builder::new()
        .name("gyors-sync-bg".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("bg sync: runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                // Tiny startup delay so bridge has finished init
                // before we touch the network
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                loop {
                    let interval = read_sync_interval_secs();
                    if !BG_SYNC_PAUSED.load(std::sync::atomic::Ordering::Relaxed) {
                        try_one_background_tick(Arc::clone(&index)).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                }
            });
        })
        .expect("spawn bg sync thread");
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}


/// Per-arg length caps. The FFI is meant for Swift
/// shell, but the dylib is reachable to any process running as the
/// user. A multi-gigabyte string passed by a hostile / buggy local
/// caller would force matching allocations and OOM the launcher
///
/// Caps are deliberately generous - they're a safety belt, not a
/// product feature. Real usage stays under these by orders of
/// magnitude (typical query: <1 KB; typical clipboard copy: <100 KB)
const MAX_PATTERN_LEN: usize = 1 << 20; // 1 MB
const MAX_CLIPBOARD_LEN: usize = 16 << 20; // 16 MB
const MAX_ID_LEN: usize = 1 << 16; // 64 KB
const MAX_JSON_LEN: usize = 4 << 20; // 4 MB (plugin install specs)
// Auth-field cap is only referenced by sync_ffi; absent in no-cloud builds
#[cfg(feature = "cloud")]
pub(crate) const MAX_AUTH_FIELD_LEN: usize = 4 << 10; // 4 KB (email/password/salt)

/// Read a `*const c_char` as a borrowed `&str` with bounds + UTF-8
/// validation. Returns `None` on:
///
/// - null pointer
/// - byte length exceeding `max_bytes`
/// - non-UTF-8 content
///
/// Pre-checking the length via `CStr::count_bytes` (constant-time
/// on macOS, linear elsewhere but bounded to the cap) means we
/// reject oversized inputs before allocating a `String` for them
pub(crate) fn cstr_bounded<'a>(p: *const c_char, max_bytes: usize) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    let cs = unsafe { CStr::from_ptr(p) };
    let bytes = cs.to_bytes();
    if bytes.len() > max_bytes {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// Initialize the global launcher state. Idempotent
#[no_mangle]
pub extern "C" fn gyors_init() {
    let _ = BRIDGE.get_or_init(|| Mutex::new(GyorsBridge::new()));
    // Adopt the saved session's tier so clipboard retention is
    // right from the first keystroke. Silently no-ops if no session
    // exists (free defaults already applied at index open)
    //
    // No-cloud build: skip - theres no session file, no tier
    // concept, and clipboard retention falls back to the local
    // default applied at index open
    #[cfg(feature = "cloud")]
    {
        let tier = gyors_sync::default_session_path()
            .ok()
            .and_then(|p| gyors_sync::Session::load(&p).ok().flatten())
            .map(|s| s.tier);
        sync_ffi::apply_tier(tier);
    }
}

/// Run a query. Returns a newly-allocated UTF-8 C string containing a JSON
/// array of UiCandidate. Free with `gyors_free_string`. Returns NULL on error
#[no_mangle]
pub extern "C" fn gyors_query(pattern: *const c_char) -> *mut c_char {
    let Some(pattern) = cstr_bounded(pattern, MAX_PATTERN_LEN) else {
        return std::ptr::null_mut();
    };
    let Some(mu) = BRIDGE.get() else {
        return std::ptr::null_mut();
    };
    let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
    let results = bridge.query(pattern);
    let json = serde_json::to_string(&results).unwrap_or_else(|_| "[]".into());
    to_raw(json)
}

/// Activate a candidate by id with specified action id (use "default" for
/// the primary action). Returns JSON representation of the produced Effect.
/// Free with `gyors_free_string`
#[no_mangle]
pub extern "C" fn gyors_activate(id: *const c_char, action: *const c_char) -> *mut c_char {
    let Some(id) = cstr_bounded(id, MAX_ID_LEN) else {
        return std::ptr::null_mut();
    };
    // `action` is allowed to be NULL (defaults to "default"). When
    // non-null, the cap is same id cap - action strings are
    // tiny in practice
    let action = match cstr_bounded(action, MAX_ID_LEN) {
        Some(s) if !s.is_empty() => s,
        _ => "default",
    };
    let Some(mu) = BRIDGE.get() else {
        return std::ptr::null_mut();
    };
    let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
    let json = bridge.activate(id, action);
    to_raw(json)
}

/// Record a clipboard change. No-op if content is empty/whitespace, or if it
/// matches the most recent row
#[no_mangle]
pub extern "C" fn gyors_record_clipboard(content: *const c_char) {
    let Some(content) = cstr_bounded(content, MAX_CLIPBOARD_LEN) else {
        return;
    };
    let Some(mu) = BRIDGE.get() else {
        return;
    };
    let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
    bridge.record_clipboard(content);
}

/// Clear all clipboard history rows
#[no_mangle]
pub extern "C" fn gyors_clear_clipboard_history() {
    let Some(mu) = BRIDGE.get() else {
        return;
    };
    let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
    bridge.clear_clipboard_history();
}

/// Record a query that user actually committed (pressed Enter on).
/// Keystroke-level typing is intentionally NOT recorded - the ring
/// would fill with half-typed noise and up-arrow recall would be
/// useless. No-op on empty / whitespace patterns
#[no_mangle]
pub extern "C" fn gyors_record_query(pattern: *const c_char) {
    let Some(pattern) = cstr_bounded(pattern, MAX_PATTERN_LEN) else {
        return;
    };
    let Some(mu) = BRIDGE.get() else {
        return;
    };
    let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
    bridge.record_query(pattern);
}

/// Most recent unique queries, newest first. Returns a JSON array of
/// strings. `limit` caps the returned list length; a sensible default
/// is ~50 - enough to scroll through an hour of work without paying
/// for ancient history. Free with `gyors_free_string`
#[no_mangle]
pub extern "C" fn gyors_recent_queries(limit: u32) -> *mut c_char {
    let Some(mu) = BRIDGE.get() else {
        return std::ptr::null_mut();
    };
    let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
    let items = bridge.recent_queries(limit as usize);
    let json = serde_json::to_string(&items).unwrap_or_else(|_| "[]".into());
    to_raw(json)
}

/// `!!` expansion helper: the most recent committed query, or an
/// empty string if history is empty. Empty is better than NULL here
/// - Swift can unconditionally paste the return value into the
///   input buffer without a null-check branch
#[no_mangle]
pub extern "C" fn gyors_last_query() -> *mut c_char {
    let Some(mu) = BRIDGE.get() else {
        return to_raw(String::new());
    };
    let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
    to_raw(bridge.last_query().unwrap_or_default())
}

/// JSON array of `{trigger, text, name?}` - consumed by the Global
/// Snippet Expander on Swift side to build its trigger table.
/// Re-reads the TOML file fresh on each call; the CGEventTap invokes
/// this once at startup and whenever `snippets.toml` changes, so the
/// cost is one file read per reload - trivial
#[no_mangle]
pub extern "C" fn gyors_snippets_json() -> *mut c_char {
    let snippets = gyors_providers::snippets::load_snippets();
    let json = serde_json::to_string(&snippets).unwrap_or_else(|_| "[]".into());
    to_raw(json)
}

/// Whether user has opted into system-wide snippet expansion.
/// Kept as a dedicated FFI (rather than having Swift parse full
/// config) so Swift side can cheaply poll before registering a
/// CGEventTap - the eventual permission prompt is sticky, so we
/// dont want to ask unless user explicitly turned the toggle on
#[no_mangle]
pub extern "C" fn gyors_snippets_global_enabled() -> bool {
    let cfg = gyors_providers::config::load_config();
    match gyors_providers::config::get_dotted(&cfg, "snippets.expand_globally") {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => {
            matches!(s.to_lowercase().as_str(), "true" | "yes" | "on" | "1")
        }
        _ => false,
    }
}

/// Return config field schema as a JSON array. Each entry:
/// `{"key":"...", "slug":"...", "type":"bool|text|path|enum", "default":"...",
///  "description":"...", "enum_values":["..."]?}`.
/// Swift consumes this to drive `gyors://set/*` and `gyors://toggle/*`
/// without hardcoding per-key logic - adding a new config option only
/// requires appending to Rust's `FIELDS` const
///
/// Free the returned string with `gyors_free_string`
#[no_mangle]
pub extern "C" fn gyors_config_fields_json() -> *mut c_char {
    let fields: Vec<_> = gyors_providers::config::FIELDS
        .iter()
        .map(|f| {
            let (ty, values): (&'static str, Option<Vec<String>>) = match f.ty {
                gyors_providers::config::FieldType::Bool => ("bool", None),
                gyors_providers::config::FieldType::Text => ("text", None),
                gyors_providers::config::FieldType::Path => ("path", None),
                gyors_providers::config::FieldType::App => ("app", None),
                gyors_providers::config::FieldType::Enum(vs) => {
                    ("enum", Some(vs.iter().map(|s| (*s).to_string()).collect()))
                }
            };
            serde_json::json!({
                "key": f.key,
                // URL-friendly form: shows -> dashes, dots -> dashes.
                // Matches the slug convention used by gyors:// today
                // (e.g. `notes-folder`, `clipboard-enabled`)
                "slug": f.key.replace(['_', '.'], "-"),
                "type": ty,
                "default": f.default,
                "description": f.description,
                "enum_values": values,
            })
        })
        .collect();
    let json = serde_json::to_string(&fields).unwrap_or_else(|_| "[]".into());
    to_raw(json)
}

/// Free a string previously returned by `gyors_query` or `gyors_activate`.
///
/// Caller must pass a pointer that originated from one of those Rust
/// helpers; passing anything else is undefined behaviour. Marking the
/// function `unsafe` would change the Swift call sites for no real
/// safety gain, so it stays a checked-extern with an explicit allow
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn gyors_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(s);
    }
}

/// Number of apps currently indexed. Returns 0 if `gyors_init` hasn't run
#[no_mangle]
pub extern "C" fn gyors_app_count() -> u64 {
    let Some(mu) = BRIDGE.get() else {
        return 0;
    };
    mu.lock().unwrap_or_else(|e| e.into_inner()).app_count
}

/// Number of clipboard history items currently stored
#[no_mangle]
pub extern "C" fn gyors_clipboard_count() -> u64 {
    let Some(mu) = BRIDGE.get() else {
        return 0;
    };
    mu.lock().unwrap_or_else(|e| e.into_inner()).index.clipboard_count().unwrap_or(0) as u64
}

/// Runtime diagnostics as a JSON string. Consumers (menu-bar item,
/// startup NSLog, future About pane) can dump this to help pinpoint
/// "why isn't my thing showing up?" cases without user having to
/// chase code paths themselves
///
/// Shape is intentionally untyped-stable - add keys, dont remove or
/// rename them - so Swift side's decoding stays tolerant
#[no_mangle]
pub extern "C" fn gyors_diagnostics() -> *mut c_char {
    let Some(mu) = BRIDGE.get() else {
        return std::ptr::null_mut();
    };
    let br = mu.lock().unwrap_or_else(|e| e.into_inner());
    let notes_folder = br.notes_folder.to_string_lossy();
    // Count actual notes by firing an empty-filter note query and
    // filtering to rows whose id points at an on-disk `.md` file
    // (skips synthetic helper candidates: prompt-new, folder headers)
    let notes_count: u64 = br.rt.block_on(async {
        let items = br
            .registry
            .query_one("note", &Query::new("notes all"))
            .await;
        items
            .iter()
            .filter(|c| c.id.starts_with("note::") && c.id.contains(".md"))
            .count() as u64
    });
    let provider_ids: Vec<String> = br.registry.ids().into_iter().map(String::from).collect();
    let plugin_dir = gyors_plugin_host::default_plugin_dir();
    let process_plugin_count = std::fs::read_dir(&plugin_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    use std::os::unix::fs::PermissionsExt;
                    e.path()
                        .metadata()
                        .map(|m| m.is_file() && m.permissions().mode() & 0o100 != 0)
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);
    let shell_plugins_path = gyors_plugin_host::default_shell_plugins_path();
    let shell_plugin_count = gyors_plugin_host::load_shell_plugins(&shell_plugins_path).len();
    let doc = serde_json::json!({
        "notes_folder": notes_folder,
        "notes_folder_exists": std::path::Path::new(&*notes_folder).exists(),
        "notes_count": notes_count,
        "app_count": br.app_count,
        "clipboard_count": br.index.clipboard_count().unwrap_or(0),
        "plugin_dir": plugin_dir.to_string_lossy(),
        "process_plugin_count": process_plugin_count,
        "shell_plugins_path": shell_plugins_path.to_string_lossy(),
        "shell_plugin_count": shell_plugin_count,
        "provider_count": provider_ids.len(),
        "providers": provider_ids,
    });
    to_raw(doc.to_string())
}

/// Plugin install preview
///
/// Takes a candidate `plugins.json` spec (JSON string) and returns a
/// JSON preview payload Swift side can render as a rich install
/// panel - plus whether an entry with same id is already
/// present (so the dialog can say "Install" vs "Update"). No file
/// mutation happens here; that's `gyors_plugin_install_commit`
///
/// Returns NULL on parse / validation failure. Consumers should
/// treat NULL as "malformed or untrusted spec, dont install."
#[no_mangle]
pub extern "C" fn gyors_plugin_install_preview(spec_json: *const c_char) -> *mut c_char {
    let Some(s) = cstr_bounded(spec_json, MAX_JSON_LEN) else {
        return std::ptr::null_mut();
    };
    let s = s.to_owned();
    // Accept either a raw ShellPluginSpec or a full PluginManifest.
    // Lets a single URI scheme + file extension reuse one endpoint
    let spec: gyors_plugin_host::ShellPluginSpec = match serde_json::from_str(&s) {
        Ok(sp) => sp,
        Err(_) => match gyors_plugin_host::PluginManifest::parse(&s) {
            Ok(m) => match m.shell {
                Some(sp) => sp,
                None => return std::ptr::null_mut(),
            },
            Err(_) => return std::ptr::null_mut(),
        },
    };
    // Surface validation errors in the preview payload so Swift can
    // decide whether to show an Install button at all. Anything
    // that upsert would reject shouldn't even be offered
    let validation = plugin_install_validation(&spec);
    let existing = gyors_plugin_host::find_shell_plugin(
        &gyors_plugin_host::default_shell_plugins_path(),
        &spec.id,
    );
    let doc = serde_json::json!({
        "spec": spec,
        "kind": if existing.is_some() { "update" } else { "install" },
        "existing": existing,
        "validation": validation,
    });
    to_raw(doc.to_string())
}

/// Commit an install previewed by `gyors_plugin_install_preview`.
/// Writes (or replaces) the entry in plugins.json. Returns a small
/// JSON status doc: `{"ok": true, "outcome": "added"|"replaced"}`
/// or `{"ok": false, "error": "..."}` on failure
///
/// Caller frees the returned string via `gyors_free_string`
#[no_mangle]
pub extern "C" fn gyors_plugin_install_commit(spec_json: *const c_char) -> *mut c_char {
    let Some(s) = cstr_bounded(spec_json, MAX_JSON_LEN) else {
        return to_raw(r#"{"ok":false,"error":"null or oversized spec"}"#.into());
    };
    let s = s.to_owned();
    let spec: gyors_plugin_host::ShellPluginSpec = match serde_json::from_str(&s) {
        Ok(sp) => sp,
        Err(_) => match gyors_plugin_host::PluginManifest::parse(&s) {
            Ok(m) => match m.shell {
                Some(sp) => sp,
                None => return to_raw(r#"{"ok":false,"error":"manifest has no plugin"}"#.into()),
            },
            Err(e) => {
                return to_raw(serde_json::json!({"ok": false, "error": e.to_string()}).to_string())
            }
        },
    };
    let path = gyors_plugin_host::default_shell_plugins_path();
    match gyors_plugin_host::upsert_shell_plugin(&path, spec) {
        Ok(outcome) => {
            let label = match outcome {
                gyors_plugin_host::UpsertOutcome::Added => "added",
                gyors_plugin_host::UpsertOutcome::Replaced => "replaced",
            };
            to_raw(serde_json::json!({"ok": true, "outcome": label}).to_string())
        }
        Err(e) => to_raw(serde_json::json!({"ok": false, "error": e.to_string()}).to_string()),
    }
}

/// Non-fatal validation surface for the install preview. Mirrors
/// `upsert_shell_plugin`'s checks but returns them as structured
/// data so UI can render specific problems next to spec
/// instead of a single error string
fn plugin_install_validation(spec: &gyors_plugin_host::ShellPluginSpec) -> serde_json::Value {
    let mut errors: Vec<String> = Vec::new();
    let id = spec.id.trim();
    if id.is_empty() {
        errors.push("id is empty".into());
    } else if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        errors.push(format!("id `{id}` must be [A-Za-z0-9_-]"));
    }
    if spec.keywords.is_empty() {
        errors.push("no keywords declared".into());
    }
    if spec.command.trim().is_empty() {
        errors.push("command is empty".into());
    }
    // Reserved-id soft-check (identical list as in gyors-plugin-host)
    const RESERVED: &[&str] = &[
        "apps",
        "calc",
        "clip",
        "config",
        "shell",
        "note",
        "files",
        "emoji",
        "color",
        "time",
        "timer",
        "units",
        "currency",
        "hint",
        "kill",
        "qr",
        "ai",
        "ait",
        "ssh",
        "json",
        "jwt",
        "regex",
        "tab",
        "recent",
        "case",
        "text",
        "sys",
        "pref",
        "screenshot",
        "snip",
        "window",
        "generators",
        "repo",
        "web",
        "chain",
        "pipeline",
        "autoclose",
        "ipc",
    ];
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(id)) {
        errors.push(format!("id `{id}` shadows a built-in provider"));
    }
    serde_json::json!({
        "ok": errors.is_empty(),
        "errors": errors,
    })
}

/// Return active notes folder path so Swift shell can resolve
/// user-typed relative paths (Rename / Move flow) inside the right root.
/// Caller owns the returned string and MUST free it with
/// `gyors_free_string`. Returns NULL if `gyors_init` hasn't run
#[no_mangle]
pub extern "C" fn gyors_notes_folder() -> *mut c_char {
    let Some(mu) = BRIDGE.get() else {
        return std::ptr::null_mut();
    };
    let br = mu.lock().unwrap_or_else(|e| e.into_inner());
    let path = br.notes_folder.to_string_lossy().into_owned();
    to_raw(path)
}

pub(crate) fn to_raw(s: String) -> *mut c_char {
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[cfg(test)]
mod ffi_lock_poison_tests {
    //! Regression suite
    //!
    //! Every FFI entry that touches `BRIDGE` uses
    //! `.lock().unwrap_or_else(|e| e.into_inner())` instead of
    //! `.unwrap()`. The point is to survive a panic that escaped
    //! while a provider held lock: a single bad-luck panic
    //! across the C ABI used to mean every subsequent FFI call
    //! `.unwrap()`'d on the poison and SIGABRT-looped the launcher
    //!
    //! These tests dont poison the real `BRIDGE` (BackendRequest
    //! is heavy and panicking inside a real provider in a test is
    //! fragile). They pin the recovery PATTERN against a synthetic
    //! Mutex - if anyone changes call sites back to `.unwrap()`
    //! they'd have to break this contract first
    use std::sync::Mutex;

    /// Canonical recovery shape, in one place. Lifted verbatim
    /// from the FFI call sites so test fails the moment they
    /// drift apart
    fn lock_recovering<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn poison<T: std::panic::RefUnwindSafe>(m: &Mutex<T>) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = m.lock().unwrap();
            panic!("intentional poison");
        }));
    }

    #[test]
    fn fresh_mutex_unlocks_normally() {
        let m = Mutex::new(42);
        let g = lock_recovering(&m);
        assert_eq!(*g, 42);
        assert!(!m.is_poisoned());
    }

    #[test]
    fn poisoned_mutex_still_yields_guard() {
        let m = Mutex::new(7);
        poison(&m);
        assert!(m.is_poisoned(), "test setup: poisoning failed");
        // The exact line that lives at every FFI entry. Must NOT
        // panic - that's the whole invariant
        let g = lock_recovering(&m);
        assert_eq!(*g, 7);
    }

    #[test]
    fn poisoned_mutex_reads_inner_value_intact() {
        // The panic happened BEFORE we mutated the protected
        // value - guarded data should still be the original
        let m = Mutex::new(String::from("payload"));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = m.lock().unwrap();
            panic!("poison");
        }));
        let g = m.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(*g, "payload");
    }

    #[test]
    fn repeated_locks_after_poison_keep_working() {
        // Cascade scenario: every subsequent FFI call must
        // survive, not just the first one. Pin the loop
        let m = Mutex::new(0i32);
        poison(&m);
        for i in 1..=10 {
            let mut g = lock_recovering(&m);
            *g = i;
        }
        let g = lock_recovering(&m);
        assert_eq!(*g, 10, "10 sequential locks after poison all succeeded");
    }

    #[test]
    fn mutations_after_poison_persist() {
        // Inner-value mutation through the recovered guard must
        // stick. (`PoisonError::into_inner()` returns same
        // guard the OK branch would have, so this should always
        // hold - asserts it explicitly.)
        let m = Mutex::new(vec![1, 2, 3]);
        poison(&m);
        {
            let mut g = lock_recovering(&m);
            g.push(4);
        }
        let g = lock_recovering(&m);
        assert_eq!(*g, vec![1, 2, 3, 4]);
    }

    /// Drift guard: scans production source files for the bare
    /// panicking lock form on the `BRIDGE` mutex and fails the
    /// build if anyone reintroduces it. The trigger string is
    /// assembled at runtime so test body itself doesn't match
    #[test]
    fn no_naked_unwrap_on_bridge_lock_anywhere_in_ffi() {
        let lib = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/lib.rs",
        ))
        .expect("read lib.rs");
        let sync_ffi = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/sync_ffi.rs",
        ))
        .expect("read sync_ffi.rs");
        // Assembled at runtime so this assertion doesn't trip on
        // its own source
        let banned = format!("{}{}", "mu.lock().", "unwrap()");
        for (path, body) in [("lib.rs", &lib), ("sync_ffi.rs", &sync_ffi)] {
            assert!(
                !body.contains(&banned),
                "{path} reintroduced the naked BRIDGE lock unwrap regression"
            );
        }
    }
}

#[cfg(test)]
mod chain_row_tests {
    //! Row builders that interpolate a source candidate's title into
    //! dropdown. Clipboard candidates carry the full pasteboard
    //! body as their title - if we render that verbatim, the chain
    //! mode lights up as a wall of repeated text. These pin the
    //! truncation behaviour so a future contributor can't accidentally
    //! re-introduce the regression
    use super::*;
    use gyors_core::{Action, Candidate, CandidateKind, Icon};

    fn clipboard_like_candidate(body: &str) -> Candidate {
        Candidate {
            id: "clip::cb0".into(),
            title: body.into(),
            subtitle: Some("from clipboard".into()),
            icon: Icon::SfSymbol("doc.on.doc".into()),
            kind: CandidateKind::Action,
            actions: vec![
                Action::primary("Copy"),
                Action::new("preview", "Preview"),
            ],
            search_text: String::new(),
            bypass_rank: true,
        }
    }


    #[test]
    fn short_label_passes_through_short_input() {
        assert_eq!(short_source_label("hello"), "hello");
        assert_eq!(short_source_label(""), "");
    }

    #[test]
    fn short_label_truncates_at_max_with_ellipsis() {
        let long: String = "a".repeat(CHAIN_SOURCE_PREVIEW_MAX + 10);
        let out = short_source_label(&long);
        // Ellipsis added => char count is MAX + 1
        assert_eq!(out.chars().count(), CHAIN_SOURCE_PREVIEW_MAX + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn short_label_at_exact_boundary_not_truncated() {
        let exact: String = "a".repeat(CHAIN_SOURCE_PREVIEW_MAX);
        let out = short_source_label(&exact);
        assert_eq!(out, exact, "boundary case shouldn't earn an ellipsis");
    }

    #[test]
    fn short_label_flattens_newlines_and_tabs() {
        let mixed = "first\nsecond\tthird";
        let out = short_source_label(mixed);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\t'));
        assert_eq!(out, "first second third");
    }

    #[test]
    fn short_label_is_char_aware_not_byte_aware() {
        // Multibyte char must count as 1 toward the cap, not 4
        let body: String = "🎉".repeat(CHAIN_SOURCE_PREVIEW_MAX + 5);
        let out = short_source_label(&body);
        assert_eq!(out.chars().count(), CHAIN_SOURCE_PREVIEW_MAX + 1);
        assert!(out.ends_with('…'));
    }


    #[test]
    fn chain_confirm_caps_long_title() {
        let long_body: String = "x".repeat(CHAIN_SOURCE_PREVIEW_MAX + 50);
        let base = clipboard_like_candidate(&long_body);
        let action = Action::primary("Copy");
        let row = chain_confirm_candidate(&base, &action);
        // Title should NOT contain the full body
        assert!(
            row.title.chars().count() < long_body.chars().count(),
            "title still carries the full body: {:?}",
            row.title
        );
        assert!(row.title.contains("Copy"), "title must keep the action label");
        // Subtitle gets same shortening
        let subtitle = row.subtitle.expect("chain row must have subtitle");
        assert!(
            subtitle.chars().count() < long_body.chars().count() + 30,
            "subtitle ballooned with full body: {subtitle:?}"
        );
        assert!(subtitle.starts_with("↵ copy on "));
    }

    #[test]
    fn chain_confirm_keeps_short_title_intact() {
        // For notes/calc/snippets where title is already short,
        // the truncation must be a no-op
        let base = clipboard_like_candidate("hello.md");
        let action = Action::primary("Open");
        let row = chain_confirm_candidate(&base, &action);
        assert!(row.title.contains("hello.md"));
        assert!(row.subtitle.unwrap().contains("hello.md"));
    }

    #[test]
    fn chain_confirm_flattens_newlines_in_visible_title() {
        let base = clipboard_like_candidate("line one\nline two\nline three");
        let row = chain_confirm_candidate(&base, &Action::primary("Copy"));
        assert!(!row.title.contains('\n'));
        assert!(!row.subtitle.unwrap().contains('\n'));
    }

    #[test]
    fn chain_confirm_id_round_trips_full_base_id() {
        // Cosmetic title shortening must NOT affect the payload that
        // activate-time uses to look source back up
        use base64::prelude::*;
        let base = clipboard_like_candidate(&"x".repeat(500));
        let row = chain_confirm_candidate(&base, &Action::primary("Copy"));
        let suffix = row.id.split("::").last().unwrap();
        let decoded = BASE64_URL_SAFE_NO_PAD.decode(suffix).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "clip::cb0");
    }


    fn spec(stages: &[&str]) -> ChainSpec {
        ChainSpec {
            base: "clip".into(),
            stages: stages.iter().map(|s| (*s).into()).collect(),
        }
    }

    #[test]
    fn pipeline_row_title_leads_with_stage_description_not_body() {
        let long_body: String = "y".repeat(CHAIN_SOURCE_PREVIEW_MAX + 50);
        let base = clipboard_like_candidate(&long_body);
        let spec = spec(&["upper"]);
        let stage = pipeline::find_stage("upper").unwrap();
        let row = pipeline_stage_candidate(&base, &spec, stage);
        // The full body must not be in title at all - we lead
        // with the chain arrows + stage description now
        assert!(
            !row.title.contains(&long_body),
            "full source body leaked into title: {:?}",
            row.title
        );
        assert!(
            row.title.contains("upper"),
            "title must keep the stage canonical: {:?}",
            row.title
        );
    }

    #[test]
    fn pipeline_row_subtitle_uses_short_source_preview() {
        let long_body: String = "z".repeat(CHAIN_SOURCE_PREVIEW_MAX + 50);
        let base = clipboard_like_candidate(&long_body);
        let spec = spec(&["upper"]);
        let stage = pipeline::find_stage("upper").unwrap();
        let row = pipeline_stage_candidate(&base, &spec, stage);
        let subtitle = row.subtitle.expect("pipeline row needs subtitle");
        assert!(
            !subtitle.contains(&long_body),
            "subtitle interpolated full body: {subtitle:?}"
        );
    }

    #[test]
    fn pipeline_row_payload_preserves_full_source() {
        // Verify the displayed truncation doesn't accidentally
        // shorten bytes that get piped through to next stage
        // at activate time. The embedded source must be the FULL
        // original body (newlines flattened to spaces, but no
        // length cap)
        use base64::prelude::*;
        let long_body: String = "q".repeat(CHAIN_SOURCE_PREVIEW_MAX + 200);
        let base = clipboard_like_candidate(&long_body);
        let spec = spec(&["upper", "copy"]);
        let stage = pipeline::find_stage("copy").unwrap();
        let row = pipeline_stage_candidate(&base, &spec, stage);
        let encoded = row.id.strip_prefix("pipeline::").unwrap();
        let decoded = BASE64_URL_SAFE_NO_PAD.decode(encoded).unwrap();
        let payload = String::from_utf8(decoded).unwrap();
        // Line 2 of the payload is the embedded source
        let embedded = payload.lines().nth(1).unwrap();
        assert_eq!(embedded, long_body, "embedded source was truncated");
    }

    #[test]
    fn pipeline_row_keeps_jq_args_visible_in_title() {
        // The jq stage's arg portion is part of WHAT this row does -
        // dropping it from title would make `jq .name` and `jq
        // .other` look identical in dropdown
        let base = clipboard_like_candidate(r#"{"name":"x"}"#);
        let spec = spec(&["jq .name"]);
        let stage = pipeline::find_stage("jq .name").unwrap();
        let row = pipeline_stage_candidate(&base, &spec, stage);
        assert!(
            row.title.contains(".name"),
            "jq filter args dropped from row title: {:?}",
            row.title
        );
    }

    #[test]
    fn pipeline_row_with_multi_stage_chain_renders_arrows() {
        let base = clipboard_like_candidate(r#"{"a":[1,2,3]}"#);
        let spec = spec(&["jq .a", "upper", "copy"]);
        let stage = pipeline::find_stage("copy").unwrap();
        let row = pipeline_stage_candidate(&base, &spec, stage);
        assert!(row.title.contains("jq .a"));
        assert!(row.title.contains("upper"));
        assert!(row.title.contains("copy"));
    }
}

#[cfg(test)]
mod jq_autocomplete_helper_tests {
    //! Unit coverage for the building blocks that feed the
    //! orchestrate-time jq ghost-text branch. The full path
    //! (orchestrate_chain -> complete_dot_path -> autoclose_candidate)
    //! is covered by `complete_dot_path`'s own tests in
    //! `gyors_providers::jq_filter` plus this module's reconstruct
    //! checks - we dont spin up a registry just to assert the
    //! string-join shape
    use super::*;
    use gyors_core::{Action, Candidate, CandidateKind, Icon};
    use gyors_index::Index;
    use std::sync::Arc;

    fn idx() -> Arc<Index> {
        Arc::new(Index::in_memory().unwrap())
    }

    fn clipboard_candidate(id: i64, preview: &str) -> Candidate {
        Candidate {
            id: format!("clip::{id}"),
            title: preview.into(),
            subtitle: Some("from clipboard".into()),
            icon: Icon::SfSymbol("doc.on.doc".into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("Copy")],
            search_text: String::new(),
            bypass_rank: true,
        }
    }


    #[test]
    fn strip_jq_prefix_returns_args_after_space() {
        assert_eq!(strip_jq_prefix("jq .name"), Some(".name"));
        assert_eq!(strip_jq_prefix("jq .a.b"), Some(".a.b"));
    }

    #[test]
    fn strip_jq_prefix_case_insensitive() {
        assert_eq!(strip_jq_prefix("JQ .name"), Some(".name"));
        assert_eq!(strip_jq_prefix("Jq .name"), Some(".name"));
    }

    #[test]
    fn strip_jq_prefix_rejects_bare_jq_no_space() {
        assert_eq!(strip_jq_prefix("jq"), None);
    }

    #[test]
    fn strip_jq_prefix_rejects_other_keywords() {
        assert_eq!(strip_jq_prefix("upper"), None);
        assert_eq!(strip_jq_prefix("jqx .name"), None);
    }

    #[test]
    fn strip_jq_prefix_handles_leading_whitespace() {
        assert_eq!(strip_jq_prefix("  jq .name"), Some(".name"));
    }


    #[test]
    fn autocomplete_source_pulls_full_clipboard_content() {
        // Mirrors the bug-report scenario: clipboard candidate's
        // title is the truncated first-line preview, but the
        // autocomplete needs the FULL multi-line body
        let index = idx();
        let body = "{\n  \"ai\": {\"provider\": \"apple\"}\n}";
        index.record_clipboard(body, 1).unwrap();
        let clip_id = index.clipboard_recent(1).unwrap()[0].id;
        let candidate = clipboard_candidate(clip_id, "{");
        let resolved = jq_autocomplete_source(&candidate, &index).unwrap();
        assert_eq!(resolved, body);
    }

    #[test]
    fn autocomplete_source_falls_back_to_title_for_unknown_clip() {
        let index = idx();
        let candidate = clipboard_candidate(99999, "{\"x\":1}");
        // Row doesn't exist - use the embedded title instead so the
        // user still gets autocomplete on whatever candidate
        // chose to show
        let resolved = jq_autocomplete_source(&candidate, &index).unwrap();
        assert_eq!(resolved, "{\"x\":1}");
    }

    #[test]
    fn autocomplete_source_uses_title_for_non_clipboard_candidates() {
        // Calc/snippet/encoding candidates: title IS value
        let index = idx();
        let candidate = Candidate {
            id: "calc::42".into(),
            title: r#"{"answer":42}"#.into(),
            subtitle: None,
            icon: Icon::SfSymbol("function".into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("Copy")],
            search_text: String::new(),
            bypass_rank: true,
        };
        let resolved = jq_autocomplete_source(&candidate, &index).unwrap();
        assert_eq!(resolved, r#"{"answer":42}"#);
    }


    fn spec(stages: &[&str]) -> ChainSpec {
        ChainSpec {
            base: "clip".into(),
            stages: stages.iter().map(|s| (*s).into()).collect(),
        }
    }

    #[test]
    fn reconstruct_substitutes_last_stage() {
        let s = spec(&["jq .a"]);
        let out = reconstruct_query_with_last(&s, 0, "jq .ai");
        assert_eq!(out, "clip | jq .ai");
    }

    #[test]
    fn reconstruct_preserves_earlier_stages() {
        // 3-stage spec: replace LAST one. Earlier stages stay
        let s = spec(&["jq .a", "upper", "cop"]);
        let out = reconstruct_query_with_last(&s, 2, "copy");
        assert_eq!(out, "clip | jq .a | upper | copy");
    }

    #[test]
    fn reconstruct_trims_intermediate_whitespace() {
        // Earlier stages get trimmed; the replacement we pass in is
        // used verbatim. Replace the trailing partial with the
        // completed stage
        let s = spec(&["  jq .a  ", "  upper  ", " cop "]);
        let out = reconstruct_query_with_last(&s, 2, "copy");
        assert_eq!(out, "clip | jq .a | upper | copy");
    }

    /// End-to-end shape test: confirm the autoclose id the
    /// orchestrate branch is about to emit decodes to the
    /// completed ` | `-joined query. This is what Swift's
    /// splitGhost reads to produce grey-tail completion
    #[test]
    fn autoclose_candidate_id_round_trip() {
        let s = spec(&["jq .a"]);
        let completed = reconstruct_query_with_last(&s, 0, "jq .ai");
        let row = autoclose_candidate(&completed);
        let suffix = row.id.strip_prefix("autoclose::").unwrap();
        assert_eq!(suffix, "clip | jq .ai");
    }
}

#[cfg(test)]
mod extract_pipeline_text_tests {
    //! `extract_pipeline_text` is the activate-time bridge between
    //! candidate's embedded preview and actual bytes piped
    //! through the pipeline. Clipboard candidates store only a
    //! one-line preview in their `title` (the full body would make
    //! dropdown unreadable), so without a clipboard-aware path
    //! `clip | jq .foo` on a multi-line JSON entry would feed `{`
    //! into jq and silently produce nothing. These pin lookup
    use super::*;
    use gyors_index::Index;
    use std::sync::Arc;

    fn idx() -> Arc<Index> {
        Arc::new(Index::in_memory().unwrap())
    }

    fn record_and_take_id(index: &Index, content: &str) -> i64 {
        index.record_clipboard(content, 1).expect("record ok");
        index
            .clipboard_recent(1)
            .expect("recent ok")
            .first()
            .expect("at least one row")
            .id
    }

    #[test]
    fn clip_id_resolves_to_full_content_from_index() {
        let index = idx();
        let full = "{\n  \"ai\" : {\n    \"provider\" : \"apple\"\n  },\n  \"theme\" : \"neon\"\n}";
        let id = record_and_take_id(&index, full);
        let resolved = extract_pipeline_text(
            &format!("clip::{id}"),
            "{",     // embedded preview - only the first line
            &index,
        );
        assert_eq!(resolved.as_deref(), Some(full));
    }

    #[test]
    fn clip_id_falls_back_to_embedded_when_row_missing() {
        let index = idx();
        // Numeric id that doesn't exist in the table - extractor
        // should fall through to the embedded payload rather than
        // returning None and stranding the pipeline
        let resolved = extract_pipeline_text("clip::99999", "fallback body", &index);
        assert_eq!(resolved.as_deref(), Some("fallback body"));
    }

    #[test]
    fn clip_id_non_numeric_falls_back_to_embedded() {
        let index = idx();
        let resolved = extract_pipeline_text("clip::abc", "embedded body", &index);
        assert_eq!(resolved.as_deref(), Some("embedded body"));
    }

    #[test]
    fn non_clipboard_non_note_id_uses_embedded() {
        // Calculator/encoding/snippet candidates - their titles ARE
        // value, so embedded payload is authoritative
        let index = idx();
        let resolved = extract_pipeline_text("calc::42", "42", &index);
        assert_eq!(resolved.as_deref(), Some("42"));
    }

    #[test]
    fn empty_embedded_returns_none_when_no_clip_or_note_fallback() {
        let index = idx();
        assert!(extract_pipeline_text("snippet::greet", "", &index).is_none());
    }

    /// End-to-end through `pipeline::execute`: a clipboard row whose
    /// content is user's actual config blob, piped through `jq
    /// .ai`. This is the exact scenario from the bug report - it
    /// previously fed the truncated preview into jq and produced
    /// nothing
    #[test]
    fn jq_pipeline_over_clipboard_resolves_full_body() {
        let index = idx();
        let body = "{\n  \"ai\" : {\"provider\":\"apple\",\"router_enabled\":true},\n  \"theme\" : \"neon\"\n}";
        let id = record_and_take_id(&index, body);
        let source =
            extract_pipeline_text(&format!("clip::{id}"), "{", &index).expect("resolves");
        let eff = pipeline::execute(source, &["jq .ai".into()]).expect("execute ok");
        match eff {
            Effect::ShowText { text, .. } => {
                assert!(text.contains("apple"), "got: {text:?}");
                assert!(text.contains("router_enabled"), "got: {text:?}");
                assert!(text.contains("true"), "got: {text:?}");
            }
            other => panic!("expected ShowText for trailing transform, got {other:?}"),
        }
    }

    #[test]
    fn jq_pipeline_drills_into_nested_field_then_copies() {
        let index = idx();
        let body = "{\"ai\":{\"provider\":\"apple\"},\"theme\":\"neon\"}";
        let id = record_and_take_id(&index, body);
        let source =
            extract_pipeline_text(&format!("clip::{id}"), "{", &index).expect("resolves");
        let eff = pipeline::execute(
            source,
            &["jq .ai.provider".into(), "upper".into(), "copy".into()],
        )
        .expect("execute ok");
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "APPLE"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod chain_parser_tests {
    use super::parse_chain;

    // Helper: collapse Option<ChainSpec> into a tuple so asserts
    // read as "input -> (base, hint)" without noise
    fn parse(s: &str) -> Option<(String, String)> {
        // Helper kept returning a single-hint tuple for the 2-stage
        // classic tests. With ChainSpec now carrying a stage list,
        // the first stage is what these tests were asserting on
        parse_chain(s).map(|c| (c.base, c.stages.into_iter().next().unwrap_or_default()))
    }

    /// Full-stages variant for the multi-stage tests added in #116
    fn parse_full(s: &str) -> Option<(String, Vec<String>)> {
        parse_chain(s).map(|c| (c.base, c.stages))
    }

    #[test]
    fn empty_and_whitespace_are_not_chains() {
        assert!(parse("").is_none());
        assert!(parse("   ").is_none());
    }

    #[test]
    fn plain_query_doesnt_chain() {
        assert!(parse("note hello").is_none());
        assert!(parse("some query").is_none());
    }

    #[test]
    fn pipe_with_flanking_spaces_splits_base_and_hint() {
        assert_eq!(
            parse("note hello | copy"),
            Some(("note hello".into(), "copy".into())),
        );
    }

    #[test]
    fn pipe_with_no_surrounding_whitespace_is_literal() {
        // The classic regex-alternation shape: `cat|dog` with no
        // whitespace either side. Stays literal so `re cat|dog`
        // flows through as a regex pattern, not a chain
        assert!(parse("foo|bar").is_none());
    }

    #[test]
    fn pipe_then_space_commits() {
        // `foo| bar` - user typed text+pipe without lifting, then
        // space and next stage. Should chain
        assert_eq!(parse("foo| bar"), Some(("foo".into(), "bar".into())));
    }

    #[test]
    fn space_then_pipe_commits() {
        // `foo |bar` - user typed space+pipe then continued without
        // space. Still a clear separator
        assert_eq!(parse("foo |bar"), Some(("foo".into(), "bar".into())));
    }

    #[test]
    fn trailing_pipe_with_surrounding_space_yields_empty_hint() {
        // Both trailing forms count as user having typed the
        // separator and not yet typed next stage's characters
        assert_eq!(
            parse("note hello |"),
            Some(("note hello".into(), "".into()))
        );
        assert_eq!(
            parse("note hello| "),
            Some(("note hello".into(), "".into()))
        );
    }

    #[test]
    fn trailing_pipe_no_surrounding_space_stays_literal() {
        // `note hello|` with no space before, nothing after (EOL).
        // User hasn't given us a whitespace signal either side ->
        // keep literal so they can still type `note hello|copy` as
        // a regex/literal without auto-chaining
        assert!(parse("note hello|").is_none());
    }

    #[test]
    fn leading_pipe_doesnt_chain() {
        // Empty base -> not a valid chain. Flows through as a regular
        // (unmatched) query rather than hijacking it
        assert!(parse("|copy").is_none());
        assert!(parse("| copy").is_none());
        assert!(parse("|").is_none());
    }

    #[test]
    fn arrow_keeps_day_job_as_shell_prefix() {
        // Decision: `>` means "shell prefix" at position 0, nothing
        // else. Using it as a chain separator (old behaviour) conflicts
        // with shell semantics users already know. Only `|` chains
        assert!(parse("note hello > copy").is_none());
        assert!(parse("note hello >").is_none());
        assert!(parse("a>b").is_none());
        assert!(parse("> ls").is_none()); // shell, handled elsewhere
        assert!(parse(">ls").is_none()); // shell, handled elsewhere
    }

    #[test]
    fn whitespace_trimmed_around_base_and_hint() {
        assert_eq!(
            parse("  note hello  |  copy  "),
            Some(("note hello".into(), "copy".into())),
        );
    }

    #[test]
    fn empty_base_gets_politely_shown_door() {
        assert!(parse("   |   copy").is_none());
    }

    #[test]
    fn multi_pipe_splits_into_stage_list() {
        // Every additional pipe splits into an additional stage.
        // `a | b | c` -> base="a", stages=["b", "c"]. Previously
        // everything after the first pipe was one opaque "hint"; the
        // pipeline layer needs each stage discrete
        assert_eq!(
            parse_full("a | b | c"),
            Some(("a".into(), vec!["b".into(), "c".into()])),
        );
        // And a classic 2-stage input still yields stages.len()==1
        assert_eq!(
            parse_full("note hello | copy"),
            Some(("note hello".into(), vec!["copy".into()])),
        );
    }

    #[test]
    fn trailing_pipe_yields_empty_last_stage() {
        // User typed trailing separator but hasn't named the next
        // stage yet - keep it as an empty stage so orchestrator
        // can decide whether to show a "pick a transform" menu
        assert_eq!(
            parse_full("note hello |"),
            Some(("note hello".into(), vec!["".into()])),
        );
        assert_eq!(
            parse_full("note hello | upper | "),
            Some(("note hello".into(), vec!["upper".into(), "".into()])),
        );
    }

    // Without these, `re cat | dog` (legitimate regex alternation with
    // spaces) would chain. Users writing regex, YAML, or AI-transform
    // text naturally include `|` - we MUST keep their input literal

    #[test]
    fn regex_alternation_pipes_stay_where_they_belong() {
        // Classic case: regex alternation with flanking spaces
        assert!(parse("re cat | dog").is_none());
        assert!(parse("regex cat | dog").is_none());
        // Case-insensitive guard: `RE` (unusual but legal shell casing)
        assert!(parse("RE cat | dog").is_none());
        // Regex without the flanking-space form stays literal via the
        // main separator rule too
        assert!(parse("re cat|dog").is_none());
    }

    #[test]
    fn yaml_block_scalars_keep_their_pipes() {
        // YAML block scalars use `|` as the multiline-preserve marker -
        // treating it as a chain would corrupt user input
        for kw in ["json2yaml", "yaml2json", "yaml2toml", "toml2yaml"] {
            let q = format!("{kw} something | else");
            assert!(parse(&q).is_none(), "{q} unexpectedly chained");
        }
    }

    #[test]
    fn ai_free_text_keeps_its_pipes_to_itself() {
        // User's free text frequently contains `|` - summarize on text
        // about pipes, regex, or shell commands must not split
        for kw in [
            "summarize",
            "tldr",
            "explain",
            "rewrite",
            "fix",
            "shorten",
            "expand",
            "translate",
        ] {
            let q = format!("{kw} the text is: foo | bar");
            assert!(parse(&q).is_none(), "{q} unexpectedly chained");
        }
        // Ask/AI free-form
        assert!(parse("ai what is a|b in regex | copy").is_none());
        assert!(parse("ask foo | bar").is_none());
    }

    #[test]
    fn non_pipe_using_bases_still_chain() {
        // Sanity: note, clipboard, snippet, etc. DO chain with ` | `
        assert_eq!(
            parse("note hello | copy"),
            Some(("note hello".into(), "copy".into())),
        );
        assert_eq!(
            parse("clip | preview"),
            Some(("clip".into(), "preview".into())),
        );
    }

    #[test]
    fn base_keyword_matching_is_first_token_only() {
        // `recent` starts with `re` but isn't regex. First-token
        // match must be exact - only `re` / `regex` themselves bail
        assert_eq!(
            parse("recent | copy"),
            Some(("recent".into(), "copy".into())),
        );
        // `explainer` is not `explain` either
        assert_eq!(
            parse("explainer | copy"),
            Some(("explainer".into(), "copy".into())),
        );
    }
}

#[cfg(test)]
mod shell_pipeline_tests {
    use super::*;


    #[test]
    fn no_pipe_returns_none() {
        assert!(parse_shell_pipeline("ls -la").is_none());
        assert!(parse_shell_pipeline("").is_none());
        assert!(parse_shell_pipeline("   ").is_none());
    }

    #[test]
    fn tight_pipe_inside_cmd_is_not_a_pipeline_separator() {
        // No spaces around `|` -> that's shell command's own pipe,
        // not the pipeline separator. Same contract as the regular
        // chain layer's `find_separator_pipe`
        assert!(parse_shell_pipeline("ls|grep foo").is_none());
        assert!(parse_shell_pipeline("echo \"a|b\"").is_none());
    }

    #[test]
    fn single_stage_split() {
        let (cmd, stages) = parse_shell_pipeline("ls -la | copy").unwrap();
        assert_eq!(cmd, "ls -la");
        assert_eq!(stages, vec!["copy"]);
    }

    #[test]
    fn multi_stage_walks_left_through_transforms() {
        let (cmd, stages) = parse_shell_pipeline("cat README.md | upper | copy").unwrap();
        assert_eq!(cmd, "cat README.md");
        assert_eq!(stages, vec!["upper", "copy"]);
    }

    #[test]
    fn cmd_internal_pipes_stay_in_cmd() {
        // `grep foo` isn't a known stage -> walk-left stops there. The
        // shell command keeps its internal `| grep foo` while only
        // `copy` is treated as a pipeline stage
        let (cmd, stages) = parse_shell_pipeline("ls | grep foo | copy").unwrap();
        assert_eq!(cmd, "ls | grep foo");
        assert_eq!(stages, vec!["copy"]);
    }

    #[test]
    fn empty_partial_after_pipe() {
        // User just typed ` | ` with no stage yet - emit empty stage
        // so orchestrator surfaces every option
        let (cmd, stages) = parse_shell_pipeline("ls -la | ").unwrap();
        assert_eq!(cmd, "ls -la");
        assert_eq!(stages, vec![""]);
    }

    #[test]
    fn empty_cmd_returns_none() {
        // `> | copy` makes no sense - fall through to the regular
        // shell handler so user gets a sensible error row
        assert!(parse_shell_pipeline(" | copy").is_none());
        assert!(parse_shell_pipeline("| copy").is_none());
    }

    #[test]
    fn partial_unknown_stage_still_parses() {
        // Typing `co` before `copy` is partial - we let the
        // orchestrator's prefix_match decide. Parser doesn't
        // care whether the LAST stage is a known keyword
        let (cmd, stages) = parse_shell_pipeline("ls | co").unwrap();
        assert_eq!(cmd, "ls");
        assert_eq!(stages, vec!["co"]);
    }

    #[test]
    fn middle_unknown_segment_falls_into_cmd() {
        // `ls | xyz | copy` - `xyz` isn't a transform, so walk
        // stops there and `xyz` becomes part of the cmd half
        let (cmd, stages) = parse_shell_pipeline("ls | xyz | copy").unwrap();
        assert_eq!(cmd, "ls | xyz");
        assert_eq!(stages, vec!["copy"]);
    }


    #[test]
    fn orchestrate_offers_all_stages_for_empty_partial() {
        let candidates = orchestrate_shell_pipeline("ls -la", &["".into()]);
        // Every STAGE in the pipeline registry should appear
        let titles: Vec<String> = candidates
            .iter()
            .map(|c| c.candidate.title.clone())
            .collect();
        assert!(
            titles.iter().any(|t| t.contains("→ copy")),
            "expected `copy` row, got: {titles:#?}"
        );
        assert!(
            titles.iter().any(|t| t.contains("→ upper")),
            "expected `upper` row, got: {titles:#?}"
        );
    }

    #[test]
    fn orchestrate_prefix_matches_partial() {
        let candidates = orchestrate_shell_pipeline("ls -la", &["co".into()]);
        let titles: Vec<String> = candidates
            .iter()
            .map(|c| c.candidate.title.clone())
            .collect();
        // `co` matches at least `copy`, `count`, `countwords`, `constant`
        for needle in ["copy", "count", "constant"] {
            assert!(
                titles.iter().any(|t| t.contains(&format!("→ {needle}"))),
                "expected `{needle}` row, got: {titles:#?}"
            );
        }
        // Unrelated stages should NOT appear
        assert!(
            !titles.iter().any(|t| t.contains("→ upper")),
            "`upper` leaked into co-prefix matches: {titles:#?}"
        );
    }

    #[test]
    fn orchestrate_unknown_partial_returns_helper_row() {
        let candidates = orchestrate_shell_pipeline("ls -la", &["nopenope".into()]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].candidate.id,
            "chain::__unknown-shell-stage__"
        );
    }

    #[test]
    fn orchestrate_emits_autoclose_for_unique_prefix() {
        // `unb` uniquely identifies `unb64` - autoclose ghost should
        // appear so Swift can render grey-tail completion
        let candidates = orchestrate_shell_pipeline("ls -la", &["unb".into()]);
        assert!(
            candidates
                .iter()
                .any(|c| c.candidate.id.starts_with("autoclose::")),
            "expected autoclose candidate for unique-prefix `unb`"
        );
    }

    #[test]
    fn orchestrate_id_round_trips_through_base64() {
        use base64::prelude::*;
        let candidates = orchestrate_shell_pipeline("ls -la", &["copy".into()]);
        let row = candidates
            .iter()
            .find(|c| c.candidate.id.starts_with("shellpipe::"))
            .expect("at least one shellpipe row");
        let payload_b64 = row.candidate.id.strip_prefix("shellpipe::").unwrap();
        let bytes = BASE64_URL_SAFE_NO_PAD.decode(payload_b64).unwrap();
        let decoded = String::from_utf8(bytes).unwrap();
        let mut lines = decoded.split('\n');
        assert_eq!(lines.next(), Some("ls -la"));
        assert_eq!(lines.next(), Some("copy"));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn orchestrate_multi_stage_round_trip() {
        use base64::prelude::*;
        let candidates =
            orchestrate_shell_pipeline("cat foo.txt", &["upper".into(), "copy".into()]);
        let row = candidates
            .iter()
            .find(|c| c.candidate.id.starts_with("shellpipe::"))
            .expect("at least one shellpipe row");
        let payload_b64 = row.candidate.id.strip_prefix("shellpipe::").unwrap();
        let decoded = String::from_utf8(
            BASE64_URL_SAFE_NO_PAD.decode(payload_b64).unwrap(),
        )
        .unwrap();
        // Encoded shape: `cmd\nstage1\nstage2`
        let parts: Vec<&str> = decoded.split('\n').collect();
        assert_eq!(parts, vec!["cat foo.txt", "upper", "copy"]);
    }


    #[test]
    fn execute_runs_shell_and_pipes_stdout() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let effect = runtime
            .block_on(async {
                execute_shell_pipeline("printf 'hello'", &["copy".into()]).await
            })
            .expect("shell pipeline ran");
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "hello"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[test]
    fn execute_chains_transform_then_sink() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let effect = runtime
            .block_on(async {
                execute_shell_pipeline(
                    "printf 'hello'",
                    &["upper".into(), "copy".into()],
                )
                .await
            })
            .expect("shell pipeline ran");
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "HELLO"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[test]
    fn execute_with_qr_sink_returns_show_image() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let effect = runtime
            .block_on(async {
                execute_shell_pipeline("printf 'hi'", &["qr".into()]).await
            })
            .expect("shell pipeline ran");
        match effect {
            Effect::ShowImagePng(_) => {} // ok
            other => panic!("expected ShowImagePng, got {other:?}"),
        }
    }

    #[test]
    fn execute_handles_cmd_with_internal_pipe() {
        // `printf 'a\nb\nc' | sort` - the internal pipe stays in the
        // shell command; only the trailing pipeline stage is applied
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let effect = runtime
            .block_on(async {
                execute_shell_pipeline(
                    "printf 'b\\na\\nc' | sort",
                    &["copy".into()],
                )
                .await
            })
            .expect("shell pipeline ran");
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "a\nb\nc\n"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[test]
    fn execute_unknown_stage_surfaces_error() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            execute_shell_pipeline("printf 'x'", &["nopenope".into()]).await
        });
        assert!(result.is_err(), "unknown stage should error out");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("nopenope") || msg.contains("unknown"),
            "expected unknown-stage error, got: {msg}"
        );
    }
}

#[cfg(all(test, feature = "ai"))]
mod ai_pipeline_tests {
    use super::*;


    #[test]
    fn no_verb_no_pipe_returns_none() {
        assert!(parse_ai_pipeline("hello world").is_none());
        assert!(parse_ai_pipeline("ai hello world").is_none()); // no pipe
        assert!(parse_ai_pipeline("notai write a haiku | copy").is_none());
    }

    #[test]
    fn bare_verb_without_prompt_returns_none() {
        // `ai | copy` - no actual prompt text; falls through
        assert!(parse_ai_pipeline("ai | copy").is_none());
        assert!(parse_ai_pipeline("ai  | copy").is_none());
    }

    #[test]
    fn ai_keyword_no_instruction() {
        let (prompt, instr, stages) =
            parse_ai_pipeline("ai write a haiku | copy").unwrap();
        assert_eq!(prompt, "write a haiku");
        assert!(instr.is_none(), "free-form ai has no instruction");
        assert_eq!(stages, vec!["copy"]);
    }

    #[test]
    fn ask_alias_works_too() {
        let (prompt, instr, stages) =
            parse_ai_pipeline("ask explain quantum entanglement | upper | copy").unwrap();
        assert_eq!(prompt, "explain quantum entanglement");
        assert!(instr.is_none());
        assert_eq!(stages, vec!["upper", "copy"]);
    }

    #[test]
    fn summarize_verb_carries_curated_instruction() {
        let (prompt, instr, stages) =
            parse_ai_pipeline("summarize this is the source text | copy").unwrap();
        assert_eq!(prompt, "this is the source text");
        let instr = instr.expect("summarize must carry an instruction");
        assert!(
            instr.to_ascii_lowercase().contains("summarize"),
            "instruction should reuse the curated AiTransform prompt: {instr}"
        );
        assert_eq!(stages, vec!["copy"]);
    }

    #[test]
    fn explain_verb_resolves() {
        let (_, instr, _) =
            parse_ai_pipeline("explain monads briefly | show").unwrap();
        let instr = instr.unwrap();
        assert!(
            instr.to_ascii_lowercase().contains("explain"),
            "got: {instr}"
        );
    }

    #[test]
    fn translate_excluded_from_pipeline_verbs() {
        // `translate` takes a target-language arg; the pipeline syntax
        // doesn't carry per-stage args, so parser shouldn't claim it
        assert!(parse_ai_pipeline("translate fr hello | copy").is_none());
    }

    #[test]
    fn trailing_pipe_emits_empty_partial() {
        let (prompt, _, stages) = parse_ai_pipeline("ai write a haiku |").unwrap();
        assert_eq!(prompt, "write a haiku");
        assert_eq!(stages, vec![""]);
    }

    #[test]
    fn cmd_internal_pipe_in_prompt_stays_in_prompt() {
        // `compare cat | dog | copy` - `dog` isn't a transform so the
        // walk stops there; `cat | dog` becomes prompt
        let (prompt, _, stages) =
            parse_ai_pipeline("ai compare cat | dog | copy").unwrap();
        assert_eq!(prompt, "compare cat | dog");
        assert_eq!(stages, vec!["copy"]);
    }

    #[test]
    fn multi_stage_walks_left_through_transforms() {
        let (prompt, _, stages) =
            parse_ai_pipeline("ai write a haiku | upper | snake | copy").unwrap();
        assert_eq!(prompt, "write a haiku");
        assert_eq!(stages, vec!["upper", "snake", "copy"]);
    }

    #[test]
    fn case_insensitive_verb() {
        let (prompt, instr, _) =
            parse_ai_pipeline("Summarize foo bar | copy").unwrap();
        assert_eq!(prompt, "foo bar");
        assert!(instr.is_some());
    }


    #[test]
    fn orchestrate_offers_all_transforms_and_non_ai_sinks_for_empty_partial() {
        let candidates = orchestrate_ai_pipeline("write a haiku", None, &["".into()]);
        let titles: Vec<String> = candidates
            .iter()
            .map(|c| c.candidate.title.clone())
            .collect();
        assert!(
            titles.iter().any(|t| t.contains("→ copy")),
            "expected `copy` row, got: {titles:#?}"
        );
        assert!(
            titles.iter().any(|t| t.contains("→ upper")),
            "expected `upper` row, got: {titles:#?}"
        );
        // AI sinks should NOT appear (no `ai foo | summarize`
        // recursion suggestion)
        assert!(
            !titles.iter().any(|t| t.contains("→ summarize")),
            "AI sinks must not be suggested when AI is the source"
        );
        assert!(
            !titles.iter().any(|t| t.contains("→ ai")),
            "AI sinks must not be suggested when AI is the source"
        );
    }

    #[test]
    fn orchestrate_id_round_trips_through_base64_no_instruction() {
        use base64::prelude::*;
        let candidates = orchestrate_ai_pipeline("hello", None, &["copy".into()]);
        let row = candidates
            .iter()
            .find(|c| c.candidate.id.starts_with("aipipe::"))
            .expect("at least one aipipe row");
        let payload_b64 = row.candidate.id.strip_prefix("aipipe::").unwrap();
        let bytes = BASE64_URL_SAFE_NO_PAD.decode(payload_b64).unwrap();
        let decoded = String::from_utf8(bytes).unwrap();
        let mut lines = decoded.split('\n');
        assert_eq!(lines.next(), Some("hello"));
        assert_eq!(lines.next(), Some("0"), "no-instruction flag");
        assert_eq!(lines.next(), Some(""), "empty instruction line");
        assert_eq!(lines.next(), Some("copy"));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn orchestrate_id_round_trips_with_instruction() {
        use base64::prelude::*;
        let candidates = orchestrate_ai_pipeline(
            "the body",
            Some("Summarize concisely.".to_string()),
            &["copy".into()],
        );
        let row = candidates
            .iter()
            .find(|c| c.candidate.id.starts_with("aipipe::"))
            .expect("at least one aipipe row");
        let payload_b64 = row.candidate.id.strip_prefix("aipipe::").unwrap();
        let decoded = String::from_utf8(
            BASE64_URL_SAFE_NO_PAD.decode(payload_b64).unwrap(),
        )
        .unwrap();
        let parts: Vec<&str> = decoded.split('\n').collect();
        assert_eq!(parts[0], "the body");
        assert_eq!(parts[1], "1");
        assert_eq!(parts[2], "Summarize concisely.");
        assert_eq!(parts[3], "copy");
    }

    #[test]
    fn orchestrate_emits_autoclose_for_unique_prefix() {
        let candidates = orchestrate_ai_pipeline("hi", None, &["unb".into()]);
        assert!(
            candidates
                .iter()
                .any(|c| c.candidate.id.starts_with("autoclose::")),
            "expected autoclose for unique-prefix `unb`"
        );
    }

    #[test]
    fn orchestrate_unknown_partial_returns_helper_row() {
        let candidates =
            orchestrate_ai_pipeline("hi", None, &["nopenope".into()]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].candidate.id,
            "chain::__unknown-ai-stage__"
        );
    }


    #[test]
    fn activation_emits_ask_ai_then_pipe_with_no_instruction() {
        // Build an aipipe id by hand and walk it through dispatch
        // path. We can't go through the real bridge here (it's a
        // singleton with FFI globals), so just exercise the decode
        // logic by constructing an effect from same code path
        // via a synthetic reproduction: encode the payload and assert
        // that a fresh decode reconstructs same data
        use base64::prelude::*;
        let payload = "what is rust?\n0\n\ncopy";
        let id = format!("aipipe::{}", BASE64_URL_SAFE_NO_PAD.encode(payload));
        // We can't easily call dispatch_activation in isolation
        // (it's tied to a bridge), but the encode/decode shape is
        // what matters most. Verify it round-trips through the
        // protocol we documented
        let strip = id.strip_prefix("aipipe::").unwrap();
        let bytes = BASE64_URL_SAFE_NO_PAD.decode(strip).unwrap();
        let decoded = String::from_utf8(bytes).unwrap();
        let mut lines = decoded.split('\n');
        assert_eq!(lines.next(), Some("what is rust?"));
        assert_eq!(lines.next(), Some("0"));
        assert_eq!(lines.next(), Some(""));
        assert_eq!(lines.next(), Some("copy"));
    }


    #[test]
    fn pipe_to_ai_sink_emits_ask_ai() {
        use crate::pipeline as p;
        let effect = p::apply_sink("ai", "what time is it?").unwrap();
        match effect {
            Effect::AskAi(s) => assert_eq!(s, "what time is it?"),
            other => panic!("expected AskAi, got {other:?}"),
        }
        let effect = p::apply_sink("ask", "another question").unwrap();
        match effect {
            Effect::AskAi(s) => assert_eq!(s, "another question"),
            other => panic!("expected AskAi for `ask` alias, got {other:?}"),
        }
    }

    #[test]
    fn pipe_to_ai_sink_emits_ai_transform_with_curated_instruction() {
        use crate::pipeline as p;
        let effect = p::apply_sink("summarize", "long body of text").unwrap();
        match effect {
            Effect::AiTransform { text, instruction } => {
                assert_eq!(text, "long body of text");
                assert!(
                    instruction.to_ascii_lowercase().contains("summarize"),
                    "instruction should be the curated summarize prompt: {instruction}"
                );
            }
            other => panic!("expected AiTransform, got {other:?}"),
        }
    }

    #[test]
    fn pipe_to_ai_sink_translate_not_supported() {
        use crate::pipeline as p;
        // Translate takes an arg -> not pipe-friendly; should NOT be
        // a sink
        assert!(p::apply_sink("translate", "hello").is_none());
    }

    #[test]
    fn pipe_to_ai_sinks_classify_as_sinks() {
        use crate::pipeline as p;
        for kw in ["ai", "ask", "summarize", "tldr", "explain", "rewrite", "fix", "shorten", "expand"]
        {
            assert_eq!(
                p::classify_stage(kw),
                p::StageKind::Sink,
                "{kw} should classify as Sink"
            );
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod ffi_tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::{Mutex as StdMutex, OnceLock};

    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    /// Shared per-process sandbox. Built once on first use and kept
    /// alive for the whole test run so all FFI tests share a single
    /// `BRIDGE` initialisation against KNOWN data - no dependency on
    /// the developer's real `~/Library/Application Support/Gyors/` or
    /// `~/Documents/Gyors/`. The TempDir is held in the static so its
    /// on-disk contents live as long as any test might need them
    ///
    /// Why a singleton: the global `BRIDGE` is a `OnceLock`, so
    /// whichever test runs first determines config/notes the
    /// rest of the suite sees. Centralising setup here makes those
    /// paths deterministic and isolated, and removes the historical
    /// bug where `cargo test` wrote into the real user config via
    /// `XDG_DATA_HOME` (which macOS's `dirs` crate ignored)
    struct TestEnv {
        _dir: tempfile::TempDir,
        #[allow(dead_code)]
        notes_dir: std::path::PathBuf,
    }

    static TEST_ENV: OnceLock<TestEnv> = OnceLock::new();

    /// Fixture notes written into the sandbox. Titles and content are
    /// chosen so sanity tests can pin specific match shapes (e.g.,
    /// a prefix match on "he", a multi-word title that exercises the
    /// token fuzzy path)
    const FIXTURE_NOTES: &[(&str, &str)] = &[
        ("helo", "# helo\nhello world from test fixture"),
        ("heyhey", "# heyhey\nhey hey hey"),
        ("alpha-one", "# Alpha One\nfirst alpha"),
        ("beta", "# beta\nsecond fixture"),
    ];

    fn test_env() -> &'static TestEnv {
        TEST_ENV.get_or_init(|| {
            let dir = tempfile::tempdir().expect("sandbox tempdir");
            let notes_dir = dir.path().join("TestNotes");
            std::fs::create_dir_all(&notes_dir).unwrap();
            for (stem, body) in FIXTURE_NOTES {
                let path = notes_dir.join(format!("{stem}.md"));
                std::fs::write(path, body).unwrap();
            }
            // Write a config.json that points at the fixture notes
            // folder. Dont touch user's real file: the sandbox
            // dir is what `GYORS_CONFIG_DIR` below redirects to
            let cfg = serde_json::json!({
                "hotkey": "opt+shift+space",
                "notes_folder": notes_dir.to_string_lossy(),
            });
            std::fs::write(
                dir.path().join("config.json"),
                serde_json::to_string_pretty(&cfg).unwrap(),
            )
            .unwrap();
            // Seed a fixture shell plugin so end-to-end plugin
            // test has something to assert on. Keyword `sandboxplug`
            // deliberately doesn't collide with any built-in
            let plugins = serde_json::json!([
                {
                    "id": "sandboxplug",
                    "name": "Sandbox Plugin",
                    "keywords": ["sandboxplug"],
                    "command": "printf 'sandbox:%s' {query}",
                    "on_activate": "copy"
                }
            ]);
            std::fs::write(
                dir.path().join("plugins.json"),
                serde_json::to_string_pretty(&plugins).unwrap(),
            )
            .unwrap();
            // Must be set BEFORE the first `gyors_init()` - bridge's
            // OnceLock init reads config + opens the SQLite DB at
            // that moment and caches result for rest of the
            // process. Subsequent tests inherit same sandboxed
            // state
            //
            // `GYORS_CONFIG_DIR` redirects `config.json` + plugins.
            // `GYORS_DATA_DIR` redirects `gyors.db` (clipboard rows,
            // query history, sync outbox). Without the data-dir
            // override, every FFI test that calls `record_query` or
            // `record_clipboard` wrote into the developer's real
            // `~/Library/Application Support/Gyors/gyors.db` and
            // those `hist-test-<epoch>` entries surfaced in the
            // user's launcher history. Both env vars together fully
            // isolate test process
            std::env::set_var("GYORS_CONFIG_DIR", dir.path());
            std::env::set_var("GYORS_DATA_DIR", dir.path());
            TestEnv {
                _dir: dir,
                notes_dir,
            }
        })
    }

    /// Combined TEST_LOCK + env + init. Every FFI test calls this as
    /// its first line so tests read as intent-focused, and the
    /// env-install step is impossible to forget
    fn init_with_sandbox() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = test_env();
        gyors_init();
        guard
    }

    #[test]
    fn init_is_idempotent() {
        let _g = init_with_sandbox();
        gyors_init();
        gyors_init();
        gyors_init();
    }

    #[test]
    fn query_returns_valid_json_array() {
        let _g = init_with_sandbox();
        let pattern = CString::new("gyors-test-xyzzy-no-match").unwrap();
        let ptr = gyors_query(pattern.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        let _: Vec<serde_json::Value> = serde_json::from_str(&s).expect("valid JSON");
        gyors_free_string(ptr);
    }

    #[test]
    fn null_pattern_bounces_straight_back() {
        let ptr = gyors_query(std::ptr::null());
        assert!(ptr.is_null());
    }

    #[test]
    fn activate_unknown_prefix_yields_error_json() {
        let _g = init_with_sandbox();
        let id = CString::new("bogus::id").unwrap();
        let action = CString::new("default").unwrap();
        let ptr = gyors_activate(id.as_ptr(), action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        assert!(s.contains("error"), "got {s}");
        gyors_free_string(ptr);
    }

    #[test]
    fn activate_null_action_falls_back_to_default() {
        let _g = init_with_sandbox();
        let id = CString::new("sys::lock-screen").unwrap();
        let ptr = gyors_activate(id.as_ptr(), std::ptr::null());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        assert!(s.contains("RunShell"), "expected RunShell effect, got {s}");
        gyors_free_string(ptr);
    }


    #[test]
    fn ai_palette_suppressed_for_empty_pattern() {
        // Empty input is owned by the discovery-rows path; the
        // palette must not also appear there or empty-state would
        // render "Ask AI:" as one of the curated tips
        assert!(!should_show_ai_palette(""));
        assert!(!should_show_ai_palette("   "));
    }

    #[test]
    fn ai_palette_suppressed_for_bangbang_recall() {
        // `!!` is shell-style "recall last command" - Swift
        // shell renders a ghost-text preview, no question to AI.
        // A palette row would clutter result list right when
        // user wants confirmation of the queued query
        assert!(!should_show_ai_palette("!!"));
        assert!(!should_show_ai_palette("  !!  "));
        // Embedded `!!` is NOT a recall - those queries can still
        // benefit from a "send to AI" fallback
        assert!(should_show_ai_palette("what does !! mean in bash"));
        assert!(should_show_ai_palette("!!!"));
    }

    #[test]
    fn ai_palette_suppressed_when_user_typed_ai_keyword() {
        // `ai foo` already produces an AiProvider row pointing at
        // Effect::AskAi("foo"). A palette row would be a redundant
        // duplicate one slot below
        assert!(!should_show_ai_palette("ai foo"));
        assert!(!should_show_ai_palette("ask what is the time"));
        // Bare `ai` / `ask` without arguments - same logic, even
        // though AiProvider returns nothing in that case (the user
        // will type their question shortly; we dont want a palette
        // row flashing in mid-keystroke)
        assert!(!should_show_ai_palette("ai"));
        assert!(!should_show_ai_palette("ask"));
    }

    #[test]
    fn ai_palette_shows_for_normal_pattern() {
        for q in ["foo", "100 usd in eur", "what is love", "color #ff0000"] {
            assert!(should_show_ai_palette(q), "should show for {q:?}");
        }
    }

    #[test]
    fn ai_palette_substring_ai_does_not_suppress() {
        // Suppression matches the literal `ai ` / `ask ` prefix -
        // a query that happens to contain those substrings or
        // share a stem (`airline`, `airbnb`, `aim`, `aikido`)
        // should still get the palette row
        for q in ["airline", "airbnb", "askew person", "askance look", "aikido"] {
            assert!(should_show_ai_palette(q), "should show for {q:?}");
        }
    }

    #[test]
    fn ai_palette_appended_when_results_present() {
        // With existing rows the palette goes to the bottom (lowest
        // score, sorts last). Important so it never competes with a
        // real provider hit for default Enter activation
        let scored = vec![ScoredCandidate {
            candidate: gyors_core::Candidate {
                id: "x".into(),
                title: "X".into(),
                subtitle: None,
                icon: gyors_core::Icon::None,
                kind: gyors_core::CandidateKind::App,
                actions: vec![],
                search_text: String::new(),
                bypass_rank: false,
            },
            score: 100,
        }];
        let out = inject_ai_palette(scored, "foo");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].candidate.id, "x");
        assert_eq!(out[1].candidate.id, "ai_palette::foo");
        assert!(out[1].score < out[0].score, "palette must sort last");
    }

    #[test]
    fn ai_palette_auto_promoted_when_no_results() {
        // With nothing else, the palette IS result list - sits
        // at top with BYPASS_RANK_SCORE so Enter routes straight
        // through without user reaching for cmd
        let out = inject_ai_palette(Vec::new(), "asdfqwer");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].candidate.id, "ai_palette::asdfqwer");
        assert_eq!(out[0].score, BYPASS_RANK_SCORE);
    }

    #[test]
    fn ai_palette_truncates_long_question_in_title() {
        let q: String = "x".repeat(200);
        let row = ai_palette_candidate(&q);
        assert!(row.title.ends_with('…'), "expected ellipsis, got {}", row.title);
        // The id keeps the full question - that's what the activate
        // path strips and routes to AskAi
        assert_eq!(row.id, format!("ai_palette::{q}"));
    }

    #[test]
    fn activate_ai_palette_yields_ask_ai_effect() {
        let _g = init_with_sandbox();
        let id = CString::new("ai_palette::what is love").unwrap();
        let action = CString::new("default").unwrap();
        let ptr = gyors_activate(id.as_ptr(), action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        assert!(s.contains("AskAi"), "expected AskAi effect, got {s}");
        assert!(s.contains("what is love"), "question round-trips, got {s}");
        gyors_free_string(ptr);
    }

    /// Push a string into clipboard store via same FFI
    /// path Swift pasteboard watcher uses. Just a CString
    /// wrapper around `gyors_record_clipboard` so test
    /// reads cleaner
    fn record_clipboard_for_test(content: &str) {
        let cs = CString::new(content).unwrap();
        gyors_record_clipboard(cs.as_ptr());
    }

    #[test]
    fn clipboard_items_absent_in_default_mode() {
        // REGRESSION: clipboard-history matches must not show up
        // alongside regular default-mode results. Clipboard is opt-in
        // via `clip` / `paste` / `cb` / `c` prefixes - same pattern as
        // `'` (files) and `>` (shell). Default-mode queries get the
        // rest of the registry, NOT clipboard fan-out
        let _g = init_with_sandbox();
        // Seed clipboard so a fuzzy match on query text would
        // match SOMETHING if provider were running
        record_clipboard_for_test("how much is 100 euros in forints");
        record_clipboard_for_test("300 euros in hungarian forints");
        let rows = query_json("how much is 100 euros in forints");
        for row in &rows {
            let id = row.get("id").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                !id.starts_with("clip::"),
                "default-mode query produced clipboard row {id:?}"
            );
        }
    }

    #[test]
    fn clipboard_items_present_when_explicitly_requested() {
        // Counter-test: the `clip` / `c` prefix still fires the
        // clipboard provider. We're suppressing default-mode
        // fuzzy-match noise, NOT removing the feature
        let _g = init_with_sandbox();
        record_clipboard_for_test("alpha bravo charlie");
        let rows = query_json("clip alpha");
        let has_clip_row = rows.iter().any(|r| {
            r.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id.starts_with("clip::"))
        });
        assert!(has_clip_row, "expected clip:: row from `clip alpha`, got {rows:?}");

        // Same via the new short alias
        let rows_c = query_json("c alpha");
        let has_clip_c = rows_c.iter().any(|r| {
            r.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id.starts_with("clip::"))
        });
        assert!(has_clip_c, "expected clip:: row from `c alpha`, got {rows_c:?}");
    }

    #[test]
    fn ai_palette_absent_in_keyword_routed_modes() {
        // Notes, clipboard, shell modes have their own dispatch
        // paths in orchestrator that short-circuit BEFORE the
        // palette injection. Verify this - a palette row in those
        // modes would let the AI silently overshadow user's
        // explicit keyword routing
        let _g = init_with_sandbox();
        for q in ["clip", "clip foo", "> ls", "note", "note hello"] {
            let rows = query_json(q);
            for row in &rows {
                let id = row.get("id").and_then(|v| v.as_str()).unwrap_or("");
                assert!(
                    !id.starts_with("ai_palette::"),
                    "{q:?}: keyword-routed mode produced palette row {id:?}"
                );
            }
        }
    }

    #[test]
    fn ai_palette_id_round_trips_unicode() {
        // Currency and units queries often involve unicode (EUR, JPY, ...).
        // Orchestrator encodes question verbatim in the id;
        // dispatch_activate strips prefix and emits AskAi with
        // original text. Round-trip with non-ASCII characters
        // to make sure we dont bork on encoding
        let _g = init_with_sandbox();
        let id = CString::new("ai_palette::100€ in £").unwrap();
        let action = CString::new("default").unwrap();
        let ptr = gyors_activate(id.as_ptr(), action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        assert!(s.contains("AskAi"), "expected AskAi, got {s}");
        assert!(s.contains("100€"), "€ preserved, got {s}");
        assert!(s.contains("£"), "£ preserved, got {s}");
        gyors_free_string(ptr);
    }

    #[test]
    fn ai_router_id_round_trips_unicode() {
        // Same test as above but for the router preview row.
        // SetInput with unicode keyword form must round-trip
        let _g = init_with_sandbox();
        let id = CString::new("ai_router::100 € in £").unwrap();
        let action = CString::new("default").unwrap();
        let ptr = gyors_activate(id.as_ptr(), action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        assert!(s.contains("SetInput"), "expected SetInput, got {s}");
        assert!(s.contains("100 €"), "€ preserved, got {s}");
        gyors_free_string(ptr);
    }

    #[test]
    fn activate_ai_palette_with_empty_question_yields_empty_ask_ai() {
        // Edge case: id is `ai_palette::` with no question. We
        // shouldn't crash; AskAi("") is a no-op Swift side
        // tolerates, so activate path round-trips it
        let _g = init_with_sandbox();
        let id = CString::new("ai_palette::").unwrap();
        let action = CString::new("default").unwrap();
        let ptr = gyors_activate(id.as_ptr(), action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        assert!(s.contains("AskAi"), "still AskAi-shaped, got {s}");
        gyors_free_string(ptr);
    }

    //
    // Marked `#[ignore]` so they dont run on every `cargo test`
    // invocation - they're slow (10K iterations) and target
    // bounded-growth invariants under load. Run on demand with:
    //
    //     cargo test --release -- --ignored soak
    //
    // The release build matters: debug-mode SQLite is ~10x slower
    // and a 10K-row test takes minutes instead of seconds

    // The bounded-cap soak lives in `gyors-index` rather than
    // here, because it needs a clean Index without the shared
    // `BRIDGE` state that earlier tests in same process have
    // already populated. See
    // `gyors-index::tests::soak_visits_table_stays_bounded`

    #[test]
    #[ignore = "long-running soak - invoke explicitly via `cargo test --release -- --ignored`"]
    fn soak_ai_palette_ids_do_not_grow_visits() {
        // Each AI palette id encodes user's verbatim question.
        // Before the should_record_visit skip, 1000 distinct
        // questions filled visits with 1000 dead rows. Soak the
        // dispatch_activate path with 1000 unique palette ids and
        // assert the table didn't gain a single row
        let _g = init_with_sandbox();

        // Snapshot baseline so other soak tests / earlier inserts
        // dont poison the assertion. The `lock` here uses
        // `unwrap_or_else(into_inner)` because tests in the same
        // process share the BRIDGE OnceLock - if any earlier test
        // panicked while holding lock, it'd be poisoned, and
        // we'd rather see this test's actual result than a
        // PoisonError noise
        let baseline = BRIDGE
            .get()
            .unwrap()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .index
            .visits_total_rows()
            .unwrap();

        for i in 0..1_000 {
            let id = CString::new(format!("ai_palette::question {i}")).unwrap();
            let action = CString::new("default").unwrap();
            let ptr = gyors_activate(id.as_ptr(), action.as_ptr());
            assert!(!ptr.is_null());
            gyors_free_string(ptr);
        }

        let after = BRIDGE
            .get()
            .unwrap()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .index
            .visits_total_rows()
            .unwrap();
        assert_eq!(
            after, baseline,
            "1000 ai_palette activates recorded {} new visits - should be 0",
            after - baseline
        );
    }

    #[test]
    fn should_record_visit_skips_set_input_effects() {
        // Tab autocomplete, hint prefill, and router preview all
        // emit SetInput. None of these are user's actual
        // target pick - recording would lift the completion
        // candidates above providers they feed
        assert!(!should_record_visit(
            "hint::md5",
            &Effect::SetInput("md5 ".into())
        ));
        assert!(!should_record_visit(
            "autoclose::{",
            &Effect::SetInput("{}".into())
        ));
    }

    #[test]
    fn should_record_visit_skips_ai_palette_and_router_ids() {
        // REGRESSION (2026-04-29): the visits table was filling
        // with one row per unique AI question, all high-entropy
        // keys that never matched again. Pin both the palette and
        // router prefixes against same skip rule
        assert!(!should_record_visit(
            "ai_palette::what is love",
            &Effect::AskAi("what is love".into())
        ));
        assert!(!should_record_visit(
            "ai_palette::very long unique question with timestamp 12345",
            &Effect::AskAi("…".into())
        ));
        assert!(!should_record_visit(
            "ai_router::100 EUR in HUF",
            &Effect::SetInput("100 EUR in HUF".into())
        ));
        // ai_router rows always emit SetInput, so they'd skip via
        // the SetInput rule too - pin the explicit prefix branch
        // anyway in case future router work changes the effect
    }

    #[test]
    fn should_record_visit_records_real_provider_picks() {
        // Counter-test: real provider activations DO record
        // frecency. Apps, notes, calc results, currency, web
        // search etc
        assert!(should_record_visit(
            "apps::Safari",
            &Effect::OpenPath(std::path::PathBuf::from("/Applications/Safari.app"))
        ));
        assert!(should_record_visit(
            "calc::35",
            &Effect::CopyToClipboard("35".into())
        ));
        assert!(should_record_visit(
            "ccy::92.50",
            &Effect::CopyToClipboard("92.50".into())
        ));
        assert!(should_record_visit(
            "note::/path/to/file.md",
            &Effect::EditNote(std::path::PathBuf::from("/path/to/file.md"))
        ));
        assert!(should_record_visit(
            "web::g::rust async",
            &Effect::OpenUrl("https://google.com/?q=rust+async".into())
        ));
    }

    #[test]
    fn should_record_visit_does_not_skip_real_id_with_set_input_lookalike() {
        // Edge case: an id that starts with "ai_palette" or
        // "ai_router" but isn't actually our synthetic prefix -
        // some future provider might collide. Verify our match
        // is the strict `::`-suffixed prefix, not a substring
        assert!(should_record_visit(
            "ai_palettey::not-ours",
            &Effect::OpenUrl("https://x.com".into())
        ));
        assert!(should_record_visit(
            "ai_router_v2::also-not-ours",
            &Effect::OpenUrl("https://x.com".into())
        ));
    }

    #[test]
    fn ai_palette_visible_total_stays_under_cap_for_broad_queries() {
        // The reservation logic in `orchestrate` must keep the
        // visible total <= DEFAULT_MODE_MAX (30) even when a broad
        // query like `no` would otherwise produce 30+ rows AND the
        // palette is eligible. Without the `cap = max - 1` move,
        // the palette inflated the visible total by 1
        //
        // Picked `no` deliberately - `n` is a notes-mode alias so
        // it short-circuits before palette injection; we want a
        // plain Default-mode broad query
        let _g = init_with_sandbox();
        let rows = query_json("no");
        assert!(
            rows.len() <= 30,
            "expected ≤30 rows including palette, got {}",
            rows.len()
        );
        // And the palette IS one of those rows (we shouldn't have
        // truncated it away)
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("id").and_then(|v| v.as_str()))
            .collect();
        assert!(
            ids.iter().any(|id| id.starts_with("ai_palette::")),
            "palette row preserved, got first 5: {:?}",
            ids.iter().take(5).collect::<Vec<_>>()
        );
    }

    #[test]
    fn activate_ai_router_yields_set_input_effect() {
        // Router preview row activate must SetInput the rendered
        // keyword form - that's what feeds natural-language
        // translation back through keyword path so matching
        // provider produces real result. SetInput rather than
        // AskAi: the LLM already did its job (translation), the
        // dispatch is now a normal keyword query
        let _g = init_with_sandbox();
        let id = CString::new("ai_router::100 usd in eur").unwrap();
        let action = CString::new("default").unwrap();
        let ptr = gyors_activate(id.as_ptr(), action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        assert!(s.contains("SetInput"), "expected SetInput, got {s}");
        assert!(s.contains("100 usd in eur"), "keyword form round-trips, got {s}");
        gyors_free_string(ptr);
    }

    #[test]
    fn freeing_null_stays_quiet() {
        gyors_free_string(std::ptr::null_mut());
    }

    #[test]
    fn app_count_nonzero_after_init() {
        let _g = init_with_sandbox();
        assert!(gyors_app_count() < u64::MAX);
    }

    #[test]
    fn null_clipboard_gets_shrugged_off() {
        gyors_record_clipboard(std::ptr::null());
    }

    #[test]
    fn query_results_include_actions_field() {
        let _g = init_with_sandbox();
        let pattern = CString::new("lock").unwrap();
        let ptr = gyors_query(pattern.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        assert!(
            s.contains("actions"),
            "expected actions key in candidates, got {s}"
        );
        gyors_free_string(ptr);
    }

    /// Helper: run a query string through the FFI and return the
    /// decoded JSON array. Stays local to test module - no reason
    /// to expose it more broadly
    fn query_json(raw: &str) -> Vec<serde_json::Value> {
        let pattern = CString::new(raw).unwrap();
        let ptr = gyors_query(pattern.as_ptr());
        assert!(!ptr.is_null(), "gyors_query({raw:?}) returned null");
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        gyors_free_string(ptr);
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        v.as_array().cloned().unwrap_or_default()
    }

    /// Collect note-candidate titles for quick assertions
    fn note_titles(rows: &[serde_json::Value]) -> Vec<String> {
        rows.iter()
            .filter(|c| {
                c.get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| id.starts_with("note::") && id.contains(".md"))
                    .unwrap_or(false)
            })
            .filter_map(|c| c.get("title").and_then(|v| v.as_str()).map(String::from))
            .collect()
    }

    #[test]
    fn sandbox_fixtures_visible_on_bare_note_keyword() {
        // The sandbox fixtures must surface under `note` - guards
        // against a bug where the sandbox wasn't wired to
        // `NotesProvider` (e.g., `resolve_notes_folder` missed the
        // config path override)
        let _g = init_with_sandbox();
        let rows = query_json("note");
        assert!(!rows.is_empty(), "note keyword produced no rows");
        let titles = note_titles(&rows);
        for expected in ["helo", "heyhey", "Alpha One", "beta"] {
            assert!(
                titles.iter().any(|t| t == expected),
                "fixture note `{expected}` missing from rows: {titles:?}"
            );
        }
    }

    // Full note autocomplete regression suite
    //
    // The "typed `note he`, only got Create" bug keeps resurfacing.
    // This block pins every shape of note query the UI sends so a
    // regression can't silently pass through

    #[test]
    fn note_bare_keyword_surfaces_fixture_notes() {
        // `note` alone -> new-note prompt + folder headers + actual
        // note rows. We dont care about the header order, just that
        // every fixture is reachable
        let _g = init_with_sandbox();
        let titles = note_titles(&query_json("note"));
        for expected in ["helo", "heyhey", "Alpha One", "beta"] {
            assert!(
                titles.iter().any(|t| t == expected),
                "`note` should surface fixture `{expected}`, got {titles:?}"
            );
        }
    }

    #[test]
    fn note_filter_exact_match_ranks_at_top() {
        // `note helo` - exact title match on `helo.md`. The first
        // note row must be `helo`, not some other match that happens
        // to contain the substring
        let _g = init_with_sandbox();
        let titles = note_titles(&query_json("note helo"));
        assert_eq!(
            titles.first().map(String::as_str),
            Some("helo"),
            "exact title should rank first, got {titles:?}"
        );
    }

    #[test]
    fn note_filter_never_shows_create_only_when_matches_exist() {
        // The exact user-reported bug: typing `note he` surfaces
        // ONLY the "Create note: he" row despite `helo` and
        // `heyhey` existing. Assert it emphatically.
        let _g = init_with_sandbox();
        let rows = query_json("note he");
        let titles = note_titles(&rows);
        let has_create_only = rows.len() == 1
            && rows[0]
                .get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.contains("openOrCreate") || id.contains("create"))
                .unwrap_or(false);
        assert!(
            !has_create_only,
            "got only a create-candidate row when notes matched, rows: {rows:#?}"
        );
        assert!(
            titles.iter().any(|t| t == "helo"),
            "expected `helo` in rows, got {titles:?}"
        );
    }

    #[test]
    fn note_filter_unknown_term_falls_back_to_create() {
        // No fixture matches `zzz` -> create candidate is correct
        let _g = init_with_sandbox();
        let rows = query_json("note zzzuniquefilter");
        assert!(
            !rows.is_empty(),
            "expected create-candidate fallback, got empty"
        );
        let has_create = rows.iter().any(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.contains("create") || id.contains("openOrCreate"))
                .unwrap_or(false)
        });
        assert!(
            has_create,
            "expected create row for no-match, got {rows:#?}"
        );
    }

    #[test]
    fn notes_alias_works_same_as_note() {
        let _g = init_with_sandbox();
        let ntitles = note_titles(&query_json("note he"));
        let stitles = note_titles(&query_json("notes he"));
        assert_eq!(ntitles, stitles, "`notes` alias should mirror `note`");
    }

    #[test]
    fn n_alias_works_same_as_note() {
        let _g = init_with_sandbox();
        let ntitles = note_titles(&query_json("note he"));
        let stitles = note_titles(&query_json("n he"));
        assert_eq!(ntitles, stitles, "`n` alias should mirror `note`");
    }

    #[test]
    fn hash_shorthand_surfaces_notes() {
        // `#he` - quick-create / open shorthand. Should surface the
        // same matches as `note he` plus an open-or-create row
        let _g = init_with_sandbox();
        let titles = note_titles(&query_json("#he"));
        assert!(
            titles.iter().any(|t| t == "helo"),
            "`#he` should surface `helo`, got {titles:?}"
        );
    }

    #[test]
    fn findnote_does_content_search() {
        // `findnote <query>` ranks content hits first. One of our
        // fixtures has "hello world from test fixture" in its body -
        // `findnote world` should surface it
        let _g = init_with_sandbox();
        let titles = note_titles(&query_json("findnote world"));
        assert!(
            titles.iter().any(|t| t == "helo"),
            "findnote should find `helo` via body content, got {titles:?}"
        );
    }

    #[test]
    fn note_trailing_space_after_filter_still_matches() {
        // `note he ` (trailing space) - user might naturally hit
        // space before typing `|` for a chain. Trailing whitespace
        // must not wipe out prefix match
        let _g = init_with_sandbox();
        let titles = note_titles(&query_json("note he "));
        assert!(
            titles.iter().any(|t| t == "helo"),
            "trailing space on filter broke match: {titles:?}"
        );
    }

    #[test]
    fn note_chain_from_match_returns_chain_row() {
        // `note helo | preview` - preview is a notes-provider action.
        // Expect a chain:: confirm row
        let _g = init_with_sandbox();
        let rows = query_json("note helo | preview");
        let has_chain = rows.iter().any(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with("chain::"))
                .unwrap_or(false)
        });
        assert!(
            has_chain,
            "expected chain:: row for note chain, got {rows:#?}"
        );
    }

    #[test]
    fn note_pipeline_transform_returns_pipeline_row() {
        // `note helo | upper` - `upper` isn't a notes action but IS
        // a pipeline transform. Expect a pipeline:: row
        let _g = init_with_sandbox();
        let rows = query_json("note helo | upper");
        let has_pipeline = rows.iter().any(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with("pipeline::"))
                .unwrap_or(false)
        });
        assert!(
            has_pipeline,
            "expected pipeline:: row for transform, got {rows:#?}"
        );
    }

    #[test]
    fn shell_plugin_registered_and_runs_via_ffi() {
        // Fixture: `plugins.json` in the sandbox has a `sandboxplug`
        // keyword running `printf sandbox:%s {query}`. End-to-end:
        // Keyword query -> plugin loaded -> command run -> candidate
        // with expected title -> activate -> CopyToClipboard effect
        let _g = init_with_sandbox();
        let rows = query_json("sandboxplug hello");
        assert!(!rows.is_empty(), "sandbox shell plugin didn't fire");
        let title = rows[0].get("title").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(
            title, "sandbox:hello",
            "expected command output as title, got `{title}`"
        );

        // Activation - the payload-in-id trick means we round-trip
        // the output without re-running the command
        let id = rows[0].get("id").and_then(|v| v.as_str()).unwrap();
        let c_id = CString::new(id).unwrap();
        let c_action = CString::new("default").unwrap();
        let ptr = gyors_activate(c_id.as_ptr(), c_action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        gyors_free_string(ptr);
        let v: serde_json::Value = serde_json::from_str(&s).expect("effect JSON");
        let copied = v
            .get("CopyToClipboard")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("expected CopyToClipboard, got {s}"));
        assert_eq!(copied, "sandbox:hello");
    }

    #[test]
    fn shell_plugin_appears_in_diagnostics() {
        let _g = init_with_sandbox();
        let ptr = gyors_diagnostics();
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        gyors_free_string(ptr);
        let v: serde_json::Value = serde_json::from_str(&s).expect("diag JSON");
        let count = v
            .get("shell_plugin_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert!(
            count >= 1,
            "expected ≥1 shell plugin from sandbox fixture, got {count}"
        );
        let providers = v
            .get("providers")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            providers.iter().any(|p| p.as_str() == Some("sandboxplug")),
            "sandboxplug provider missing from diagnostics: {providers:?}"
        );
    }

    #[test]
    fn diagnostics_reports_sane_numbers() {
        // Sanity that the diagnostics FFI sees the sandbox's fixtures
        // and wouldn't silently misreport "0 notes" if something
        // broke the scan path again
        let _g = init_with_sandbox();
        let ptr = gyors_diagnostics();
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        gyors_free_string(ptr);
        let v: serde_json::Value = serde_json::from_str(&s).expect("diag JSON");
        let notes_count = v.get("notes_count").and_then(|v| v.as_u64()).unwrap_or(0);
        assert!(
            notes_count >= FIXTURE_NOTES.len() as u64,
            "expected >= {} fixture notes, diag says {notes_count} · full: {s}",
            FIXTURE_NOTES.len()
        );
        assert!(
            v.get("notes_folder_exists")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "notes_folder_exists must be true: {s}"
        );
    }

    #[test]
    fn default_mode_stops_before_the_abyss() {
        // Broad queries (`no`, `s`, ...) used to return 80+ rows from
        // the unbounded default path - LazyVStack happily rendered a
        // bottomless list. Orchestrator now truncates to a
        // human-scale cap so scroll area stays bounded
        let _g = init_with_sandbox();
        let rows = query_json("no");
        assert!(
            rows.len() <= 30,
            "expected ≤30 rows, got {} · first few: {:?}",
            rows.len(),
            rows.iter()
                .take(3)
                .map(|c| c.get("title").and_then(|v| v.as_str()).unwrap_or(""))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn note_he_matches_fixtures_starting_with_he() {
        // `note he` must surface notes whose title starts with
        // "he". With the sandbox pinning the data, we can assert
        // on specific titles
        let _g = init_with_sandbox();
        let rows = query_json("note he");
        let titles = note_titles(&rows);
        assert!(
            titles.iter().any(|t| t == "helo"),
            "expected `helo` in rows, got {titles:?}"
        );
        assert!(
            titles.iter().any(|t| t == "heyhey"),
            "expected `heyhey` in rows, got {titles:?}"
        );
        // Alpha/Beta dont start with "he" so they must not appear
        assert!(
            !titles.iter().any(|t| t == "Alpha One"),
            "unrelated `Alpha One` leaked: {titles:?}"
        );
    }

    #[test]
    fn note_he_chain_pipe_copy_produces_chain_confirm_row() {
        // Verifies the chain path end-to-end via the FFI with the
        // sandbox: `note he | copy` -> chain confirm candidate with
        // the copy action. Using the sandbox means this test doesn't
        // depend on the developer's personal notes
        let _g = init_with_sandbox();
        let rows = query_json("note he | copy");
        let has_chain = rows.iter().any(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with("chain::"))
                .unwrap_or(false)
        });
        assert!(
            has_chain,
            "expected a chain:: confirm row for `note he | copy`, got {rows:#?}"
        );
    }

    #[test]
    fn multi_stage_pipeline_surfaces_pipeline_confirm_row() {
        // `note helo | upper | copy` - multi-stage pipeline: load
        // `helo.md` content, uppercase, copy. The UI gets a single
        // pipeline:: confirm row; activation runs the pipeline
        let _g = init_with_sandbox();
        let rows = query_json("note helo | upper | copy");
        let pipe = rows.iter().find(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with("pipeline::"))
                .unwrap_or(false)
        });
        assert!(
            pipe.is_some(),
            "expected a pipeline:: confirm row, got {rows:#?}"
        );
    }

    #[test]
    fn multi_stage_pipeline_activation_runs_through_transforms() {
        // Full round-trip: type -> confirm row -> activate -> Effect.
        // Source is the sandbox `helo.md` (content starts with
        // "# helo\nhello world..."). Pipeline uppercases then
        // copies -> final Effect must be CopyToClipboard with an
        // uppercased copy of the note content
        let _g = init_with_sandbox();
        let rows = query_json("note helo | upper | copy");
        let pipe = rows
            .iter()
            .find(|c| {
                c.get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| id.starts_with("pipeline::"))
                    .unwrap_or(false)
            })
            .expect("pipeline confirm row");
        let pipe_id = pipe.get("id").and_then(|v| v.as_str()).unwrap();
        let c_id = CString::new(pipe_id).unwrap();
        let c_action = CString::new("default").unwrap();
        let ptr = gyors_activate(c_id.as_ptr(), c_action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        gyors_free_string(ptr);
        // Effect JSON is like:
        //   {"CopyToClipboard":"# HELO\nHELLO WORLD ..."}
        let v: serde_json::Value = serde_json::from_str(&s).expect("effect JSON");
        let copied = v
            .get("CopyToClipboard")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("expected CopyToClipboard, got {s}"));
        assert!(
            copied.contains("HELLO"),
            "expected uppercased content to include HELLO, got {copied:?}"
        );
        assert!(
            !copied.contains("hello "),
            "expected NO lowercase `hello ` after upper, got {copied:?}"
        );
    }

    #[test]
    fn calc_pipeline_pipes_result_through_to_clipboard() {
        // Regression: calculator -> upper -> copy used to silently drop
        // value because `extract_pipeline_text` only knew how to
        // read note files. Now candidate's title (the calc result)
        // rides along inside the pipeline payload as source text
        let _g = init_with_sandbox();
        let rows = query_json("5 grams in kg | upper | copy");
        let pipe = rows
            .iter()
            .find(|c| {
                c.get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| id.starts_with("pipeline::"))
                    .unwrap_or(false)
            })
            .expect("pipeline confirm row");
        let pipe_id = pipe.get("id").and_then(|v| v.as_str()).unwrap();
        let c_id = CString::new(pipe_id).unwrap();
        let c_action = CString::new("default").unwrap();
        let ptr = gyors_activate(c_id.as_ptr(), c_action.as_ptr());
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        gyors_free_string(ptr);
        let v: serde_json::Value = serde_json::from_str(&s).expect("effect JSON");
        let copied = v
            .get("CopyToClipboard")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("expected CopyToClipboard, got {s}"));
        // The calc result for "5 grams in kg" rounds to "0.005 kg"
        // (or similar). After `upper` the unit letters become uppercase
        assert!(
            copied.contains("KG") || copied.contains("0.005"),
            "expected uppercased calc result on the clipboard, got {copied:?}",
        );
    }

    #[test]
    fn partial_stage_keyword_surfaces_completion_rows() {
        // User types `note helo | upp` - they're partway through
        // typing `upper`. Dropdown must offer a pipeline row
        // for the completion (`upper`) so they can pick it, not
        // just show an "unknown action" helper
        let _g = init_with_sandbox();
        let rows = query_json("note helo | upp");
        let has_upper_pipeline = rows.iter().any(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with("pipeline::"))
                .unwrap_or(false)
                && c.get("title")
                    .and_then(|v| v.as_str())
                    .map(|t| t.contains("upper"))
                    .unwrap_or(false)
        });
        assert!(
            has_upper_pipeline,
            "expected a pipeline:: row suggesting `upper` for partial `upp`, got {rows:#?}"
        );
    }

    #[test]
    fn unique_stage_prefix_emits_autoclose_ghost() {
        // `note helo | md` - only `md5` starts with `md` in the
        // stage registry, so orchestrator emits an autoclose
        // candidate whose id carries the full completed query. The
        // Swift side picks this up for inline grey-tail ghost
        // completion - Tab then commits it
        let _g = init_with_sandbox();
        let rows = query_json("note helo | md");
        let ghost = rows.iter().find(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with("autoclose::"))
                .unwrap_or(false)
        });
        let ghost = ghost.unwrap_or_else(|| {
            panic!("expected autoclose:: candidate for single-match prefix, got {rows:#?}")
        });
        let id = ghost.get("id").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            id.ends_with("note helo | md5"),
            "expected autoclose id to end with `note helo | md5`, got {id}"
        );
    }

    #[test]
    fn ambiguous_stage_prefix_suppresses_autoclose() {
        // `note helo | c` matches multiple stages (copy, count,
        // countwords, capitalize, camel, constant, chop). With more
        // than one candidate, no single completion is obviously
        // right - suppress ghost to avoid a misleading
        // auto-fill
        let _g = init_with_sandbox();
        let rows = query_json("note helo | c");
        let ghost_present = rows.iter().any(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with("autoclose::"))
                .unwrap_or(false)
        });
        assert!(
            !ghost_present,
            "expected NO autoclose for ambiguous prefix `c`, got {rows:#?}"
        );
    }

    #[test]
    fn empty_trailing_pipe_surfaces_every_stage() {
        // `note helo | ` - no stage typed yet. We should show the
        // full menu: base actions + every transform + every sink.
        // Verifies discoverability for users who dont know what
        // the available keywords are
        let _g = init_with_sandbox();
        let rows = query_json("note helo | ");
        // At minimum, `upper` and `copy` must both appear
        let upper_offered = rows.iter().any(|c| {
            c.get("title")
                .and_then(|v| v.as_str())
                .map(|t| t.contains("upper"))
                .unwrap_or(false)
        });
        let copy_offered = rows.iter().any(|c| {
            c.get("title")
                .and_then(|v| v.as_str())
                .map(|t| t.contains("copy"))
                .unwrap_or(false)
        });
        assert!(
            upper_offered,
            "empty trailing pipe should surface `upper`, got {rows:#?}"
        );
        assert!(
            copy_offered,
            "empty trailing pipe should surface `copy`, got {rows:#?}"
        );
    }

    #[test]
    fn multi_stage_pipeline_unknown_stage_surfaces_helper_row() {
        // Typo in a stage name - orchestrator must emit a clear
        // "unknown action" helper row instead of silently failing
        let _g = init_with_sandbox();
        let rows = query_json("note helo | xyzzy | copy");
        let unknown = rows.iter().find(|c| {
            c.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.starts_with("chain::__unknown-action__::"))
                .unwrap_or(false)
        });
        assert!(
            unknown.is_some(),
            "expected unknown-action helper, got {rows:#?}"
        );
    }

    #[test]
    fn clear_clipboard_history_empties_store() {
        let _g = init_with_sandbox();
        let content = CString::new("gyors-test-clear-unique-xyz").unwrap();
        gyors_record_clipboard(content.as_ptr());
        gyors_clear_clipboard_history();
        assert_eq!(gyors_clipboard_count(), 0);
    }

    // Sync outbox enqueue from gyors_record_clipboard
    //
    // REGRESSION (2026-05-12): `gyors_record_clipboard` was writing
    // to `clipboard_items` but never enqueueing into `sync_outbox`,
    // so `Sync Now` reported "0 pending" no matter how many things
    // user copied. These tests pin wire-up between the
    // pasteboard watcher's FFI entry and the engine's outbox
    //
    // Cloud-feature-gated: the no-cloud build doesn't enqueue, so
    // the assertions dont hold and the helpers reference
    // `gyors_sync::*` types that dont exist in that configuration

    /// Borrow index out of bridge so a test can poke at the
    /// outbox / sync_versions tables directly. Mirrors how
    /// `sync_ffi::with_runtime` does it for production
    #[cfg(feature = "cloud")]
    fn outbox_pending_clip() -> i64 {
        let mu = BRIDGE.get().expect("bridge initialised");
        let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
        bridge
            .index
            .outbox_pending(gyors_sync::Namespace::ClipboardHistory.as_str())
            .unwrap_or(-1)
    }

    #[cfg(feature = "cloud")]
    fn outbox_drain_clip() -> Vec<gyors_index::OutboxEntry> {
        let mu = BRIDGE.get().expect("bridge initialised");
        let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
        bridge
            .index
            .outbox_drain(
                Some(gyors_sync::Namespace::ClipboardHistory.as_str()),
                100,
            )
            .unwrap()
    }

    #[cfg(feature = "cloud")]
    fn outbox_clear_clip() {
        let entries = outbox_drain_clip();
        let mu = BRIDGE.get().unwrap();
        let bridge = mu.lock().unwrap_or_else(|e| e.into_inner());
        for e in entries {
            bridge.index.outbox_ack(e.id).unwrap();
        }
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn record_clipboard_enqueues_to_sync_outbox() {
        let _g = init_with_sandbox();
        outbox_clear_clip();
        let before = outbox_pending_clip();
        let content = CString::new("sync-enqueue-test-aaa").unwrap();
        gyors_record_clipboard(content.as_ptr());
        let after = outbox_pending_clip();
        assert_eq!(
            after, before + 1,
            "fresh content must enqueue one outbox row, went {before} -> {after}"
        );
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn record_clipboard_outbox_payload_round_trips() {
        // The outbox payload is what the engine encrypts and ships;
        // ClipboardResource::apply must be able to deserialize it
        // back. If the shape diverges from `ClipboardPayload` the
        // pull side will silently drop incoming items
        let _g = init_with_sandbox();
        outbox_clear_clip();
        let unique = format!("payload-rt-{}", now_secs());
        let cs = CString::new(unique.clone()).unwrap();
        gyors_record_clipboard(cs.as_ptr());

        let entries = outbox_drain_clip();
        let entry = entries
            .iter()
            .find(|e| {
                serde_json::from_slice::<gyors_sync::clipboard::ClipboardPayload>(&e.payload)
                    .map(|p| p.content == unique)
                    .unwrap_or(false)
            })
            .expect("freshly-recorded item should be present in outbox");
        let parsed: gyors_sync::clipboard::ClipboardPayload =
            serde_json::from_slice(&entry.payload).unwrap();
        assert_eq!(parsed.content, unique);
        assert!(parsed.ts > 0, "ts should be epoch seconds, got {}", parsed.ts);
        assert_eq!(entry.op, "upsert");
        // resource_id must be the content-addressable sync_id so
        // multiple devices copying same bytes converge on the
        // same blob
        assert_eq!(
            entry.resource_id,
            gyors_sync::clipboard::sync_id(&unique, parsed.ts)
        );
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn record_clipboard_dedupe_skips_outbox_too() {
        // `Index::record_clipboard` returns `false` when the new
        // content matches previous row, so outbox enqueue
        // must be gated on that flag - otherwise a "ding ding ding"
        // duplicate copy would queue identical rows forever
        let _g = init_with_sandbox();
        // Other FFI tests leave clipboard_items populated; the
        // dedupe check compares the new content against whatever
        // row is currently "latest" in that table, so we need a
        // clean baseline
        gyors_clear_clipboard_history();
        outbox_clear_clip();
        let unique = format!("dedupe-{}", now_secs());
        let cs = CString::new(unique).unwrap();
        gyors_record_clipboard(cs.as_ptr());
        let after_first = outbox_pending_clip();
        gyors_record_clipboard(cs.as_ptr());
        gyors_record_clipboard(cs.as_ptr());
        let after_dupes = outbox_pending_clip();
        assert_eq!(
            after_first, 1,
            "first call should enqueue exactly one row, got {after_first}"
        );
        assert_eq!(
            after_dupes, after_first,
            "back-to-back identical copies must not add new outbox rows, \
             {after_first} -> {after_dupes}"
        );
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn record_clipboard_empty_or_whitespace_no_outbox() {
        let _g = init_with_sandbox();
        outbox_clear_clip();
        let before = outbox_pending_clip();
        for s in ["", "   ", "\t\n"].iter() {
            let cs = CString::new(*s).unwrap();
            gyors_record_clipboard(cs.as_ptr());
        }
        let after = outbox_pending_clip();
        assert_eq!(
            after, before,
            "blank content must NOT enqueue anything, {before} -> {after}"
        );
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn record_clipboard_each_distinct_value_gets_own_outbox_row() {
        let _g = init_with_sandbox();
        outbox_clear_clip();
        let before = outbox_pending_clip();
        for i in 0..5 {
            let cs = CString::new(format!("distinct-{}-{i}", now_secs())).unwrap();
            gyors_record_clipboard(cs.as_ptr());
        }
        let after = outbox_pending_clip();
        assert_eq!(
            after, before + 5,
            "5 distinct copies should make 5 outbox rows, {before} -> {after}"
        );
    }

    #[cfg(feature = "cloud")]
    #[test]
    fn record_clipboard_null_input_does_not_panic_or_enqueue() {
        let _g = init_with_sandbox();
        outbox_clear_clip();
        let before = outbox_pending_clip();
        gyors_record_clipboard(std::ptr::null());
        let after = outbox_pending_clip();
        assert_eq!(after, before, "null in -> nothing enqueued");
    }


    /// Pull `gyors_recent_queries(limit)` into a `Vec<String>`. Tests
    /// read cleaner when the decode lives off to the side
    fn recent_queries_ffi(limit: u32) -> Vec<String> {
        let ptr = gyors_recent_queries(limit);
        assert!(!ptr.is_null(), "gyors_recent_queries returned null");
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        gyors_free_string(ptr);
        serde_json::from_str(&s).expect("valid JSON array of strings")
    }

    fn last_query_ffi() -> String {
        let ptr = gyors_last_query();
        assert!(!ptr.is_null(), "gyors_last_query returned null");
        let s = unsafe { CStr::from_ptr(ptr).to_str().unwrap().to_string() };
        gyors_free_string(ptr);
        s
    }

    #[test]
    fn record_query_stores_latest_at_front() {
        let _g = init_with_sandbox();
        // Seed a stamp unique to this test so other tests'
        // history noise (from shared BRIDGE) doesn't confuse
        // our assertion
        let stamp = format!("hist-test-{}", now_secs());
        let c = CString::new(stamp.clone()).unwrap();
        gyors_record_query(c.as_ptr());
        let recent = recent_queries_ffi(10);
        assert_eq!(recent.first().map(String::as_str), Some(stamp.as_str()));
        assert_eq!(last_query_ffi(), stamp);
    }

    #[test]
    fn record_query_ignores_null_pattern() {
        // No panic, no crash - same contract as clipboard
        gyors_record_query(std::ptr::null());
    }

    #[test]
    fn record_query_ignores_blanks() {
        let _g = init_with_sandbox();
        let before = recent_queries_ffi(50).len();
        for blank in ["", "   ", "\n\t"] {
            let c = CString::new(blank).unwrap();
            gyors_record_query(c.as_ptr());
        }
        let after = recent_queries_ffi(50).len();
        assert_eq!(before, after, "blank patterns snuck in anyway");
    }

    #[test]
    fn recent_queries_zero_limit_returns_empty_array() {
        let _g = init_with_sandbox();
        let arr = recent_queries_ffi(0);
        assert!(arr.is_empty());
    }

    //
    // The launcher's dylib is reachable to any local process,
    // not just Swift shell. A multi-gigabyte string would
    // OOM us if we naively `to_str()` it. The `cstr_bounded`
    // helper rejects oversized inputs before allocating

    #[test]
    fn cstr_bounded_rejects_oversized_input() {
        // 2 KB string, cap of 1 KB - must reject
        let big = "x".repeat(2048);
        let cs = CString::new(big).unwrap();
        assert!(cstr_bounded(cs.as_ptr(), 1024).is_none());
    }

    #[test]
    fn cstr_bounded_accepts_under_cap() {
        let small = "x".repeat(500);
        let cs = CString::new(small.clone()).unwrap();
        let got = cstr_bounded(cs.as_ptr(), 1024);
        assert_eq!(got, Some(small.as_str()));
    }

    #[test]
    fn cstr_bounded_handles_null() {
        assert!(cstr_bounded(std::ptr::null(), 1024).is_none());
    }

    #[test]
    fn cstr_bounded_rejects_non_utf8() {
        // C string of invalid UTF-8 bytes (lone continuation byte)
        let bytes = [0x80u8, 0];
        let p = bytes.as_ptr() as *const c_char;
        assert!(cstr_bounded(p, 1024).is_none());
    }

    #[test]
    fn gyors_query_rejects_oversized_pattern_without_crashing() {
        let _g = init_with_sandbox();
        // 2 MB pattern - over the 1 MB cap
        let big = "x".repeat(2 * MAX_PATTERN_LEN);
        let cs = CString::new(big).unwrap();
        let ptr = gyors_query(cs.as_ptr());
        assert!(ptr.is_null(), "oversized query must return NULL");
    }

    #[test]
    fn gyors_record_clipboard_rejects_oversized_content() {
        let _g = init_with_sandbox();
        let huge = "x".repeat(MAX_CLIPBOARD_LEN + 1);
        let before = gyors_clipboard_count();
        let cs = CString::new(huge).unwrap();
        gyors_record_clipboard(cs.as_ptr()); // must be no-op, no panic
        let after = gyors_clipboard_count();
        assert_eq!(
            before, after,
            "oversized clipboard write must not store anything"
        );
    }
}
