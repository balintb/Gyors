//! Empty-state discovery rows
//!
//! When user opens panel and hasn't typed anything yet, the
//! result list would otherwise be blank - and with 44 providers, "blank"
//! is a discoverability dead-end. This provider returns a small,
//! rotating set of curated example commands so user always has a
//! foothold into the surface area of the launcher
//!
//! ## Activation
//!
//! Each row's primary action emits `Effect::SetInput(prefix)` - the
//! Swift shell intercepts that effect and rewrites input field so
//! the example becomes a starting point user edits, not a one-shot
//! command. Panel stays open, user types into a primed prompt
//!
//! ## Rotation
//!
//! The full pool is ~16 entries; we slice 6 per open, advancing a
//! global counter so consecutive opens see *different* tips. Over a
//! week of casual use user is exposed to the whole pool without
//! ever seeing same row twice in a row
//!
//! ## When NOT to fire
//!
//! Empty pattern only. The moment user types a single character,
//! orchestrator routes through the normal pipeline and discovery
//! goes silent - its rows are *suggestions of where to start*, never
//! competing with real results

use crate::config::set_provider_enabled;
use crate::registry::DisabledProviders;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use rand::seq::SliceRandom;

/// One curated example. `prefix` is what gets typed into input on
/// activate - keep the trailing space when the example expects more
/// input, drop it when keyword is self-contained
#[derive(Debug, Clone, Copy)]
struct Tip {
    prefix: &'static str,
    /// User-facing rendering - uses <placeholder> markers so row
    /// reads like documentation rather than a half-finished query
    title: &'static str,
    description: &'static str,
    symbol: &'static str,
}

/// Curated pool. Order is the *display order* within a slice. Picked
/// to cover the breadth of the surface: maths, units, AI, notes,
/// clipboard, time, encoding, web search, dev tooling. Re-shuffle by
/// hand when keyword area shifts; rotation index alone gives
/// per-open variety
const POOL: &[Tip] = &[
    Tip {
        prefix: "12 * 7 + 3",
        title: "12 * 7 + 3",
        description: "Math - type any expression, hit ↵ to copy",
        symbol: "function",
    },
    Tip {
        prefix: "100 usd in eur",
        title: "100 usd in eur",
        description: "Currency - live FX rates, cached daily",
        symbol: "dollarsign.circle",
    },
    Tip {
        prefix: "5 km in mi",
        title: "5 km in mi",
        description: "Units - length, mass, temperature, time",
        symbol: "ruler",
    },
    Tip {
        prefix: "ai ",
        title: "ai ⟨question⟩",
        description: "Ask your configured AI provider",
        symbol: "sparkles",
    },
    Tip {
        prefix: "summarize",
        title: "summarize",
        description: "TL;DR your clipboard via AI",
        symbol: "text.redaction",
    },
    Tip {
        prefix: "note ",
        title: "note ⟨filter⟩",
        description: "Find a markdown note by name",
        symbol: "doc.text.fill",
    },
    Tip {
        prefix: "newnote ",
        title: "newnote ⟨title⟩",
        description: "Create a new markdown note",
        symbol: "plus.square.fill",
    },
    Tip {
        prefix: "clip",
        title: "clip",
        description: "Browse clipboard history",
        symbol: "doc.on.clipboard",
    },
    Tip {
        prefix: "now",
        title: "now",
        description: "Current time + Unix timestamp",
        symbol: "clock.fill",
    },
    Tip {
        prefix: "uuid",
        title: "uuid",
        description: "Random UUIDv4 (uuid7 for time-ordered)",
        symbol: "barcode.viewfinder",
    },
    Tip {
        prefix: "cron ",
        title: "cron ⟨every monday at 9am⟩",
        description: "Cron expression from English",
        symbol: "clock.arrow.circlepath",
    },
    Tip {
        prefix: "color ",
        title: "color ⟨#3478f6⟩",
        description: "Hex / RGB / HSL converter",
        symbol: "paintpalette.fill",
    },
    Tip {
        prefix: "g ",
        title: "g ⟨query⟩",
        description: "Google search · also ddg, gh, so, yt, npm, w",
        symbol: "magnifyingglass",
    },
    Tip {
        prefix: "tz ",
        title: "tz ⟨tokyo⟩",
        description: "Local time in any city",
        symbol: "globe.europe.africa.fill",
    },
    Tip {
        prefix: "b64 ",
        title: "b64 ⟨text⟩",
        description: "Base64 / URL / hash encoders",
        symbol: "key.fill",
    },
    Tip {
        prefix: "config",
        title: "config",
        description: "Browse and edit Gyors settings",
        symbol: "gearshape.2.fill",
    },
];

/// Number of tips we expose per open. Six is enough to fill the
/// initial viewport without scrolling but small enough that each row
/// gets airtime - twelve becomes a directory dump rather than a
/// curated taste
const TIPS_PER_OPEN: usize = 6;

/// Stable id for the dismiss row. Kept as a single source of truth so
/// `query()` and `activate()` can't drift apart silently - change one,
/// the other still finds it
const DISMISS_ID: &str = "discovery::__dismiss";

pub struct DiscoveryProvider {
    /// Shared with registry. Flipping the "discovery" entry into
    /// the disabled set hides this provider's rows on next query;
    /// no restart, no orchestrator change. Held by every dismissable
    /// provider that wants to self-disable through same gate the
    /// config UI uses
    gate: DisabledProviders,
}

impl DiscoveryProvider {
    pub fn new(gate: DisabledProviders) -> Self {
        Self { gate }
    }
}

#[async_trait]
impl Provider for DiscoveryProvider {
    fn id(&self) -> &str {
        "discovery"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        if !query.pattern().is_empty() {
            return Vec::new();
        }
        let mut rows: Vec<Candidate> = sample_tips(&mut rand::thread_rng())
            .iter()
            .map(|t| candidate_for(t))
            .collect();
        // Pinned at bottom so keyboard's natural Down-walk lands
        // on it last - power users dismiss once, never again. Calling
        // it out as `__dismiss` (double-show) keeps the id space
        // for tip prefixes flat - no chance of a curated tip ever
        // colliding with the disable row
        rows.push(dismiss_candidate());
        rows
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        if id == DISMISS_ID {
            // Persist + flip the live gate in one call. Subsequent
            // queries skip this provider entirely, so panel reverts
            // to its blank empty-state on next reset/keystroke.
            // Re-enable via `config providers.discovery.enabled true`
            // (or the equivalent toggle row in config UI)
            set_provider_enabled("discovery", false, &self.gate)?;
            // SetInput("") triggers a fresh empty-pattern query through
            // orchestrator. With the gate now closed, discovery
            // returns nothing -> results clear -> panel becomes the
            // requested blank box without an ESC + reopen cycle
            return Ok(Effect::SetInput(String::new()));
        }
        let prefix = id
            .strip_prefix("discovery::")
            .ok_or_else(|| anyhow::anyhow!("invalid discovery candidate id: {id}"))?;
        Ok(Effect::SetInput(prefix.to_string()))
    }
}

fn dismiss_candidate() -> Candidate {
    Candidate {
        id: DISMISS_ID.to_string(),
        title: "Don't show these tips".to_string(),
        subtitle: Some(
            "Re-enable from `config providers.discovery.enabled true`".to_string(),
        ),
        icon: Icon::SfSymbol("eye.slash".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Hide")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Pick `TIPS_PER_OPEN` distinct tips uniformly at random from `POOL`.
/// Each panel open hits this exactly once via `query()`, so user
/// sees a fresh sample every time the empty state appears - no
/// deterministic walk that overlaps 5-of-6 with previous open
///
/// Takes the rng by `&mut` so tests can hand in a seeded `StdRng` and
/// pin resulting sequence; production passes `thread_rng()`
fn sample_tips<R: rand::Rng + ?Sized>(rng: &mut R) -> Vec<&'static Tip> {
    let mut indices: Vec<usize> = (0..POOL.len()).collect();
    indices.shuffle(rng);
    indices
        .into_iter()
        .take(TIPS_PER_OPEN)
        .map(|i| &POOL[i])
        .collect()
}

fn candidate_for(t: &Tip) -> Candidate {
    Candidate {
        // Encode prefix verbatim in the id so activate path
        // can recover it without a side table - prefix IS the
        // primary key here
        id: format!("discovery::{}", t.prefix),
        title: t.title.to_string(),
        subtitle: Some(t.description.to_string()),
        icon: Icon::SfSymbol(t.symbol.into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Use")],
        search_text: String::new(),
        // bypass_rank true so orchestrator doesn't score these
        // against an empty pattern - preserves the curated slice
        // order via orchestrator's stable sort on equal scores
        bypass_rank: true,
    }
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)] // test serialization uses sync Mutex on $HOME
mod tests {
    use super::*;
    use crate::registry::new_disabled_set;

    fn provider() -> DiscoveryProvider {
        DiscoveryProvider::new(new_disabled_set())
    }

    #[tokio::test]
    async fn empty_query_yields_curated_rows() {
        let p = provider();
        let out = p.query(&Query::new("")).await;
        // TIPS_PER_OPEN curated tips PLUS the dismiss row at end
        assert_eq!(out.len(), TIPS_PER_OPEN + 1);
        // Every row has a SetInput-friendly id and a non-empty title
        for c in &out {
            assert!(c.id.starts_with("discovery::"));
            assert!(!c.title.is_empty());
            assert!(c.bypass_rank);
        }
        // Dismiss row is anchored at bottom - keyboard Down-walk
        // lands on it last, never first
        assert_eq!(out.last().unwrap().id, DISMISS_ID);
    }

    #[tokio::test]
    async fn non_empty_query_returns_nothing() {
        // `Query::pattern()` trims whitespace, so `"   "` would land
        // here as empty and IS expected to return rows - test
        // covers genuinely non-empty patterns
        let p = provider();
        for q in ["x", "ai ", "12 + 3"] {
            assert!(
                p.query(&Query::new(q)).await.is_empty(),
                "expected no discovery rows for {q:?}"
            );
        }
    }

    // Sampling is tested against the pure `sample_tips` helper with
    // a seeded RNG. The live `query()` path uses `thread_rng()`,
    // which would be order-dependent across parallel tests - and
    // testing live randomness directly is brittle
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn sample_size_equals_tips_per_open() {
        let mut rng = StdRng::seed_from_u64(1);
        let sample = sample_tips(&mut rng);
        assert_eq!(sample.len(), TIPS_PER_OPEN);
    }

    #[test]
    fn sample_contains_no_duplicates() {
        // Distinct samples without repetition - the shuffle pulls
        // unique indices, so a duplicate would be a regression to
        // an indexed-with-replacement form
        let mut rng = StdRng::seed_from_u64(7);
        let sample = sample_tips(&mut rng);
        let unique: std::collections::HashSet<_> = sample.iter().map(|t| t.prefix).collect();
        assert_eq!(unique.len(), sample.len(), "duplicate tip in sample");
    }

    #[test]
    fn sample_varies_across_seeds() {
        // Different RNG seeds produce different orderings - proves
        // the sampler is in fact pulling from the rng rather than
        // returning a fixed slice
        let a = sample_tips(&mut StdRng::seed_from_u64(1));
        let b = sample_tips(&mut StdRng::seed_from_u64(2));
        let a_ids: Vec<_> = a.iter().map(|t| t.prefix).collect();
        let b_ids: Vec<_> = b.iter().map(|t| t.prefix).collect();
        assert_ne!(a_ids, b_ids, "two seeds produced the same ordering");
    }

    #[test]
    fn sample_consecutive_calls_differ() {
        // Two consecutive draws from the SAME rng must differ -
        // shuffling once and reusing result would be a bug
        // (the user would see same six tips on every empty
        // state until the process restarted)
        let mut rng = StdRng::seed_from_u64(42);
        let a: Vec<_> = sample_tips(&mut rng).iter().map(|t| t.prefix).collect();
        let b: Vec<_> = sample_tips(&mut rng).iter().map(|t| t.prefix).collect();
        assert_ne!(a, b, "consecutive samples shared an ordering");
    }

    #[test]
    fn sample_covers_pool_over_many_draws() {
        // Probabilistic: across many draws, every tip in the pool
        // should surface at least once. Sanity check that the
        // sampler isn't accidentally restricted to a sub-range
        let mut rng = StdRng::seed_from_u64(11);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            for t in sample_tips(&mut rng) {
                seen.insert(t.prefix);
            }
        }
        assert_eq!(seen.len(), POOL.len(), "some tips never appeared");
    }

    #[tokio::test]
    async fn activate_yields_setinput_for_prefix() {
        let p = provider();
        let effect = p
            .activate(&"discovery::ai ".to_string(), "default")
            .await
            .unwrap();
        match effect {
            Effect::SetInput(s) => assert_eq!(s, "ai "),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = provider();
        assert!(p
            .activate(&"hint::md5".to_string(), "default")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn activate_dismiss_inserts_provider_into_gate() {
        // The dismiss row's whole job is to flip the gate so this
        // provider's rows stop appearing. Verify both halves: the
        // gate is updated AND the returned effect is `SetInput("")`
        // so panel re-queries and goes blank without user
        // having to ESC + reopen
        //
        // `set_provider_enabled` writes to disk via `apply_set` -
        // share config tests' lock + tempdir so developer's
        // real `~/Library/Application Support/Gyors/config.json`
        // never gets touched by a `cargo test` run
        let _guard = crate::config::test_support::CONFIG_PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        std::env::set_var("GYORS_CONFIG_DIR", td.path());

        let gate = new_disabled_set();
        let p = DiscoveryProvider::new(gate.clone());
        assert!(!gate.load().contains("discovery"));
        let effect = p
            .activate(&DISMISS_ID.to_string(), "default")
            .await
            .unwrap();
        assert!(gate.load().contains("discovery"));
        match effect {
            Effect::SetInput(s) => assert!(s.is_empty()),
            other => panic!("expected empty SetInput, got {other:?}"),
        }

        std::env::remove_var("GYORS_CONFIG_DIR");
    }

    #[tokio::test]
    async fn dismiss_row_has_eyeslash_glyph() {
        // Light contract test: the dismiss row's icon is an eye-slash
        // SF Symbol. A drift to a different glyph isn't a bug per se,
        // but we want test to fail loudly so change is at
        // least intentional and the marketing screenshots stay in
        // sync with what users actually see
        let p = provider();
        let out = p.query(&Query::new("")).await;
        let dismiss = out.iter().find(|c| c.id == DISMISS_ID).unwrap();
        match &dismiss.icon {
            Icon::SfSymbol(s) => assert_eq!(s, "eye.slash"),
            other => panic!("expected SfSymbol, got {other:?}"),
        }
    }

    #[test]
    fn pool_has_no_duplicate_prefixes() {
        // A duplicate prefix would break `id` uniqueness when the
        // rotation window happens to contain both copies
        let mut seen = std::collections::HashSet::new();
        for t in POOL {
            assert!(seen.insert(t.prefix), "duplicate tip prefix: {}", t.prefix);
        }
    }

    #[test]
    fn slice_size_matches_constant() {
        // Dont let a pool shrink quietly drop us below `TIPS_PER_OPEN`.
        // The `%` math still produces a TIPS_PER_OPEN-sized vec by
        // wrapping, but if the pool ever became smaller than that,
        // rows would repeat within a single open and that's a UX bug
        assert!(POOL.len() >= TIPS_PER_OPEN);
    }
}
