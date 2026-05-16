use crate::{Candidate, CandidateId, Effect, Query};
use async_trait::async_trait;

/// One keyword owned by a provider. The metadata block that drove
/// `hints.rs` lived in a parallel const array hand-maintained next
/// to provider; with `KeywordSpec` it lives ON provider, so
/// adding a new keyword is one place instead of two-and-a-quarter
///
/// `aliases` are extra keywords that route to same handler -
/// dropdown still surfaces only canonical `keyword`. Empty
/// `aliases: &[]` is fine for keywords with no shorthand
///
/// Hints, command discoverability, and (in the future) the typo-
/// correct fallback in `CommandHintsProvider` all read from this
#[derive(Debug, Clone, Copy)]
pub struct KeywordSpec {
    pub keyword: &'static str,
    pub aliases: &'static [&'static str],
    /// Human-readable usage line shown in the hint subtitle:
    /// `"b64 <text>"`, `"clip [filter]"`, `"now"`. Mirrors what the
    /// hint provider currently renders
    pub syntax: &'static str,
    pub description: &'static str,
    /// SF Symbol name for row icon
    pub symbol: &'static str,
    /// True for keywords whose provider produces output without
    /// arguments (`now`, `uuid`, `lorem`). The hint provider hides
    /// the redundant hint row when user has typed keyword
    /// exactly - otherwise the hint appears above actual output
    /// and pushes answer off the top of the list
    pub self_contained: bool,
}

impl KeywordSpec {
    pub const fn new(
        keyword: &'static str,
        syntax: &'static str,
        description: &'static str,
        symbol: &'static str,
    ) -> Self {
        Self {
            keyword,
            aliases: &[],
            syntax,
            description,
            symbol,
            self_contained: false,
        }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;

    async fn query(&self, query: &Query) -> Vec<Candidate>;

    async fn activate(
        &self,
        candidate_id: &CandidateId,
        action_id: &str,
    ) -> anyhow::Result<Effect>;

    /// Keywords this provider claims. Defaults to none - providers
    /// like `AppsProvider` and `FilesProvider` respond to fuzzy
    /// match against names rather than to a typed keyword
    ///
    /// THE source of truth for "what keywords does X own?" - hints,
    /// autocomplete, typo correction, and the eventual single-pass
    /// hint generator all walk this list. Keep entries here in sync
    /// with anything provider actually parses; a regression
    /// test (`registry::tests::declared_keywords_match_hints`)
    /// checks for drift against the current `hints.rs`
    fn keywords(&self) -> &'static [KeywordSpec] {
        &[]
    }
}
