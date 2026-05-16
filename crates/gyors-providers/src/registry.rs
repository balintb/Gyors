//! Provider registration and orchestration
//!
//! Before this refactor, wiring a new provider required edits to six
//! separate files. `ProviderRegistry` collapses that to two:
//!
//! 1. Create `src/your_provider.rs` and implement `Provider`.
//! 2. Add one line to `ProviderRegistry::builder().add(YourProvider)`
//!    at call site (e.g. `gyors-ipc/src/lib.rs`)
//!
//! Registry handles
//! - parallel querying of every provider,
//! - single-provider fan-out for `QueryMode::Clipboard` / `Shell`,
//! - id-prefix routing for `activate` (no hand-maintained match arms)
//!
//! The Provider trait stays untouched - registry is a thin wrapper
//! that stores boxed trait objects. Per-query dispatch overhead is a
//! handful of vtable calls; irrelevant next to FS I/O and SQLite

use anyhow::Result;
use arc_swap::ArcSwap;
use futures::future::join_all;
use gyors_core::{Candidate, CandidateId, Effect, Provider, Query};
use std::collections::HashSet;
use std::sync::Arc;

/// Shared set of provider ids user has disabled. Wrapped in an
/// `ArcSwap` so config provider can flip individual providers
/// on/off live - no restart, no allocator thrash on the hot path,
/// readers get a lock-free snapshot
///
/// Empty by default (all providers enabled)
pub type DisabledProviders = Arc<ArcSwap<HashSet<String>>>;

pub fn new_disabled_set() -> DisabledProviders {
    Arc::new(ArcSwap::from(Arc::new(HashSet::new())))
}

/// Provider ids that are considered core to the launcher's identity -
/// an "app launcher that can't launch apps" isn't useful. Config
/// UI won't emit toggle rows for these, and the disabled-set filter
/// ignores them even if someone edits config.json directly
pub const ALWAYS_ON_PROVIDERS: &[&str] = &[
    "apps",   // primary launcher function
    "config", // needs to stay reachable so users can re-enable others
    "hint",   // command discovery
];

pub fn is_always_on(id: &str) -> bool {
    ALWAYS_ON_PROVIDERS.contains(&id)
}

/// Owns every built-in provider as a trait object. Construct via
/// `ProviderRegistry::builder()` and feed result into whichever
/// orchestrator needs it (the IPC bridge, the CLI, tests)
pub struct ProviderRegistry {
    providers: Vec<Box<dyn Provider>>,
    /// Shared gate. When a provider's id() is in this set, the
    /// registry skips invoking it on `query_*` and refuses `dispatch`
    disabled: DisabledProviders,
}

impl ProviderRegistry {
    pub fn builder() -> RegistryBuilder {
        RegistryBuilder {
            providers: Vec::new(),
            disabled: new_disabled_set(),
        }
    }

    /// Build with a caller-supplied disabled set so config provider
    /// can share same `ArcSwap` and toggle providers live
    pub fn builder_with_gate(disabled: DisabledProviders) -> RegistryBuilder {
        RegistryBuilder { providers: Vec::new(), disabled }
    }

    /// Snapshot handle to the shared disabled set. Consumers (the
    /// config provider) use this to read+write toggles without having
    /// to know about registry's internals
    pub fn gate(&self) -> DisabledProviders {
        Arc::clone(&self.disabled)
    }

    /// Append a provider to an already-built registry. Useful for
    /// providers that introspect registry (e.g. `ConfigProvider`,
    /// which lists every provider's enable/disable toggle) - they
    /// need to know the final id list before being constructed
    pub fn add_provider<P: Provider + 'static>(&mut self, p: P) {
        self.providers.push(Box::new(p));
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Ids of every registered provider, in registration order. Useful
    /// for diagnostics and completion scripts
    pub fn ids(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.id()).collect()
    }

    fn is_enabled(&self, id: &str) -> bool {
        // Always-on providers short-circuit - even a config.json that
        // lists them as disabled is overridden. Prevents user from
        // softlocking themselves out of the apps launcher or the
        // config UI
        if is_always_on(id) { return true; }
        !self.disabled.load().contains(id)
    }

    /// Run every enabled provider against `query` in parallel and
    /// flatten results, preserving per-provider ordering within
    /// each slice. Ranking, filtering, and frecency are left to the
    /// caller - registry is deliberately shallow
    pub async fn query_all(&self, query: &Query) -> Vec<Candidate> {
        let futures = self
            .providers
            .iter()
            .filter(|p| self.is_enabled(p.id()))
            .map(|p| p.query(query));
        let nested: Vec<Vec<Candidate>> = join_all(futures).await;
        nested.into_iter().flatten().collect()
    }

    /// Query a single provider by its `id()`. Returns empty for
    /// unknown ids or disabled providers so callers can fall through
    pub async fn query_one(&self, provider_id: &str, query: &Query) -> Vec<Candidate> {
        if !self.is_enabled(provider_id) { return Vec::new(); }
        match self.providers.iter().find(|p| p.id() == provider_id) {
            Some(p) => p.query(query).await,
            None => Vec::new(),
        }
    }

    /// Query every enabled provider except the ones listed in
    /// `exclude`. Used by orchestrator to skip expensive keyword-
    /// gated providers (file search, shell) when user hasn't
    /// opted in - running them only to drop results wastes real
    /// time (mdfind spawns a subprocess per keystroke)
    pub async fn query_all_except(
        &self,
        exclude: &[&str],
        query: &Query,
    ) -> Vec<Candidate> {
        let futures = self
            .providers
            .iter()
            .filter(|p| !exclude.contains(&p.id()) && self.is_enabled(p.id()))
            .map(|p| p.query(query));
        let nested: Vec<Vec<Candidate>> = join_all(futures).await;
        nested.into_iter().flatten().collect()
    }

    /// Route an activation by the id prefix (e.g. `"apps::Safari"` ->
    /// `AppsProvider`). Disabled providers still dispatch - users
    /// activating an existing candidate (e.g. from frecency history)
    /// after disabling shouldn't silently fail. They just won't get
    /// new candidates in the list until re-enabled
    pub async fn dispatch(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        let prefix = id.split("::").next().unwrap_or("");
        let provider = self
            .providers
            .iter()
            .find(|p| p.id() == prefix)
            .ok_or_else(|| anyhow::anyhow!("unknown provider prefix: {prefix:?}"))?;
        provider.activate(id, action).await
    }
}

/// Fluent builder so registration reads naturally at call sites:
///
/// ```ignore
/// let registry = ProviderRegistry::builder()
///     .add(CalculatorProvider)
///     .add(AppsProvider::new().await?)
///     .add(ClipboardProvider::new(index.clone()))
///     .build();
/// ```
pub struct RegistryBuilder {
    providers: Vec<Box<dyn Provider>>,
    disabled: DisabledProviders,
}

impl RegistryBuilder {
    /// Append a provider. Accepts any concrete type that implements
    /// `Provider + 'static`; the builder boxes it for storage
    #[allow(clippy::should_implement_trait)]
    pub fn add<P: Provider + 'static>(mut self, p: P) -> Self {
        self.providers.push(Box::new(p));
        self
    }

    pub fn build(self) -> ProviderRegistry {
        ProviderRegistry { providers: self.providers, disabled: self.disabled }
    }

    /// Count as the builder grows - useful for sanity-checks ("did we
    /// register everything?") in tests
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

/// The pre-initialised async providers + shared state every binary
/// needs to wire up default provider set. Callers build this
/// however they like - IPC parallelises the async constructors for
/// fast cold start, CLI does them sequentially. Once it's filled,
/// `add_core_providers` does rest in one place
///
/// Keeping the per-binary differences (parallel-vs-sequential init,
/// plugin loading, panel-only providers added afterwards) outside
/// the shared helper means the helper itself is just a flat list of
/// `.add(...)` calls - adding a new core provider is a one-line
/// edit, both binaries pick it up automatically
pub struct CoreProviderContext {
    pub index: std::sync::Arc<gyors_index::Index>,
    pub disabled: DisabledProviders,
    pub apps: crate::apps::AppsProvider,
    pub git_repos: crate::git_repos::GitReposProvider,
    pub shortcuts: crate::shortcuts::ShortcutsProvider,
    pub notes: crate::notes::NotesProvider,
    pub snippets: crate::snippets::SnippetsProvider,
    pub ssh: crate::ssh::SshProvider,
    pub currency: crate::currency::CurrencyProvider,
}

/// Add every provider that ships in BOTH menu-bar app and the
/// `riff` CLI. Panel-only providers (`AiTransformsProvider`,
/// `JwtProvider`, `InputAutocompleteProvider`, `PluginsProvider`,
/// `ScratchpadProvider`) live behind UI affordances the CLI doesn't
/// render - IPC adds those itself after this helper runs
///
/// One source of truth for the common 45-provider set. Adding a new
/// core provider is a single `.add(...)` line here; both the
/// menu-bar app and the CLI pick it up automatically
pub fn add_core_providers(
    builder: RegistryBuilder,
    ctx: CoreProviderContext,
) -> RegistryBuilder {
    use crate::*;
    let CoreProviderContext {
        index,
        disabled,
        apps,
        git_repos,
        shortcuts,
        notes,
        snippets,
        ssh,
        currency,
    } = ctx;
    // Split the long builder chain around the optional AiProvider
    // so no-AI build can drop just that one row. Everything
    // before/after is unconditional
    let builder = builder
        .add(apps)
        .add(CalculatorProvider)
        .add(FilesProvider::new())
        .add(ClipboardProvider::new(std::sync::Arc::clone(&index)))
        .add(SystemProvider)
        .add(ShellProvider)
        .add(WebSearchProvider)
        .add(EncodingProvider)
        .add(KillProvider)
        .add(EmojiProvider)
        .add(ColorProvider)
        .add(TimeProvider)
        .add(GeneratorsProvider)
        .add(CaseProvider)
        .add(WindowManagementProvider)
        .add(ScreenshotProvider)
        .add(git_repos)
        .add(DictionaryProvider)
        .add(QrProvider);
    #[cfg(feature = "ai")]
    let builder = builder.add(AiProvider);
    builder
        .add(notes)
        .add(snippets)
        .add(shortcuts)
        .add(TextOpsProvider)
        .add(SystemPrefsProvider)
        .add(HttpStatusProvider)
        .add(PortProvider)
        .add(MimeProvider)
        .add(DnsProvider)
        .add(RomanProvider)
        .add(NumFormatProvider)
        .add(MorseProvider)
        .add(CodepointProvider)
        .add(JsonProvider)
        .add(RegexProvider)
        .add(UnitConverterProvider)
        .add(TimezoneProvider)
        .add(ssh)
        .add(BrowserTabsProvider)
        .add(RecentProvider)
        .add(BaseConverterProvider)
        .add(FormatConverterProvider)
        .add(currency)
        .add(TimerProvider)
        .add(CommandHintsProvider)
        .add(CronProvider)
        .add(DiscoveryProvider::new(disabled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use gyors_core::{Action, CandidateKind, Icon};

    struct StubProvider {
        id: &'static str,
        tag: &'static str,
    }

    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            self.id
        }
        async fn query(&self, _q: &Query) -> Vec<Candidate> {
            vec![Candidate {
                id: format!("{}::{}", self.id, self.tag),
                title: self.tag.into(),
                subtitle: None,
                icon: Icon::None,
                kind: CandidateKind::Action,
                actions: vec![Action::primary("Go")],
                search_text: String::new(),
                bypass_rank: true,
            }]
        }
        async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
            Ok(Effect::CopyToClipboard(format!("{}:{id}", self.tag)))
        }
    }

    fn two_registry() -> ProviderRegistry {
        ProviderRegistry::builder()
            .add(StubProvider { id: "a", tag: "alpha" })
            .add(StubProvider { id: "b", tag: "beta" })
            .build()
    }

    #[tokio::test]
    async fn query_all_flattens_in_registration_order() {
        let r = two_registry();
        let out = r.query_all(&Query::new("x")).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a::alpha");
        assert_eq!(out[1].id, "b::beta");
    }

    #[tokio::test]
    async fn query_one_matches_by_id() {
        let r = two_registry();
        let out = r.query_one("b", &Query::new("x")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "b::beta");
    }

    #[tokio::test]
    async fn query_one_unknown_id_returns_empty() {
        let r = two_registry();
        assert!(r.query_one("zzz", &Query::new("x")).await.is_empty());
    }

    #[tokio::test]
    async fn dispatch_routes_by_id_prefix() {
        let r = two_registry();
        let eff = r.dispatch(&"a::alpha".to_string(), "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert!(s.starts_with("alpha:")),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_unknown_prefix_errors() {
        let r = two_registry();
        assert!(r.dispatch(&"zzz::foo".to_string(), "default").await.is_err());
    }

    #[test]
    fn builder_tracks_count_and_ids() {
        let r = two_registry();
        assert_eq!(r.len(), 2);
        assert_eq!(r.ids(), vec!["a", "b"]);
    }

    // Regression: excluded providers must NOT be invoked

    /// Counts how many times its `query` method is called. Used to
    /// prove `query_all_except` never invokes excluded providers -
    /// critical because `FilesProvider::query` spawns `mdfind`, and a
    /// filtered-out-after-the-fact approach still paid the subprocess
    /// cost per keystroke
    struct CountingProvider {
        id: &'static str,
        count: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl Provider for CountingProvider {
        fn id(&self) -> &str {
            self.id
        }
        async fn query(&self, _q: &Query) -> Vec<Candidate> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Vec::new()
        }
        async fn activate(&self, _id: &CandidateId, _a: &str) -> anyhow::Result<Effect> {
            Ok(Effect::None)
        }
    }

    use std::sync::Arc;

    #[tokio::test]
    async fn regression_query_all_except_skips_excluded_providers() {
        let fast = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let heavy = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = ProviderRegistry::builder()
            .add(CountingProvider { id: "fast", count: Arc::clone(&fast) })
            .add(CountingProvider { id: "heavy", count: Arc::clone(&heavy) })
            .build();

        for _ in 0..10 {
            registry
                .query_all_except(&["heavy"], &Query::new("xyz"))
                .await;
        }
        assert_eq!(fast.load(std::sync::atomic::Ordering::SeqCst), 10);
        // THE regression: `heavy` must not be invoked even once
        assert_eq!(heavy.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn query_all_except_with_empty_exclude_behaves_like_query_all() {
        let a = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let b = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = ProviderRegistry::builder()
            .add(CountingProvider { id: "a", count: Arc::clone(&a) })
            .add(CountingProvider { id: "b", count: Arc::clone(&b) })
            .build();
        registry.query_all_except(&[], &Query::new("x")).await;
        assert_eq!(a.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(b.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn query_all_except_unknown_ids_are_silently_ignored() {
        let a = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = ProviderRegistry::builder()
            .add(CountingProvider { id: "a", count: Arc::clone(&a) })
            .build();
        // Passing an id that doesn't exist in registry shouldn't
        // error or change behaviour - all registered providers run
        registry.query_all_except(&["nonexistent"], &Query::new("x")).await;
        assert_eq!(a.load(std::sync::atomic::Ordering::SeqCst), 1);
    }


    /// Cheap in-process timing for `query_all_except`. Not a scientific
    /// microbench - CI noise etc. - but it catches the "someone added
    /// a per-keystroke subprocess" class of bug very cheaply. Budget
    /// is deliberately generous (100 us per call across 40 trivial
    /// providers) so it doesn't flap under load
    #[tokio::test]
    async fn perf_query_all_except_under_budget() {
        // 40 near-no-op providers to simulate the live registry size
        let mut b = ProviderRegistry::builder();
        for i in 0..40 {
            let id: &'static str = Box::leak(format!("p{i}").into_boxed_str());
            b = b.add(CountingProvider {
                id,
                count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            });
        }
        let registry = b.build();

        // Warm-up - first call pays allocator init, JIT, etc
        let _ = registry.query_all_except(&[], &Query::new("x")).await;

        let iters = 1_000u32;
        let start = std::time::Instant::now();
        for _ in 0..iters {
            let _ = registry.query_all_except(&[], &Query::new("xyz")).await;
        }
        let per_call_us = start.elapsed().as_micros() as f64 / iters as f64;
        // 100 us / call is 10x the real budget - lets CI / heavy load
        // not trip test while still catching obvious regressions
        assert!(
            per_call_us < 100.0,
            "query_all_except regressed: {per_call_us:.2} µs/call \
             (budget 100 µs); investigate before shipping",
        );
    }

    // Keyword metadata SSOT (Provider::keywords())

    /// Pin the proof-of-concept: `EncodingProvider::keywords()`
    /// declares every keyword provider's `apply_op` table parses.
    /// Drift here means a future contributor added a new encoding
    /// keyword in `parse_op` but forgot to surface it via trait
    /// method, so hint generator (once it walks `keywords()`)
    /// would miss the new keyword
    #[test]
    fn encoding_provider_declares_all_its_keywords() {
        use crate::EncodingProvider;
        let p = EncodingProvider;
        let kws = p.keywords();
        let names: std::collections::HashSet<&str> =
            kws.iter().map(|k| k.keyword).collect();
        // Hash-family keywords: hard-pinned because users rely on
        // these from muscle memory
        for required in [
            "b64", "b64d", "url", "urld", "md5", "sha1", "sha256",
            "sha3", "sha3-512", "blake3", "rot13", "caesar", "hmac",
            "htmlescape", "htmlunescape", "jsonescape", "jsonunescape",
        ] {
            assert!(
                names.contains(required),
                "EncodingProvider::keywords() missing `{required}` - \
                 the dropdown / hint surface won't show it"
            );
        }
    }

    /// Pin trait's "default empty" semantics. Provider implementors
    /// who dont own keywords (apps, files, calc) should leave the
    /// default - anyone who deletes default impl by accident
    /// breaks the build for every such provider, which is good
    #[test]
    fn provider_keywords_default_is_empty() {
        let stub = StubProvider { id: "x", tag: "y" };
        assert!(stub.keywords().is_empty());
    }

    /// Aliases must not collide with canonicals from the SAME provider -
    /// a self-collision means `find_by_keyword` would resolve the alias
    /// to two different specs depending on iteration order. Catches a
    /// future copy-paste bug in `KEYWORDS` arrays
    #[test]
    fn encoding_aliases_dont_collide_with_canonicals() {
        use crate::EncodingProvider;
        let kws = EncodingProvider.keywords();
        let mut seen: std::collections::HashSet<&str> = Default::default();
        for spec in kws {
            assert!(
                seen.insert(spec.keyword),
                "duplicate canonical `{}` in EncodingProvider::keywords()",
                spec.keyword
            );
        }
        for spec in kws {
            for alias in spec.aliases {
                assert!(
                    !seen.contains(alias) || *alias == spec.keyword,
                    "alias `{alias}` of `{}` collides with another canonical",
                    spec.keyword
                );
            }
        }
    }

    /// `KeywordSpec::syntax` must lead with canonical keyword
    /// itself - the hint UI shows the syntax line verbatim, and a
    /// mismatch ("`b64 <text>`" but canonical "base64") would
    /// confuse users about what to actually type
    #[test]
    fn encoding_syntax_strings_lead_with_their_keyword() {
        use crate::EncodingProvider;
        for spec in EncodingProvider.keywords() {
            assert!(
                spec.syntax.starts_with(spec.keyword),
                "`{}`.syntax (`{}`) doesn't start with the keyword",
                spec.keyword,
                spec.syntax
            );
        }
    }
}
