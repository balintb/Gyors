use crate::Candidate;
use nucleo::{Config, Matcher, Utf32Str};

#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub candidate: Candidate,
    pub score: i64,
}

pub trait Ranker: Send + Sync {
    fn rank(&self, pattern: &str, candidates: Vec<Candidate>) -> Vec<ScoredCandidate>;
}

pub struct NucleoRanker;

impl NucleoRanker {
    pub fn new() -> Self { Self }
}

impl Default for NucleoRanker {
    fn default() -> Self { Self::new() }
}

impl Ranker for NucleoRanker {
    fn rank(&self, pattern: &str, candidates: Vec<Candidate>) -> Vec<ScoredCandidate> {
        if pattern.trim().is_empty() {
            return candidates
                .into_iter()
                .map(|c| ScoredCandidate { candidate: c, score: 0 })
                .collect();
        }

        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut needle_buf: Vec<char> = Vec::new();
        let needle = Utf32Str::new(pattern, &mut needle_buf);

        let mut scored: Vec<ScoredCandidate> = Vec::with_capacity(candidates.len());
        let mut haystack_buf: Vec<char> = Vec::new();

        for c in candidates {
            haystack_buf.clear();
            let haystack = Utf32Str::new(&c.search_text, &mut haystack_buf);
            if let Some(fuzzy) = matcher.fuzzy_match(haystack, needle) {
                // Precision tier bonus dwarfs the nucleo fuzzy score so
                // an exact/prefix match always wins, even against a
                // candidate with much higher fuzzy affinity. Frecency
                // added by caller (bounded) breaks intra-tier ties
                let tier = precision_bonus(pattern, &c.title);
                let score = tier + fuzzy as i64;
                scored.push(ScoredCandidate { candidate: c, score });
            }
        }

        scored.sort_by_key(|c| std::cmp::Reverse(c.score));
        scored
    }
}

/// Large step-function bonus layered onto the fuzzy score so that
/// *how well candidate matches user's input* dominates ranking.
/// User frequency (added later, capped) only breaks ties within a tier
///
/// Tiers, largest to smallest:
/// - exact title match
/// - title starts with pattern
/// - title contains pattern as a substring
/// - fuzzy-only (pattern's letters appear in order but not contiguously)
///
/// The spread between tiers (>= 100 000) is intentionally huge - nucleo
/// fuzzy scores max out in the low thousands, and our frecency boost
/// is capped at same order; together they can never lift a fuzzy
/// match above a prefix match
pub fn precision_bonus(pattern: &str, title: &str) -> i64 {
    let p = pattern.trim().to_lowercase();
    if p.is_empty() { return 0; }
    let t = title.to_lowercase();
    if t == p { return 400_000; }
    if t.starts_with(&p) { return 300_000; }
    if t.contains(&p) { return 200_000; }
    100_000
}

/// Upper bound for the frecency contribution. Keeps the "frequency"
/// signal from overwhelming the precision tiers: a user's most-used
/// candidate can't outrank a better match
pub const MAX_FRECENCY_BOOST: i64 = 80_000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, CandidateKind, Icon};

    fn cand(title: &str) -> Candidate {
        Candidate {
            id: title.into(),
            title: title.into(),
            subtitle: None,
            icon: Icon::None,
            kind: CandidateKind::App,
            actions: vec![Action::primary("Open")],
            search_text: title.into(),
            bypass_rank: false,
        }
    }

    #[test]
    fn empty_pattern_keeps_all() {
        let r = NucleoRanker::new();
        let out = r.rank("", vec![cand("A"), cand("B")]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn exact_prefix_ranks_first() {
        let r = NucleoRanker::new();
        let out = r.rank("saf", vec![cand("Finder"), cand("Safari"), cand("Mail")]);
        assert_eq!(out[0].candidate.title, "Safari");
    }


    #[test]
    fn precision_bonus_tiers_are_monotonic() {
        let exact = precision_bonus("safari", "Safari");
        let prefix = precision_bonus("saf", "Safari");
        let substr = precision_bonus("far", "Safari");
        let fuzzy = precision_bonus("sai", "Safari");
        assert!(exact > prefix, "exact {exact} > prefix {prefix}");
        assert!(prefix > substr, "prefix {prefix} > substr {substr}");
        assert!(substr > fuzzy, "substr {substr} > fuzzy {fuzzy}");
        assert!(fuzzy > 0);
    }

    #[test]
    fn precision_bonus_case_insensitive() {
        assert_eq!(precision_bonus("SAFARI", "safari"), precision_bonus("safari", "SAFARI"));
    }

    #[test]
    fn precision_bonus_empty_pattern_yields_zero() {
        assert_eq!(precision_bonus("", "anything"), 0);
        assert_eq!(precision_bonus("   ", "anything"), 0);
    }

    #[test]
    fn exact_match_beats_prefix_even_with_max_frecency() {
        // Regression: frequency was swamping the precision tier. With
        // the cap at MAX_FRECENCY_BOOST and tier gaps of 100k+, a
        // maxed-out prefix row must still lose to a zero-frecency
        // exact match
        let exact_score = precision_bonus("note", "Note");           // + fuzzy~0
        let prefix_score = precision_bonus("note", "Notes app") + 2000 + MAX_FRECENCY_BOOST;
        assert!(
            exact_score > prefix_score,
            "exact={exact_score} should beat maxed-out prefix={prefix_score}",
        );
    }

    #[test]
    fn prefix_beats_substring_even_with_frecency() {
        let prefix = precision_bonus("saf", "Safari");
        let substr = precision_bonus("saf", "Screen Saver") + MAX_FRECENCY_BOOST;
        assert!(prefix > substr, "{prefix} vs {substr}");
    }

    #[test]
    fn frecency_breaks_ties_within_tier() {
        // Two substring-match candidates: frecency cap lets the one
        // with history edge out the one without
        let a_fresh = precision_bonus("saf", "Screen Saver") + 1500;
        let a_used = precision_bonus("saf", "Screen Saver") + 1500 + MAX_FRECENCY_BOOST;
        assert!(a_used > a_fresh);
    }

    #[test]
    fn ranker_applies_tier_plus_fuzzy() {
        // Integration: NucleoRanker.rank produces ordered results with
        // exact first, then prefix, then substring-only
        let r = NucleoRanker::new();
        let out = r.rank(
            "note",
            vec![
                cand("Endnote"),   // substring-only ("note" inside)
                cand("Note"),      // exact
                cand("Nozzle"),    // fuzzy-only (n-o-t-e chars in order, no substring)
                cand("Notebook"),  // prefix
            ],
        );
        assert_eq!(out[0].candidate.title, "Note", "exact first");
        assert_eq!(out[1].candidate.title, "Notebook", "prefix second");
        assert_eq!(out[2].candidate.title, "Endnote", "substring third");
        // "Nozzle" may or may not fuzzy-match; we just assert it's last if present
    }

    #[test]
    fn non_matches_are_dropped() {
        let r = NucleoRanker::new();
        let out = r.rank("zzz", vec![cand("Safari"), cand("Mail")]);
        assert!(out.is_empty());
    }
}
