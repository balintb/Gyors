//! Regex validator / match tester. Keyword `re` / `regex`
//!
//! Forms:
//! - `re <pattern>`              -> validate only; error message if bad
//! - `re <pattern> :: <text>`    -> validate + find matches in `<text>`
//!
//! Always emits a single candidate summarising status. Copy action puts
//! the pattern itself (or the first match text) on clipboard so
//! users can move regex into scripts or search boxes

use async_trait::async_trait;
use regex::Regex;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct RegexProvider;

#[async_trait]
impl Provider for RegexProvider {
    fn id(&self) -> &str {
        "re"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(rest) = pattern
            .strip_prefix("re ")
            .or_else(|| pattern.strip_prefix("regex "))
        else {
            return vec![];
        };
        let rest = rest.trim();
        if rest.is_empty() {
            return vec![];
        }

        let (regex_str, test_text) = split_pattern_and_text(rest);
        let regex_str = regex_str.trim();
        if regex_str.is_empty() {
            return vec![];
        }

        match Regex::new(regex_str) {
            Ok(re) => match test_text {
                Some(text) => vec![match_candidate(&re, text)],
                None => vec![valid_candidate(regex_str)],
            },
            Err(e) => vec![error_candidate(regex_str, &e.to_string())],
        }
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let payload = id
            .strip_prefix("re::ok::")
            .or_else(|| id.strip_prefix("re::match::"))
            .or_else(|| id.strip_prefix("re::err::"))
            .ok_or_else(|| anyhow::anyhow!("invalid re candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(payload.to_string()))
    }
}

/// Split on the first ` :: ` delimiter (space-colon-colon-space) so
/// actual patterns can still contain `::` if they want. Returns `(pattern,
/// Some(text))` when a delimiter is present, `(pattern, None)` otherwise
fn split_pattern_and_text(rest: &str) -> (&str, Option<&str>) {
    if let Some(idx) = rest.find(" :: ") {
        let (p, t) = rest.split_at(idx);
        return (p, Some(&t[" :: ".len()..]));
    }
    (rest, None)
}

fn valid_candidate(pattern: &str) -> Candidate {
    Candidate {
        id: format!("re::ok::{pattern}"),
        title: format!("✓ valid · /{pattern}/"),
        subtitle: Some("Add `  ::  <text>` to test against a string".into()),
        icon: Icon::SfSymbol("checkmark.circle.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy Pattern")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn match_candidate(re: &Regex, text: &str) -> Candidate {
    let matches: Vec<_> = re.find_iter(text).collect();
    let count = matches.len();
    let first = matches.first().map(|m| m.as_str()).unwrap_or("");
    let summary = if count == 0 {
        "0 matches".to_string()
    } else if count == 1 {
        format!("1 match · {}", truncate(first, 60))
    } else {
        format!("{count} matches · first: {}", truncate(first, 60))
    };
    Candidate {
        id: format!("re::match::{first}"),
        title: format!("/{}/", re.as_str()),
        subtitle: Some(summary),
        icon: Icon::SfSymbol("text.magnifyingglass".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy First Match")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn error_candidate(pattern: &str, err: &str) -> Candidate {
    Candidate {
        id: format!("re::err::{pattern}"),
        title: "Invalid regex".into(),
        subtitle: Some(truncate(err, 120)),
        icon: Icon::SfSymbol("exclamationmark.triangle.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy Pattern")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn truncate(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max {
        flat
    } else {
        let head: String = flat.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = RegexProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn keyword_with_no_pattern_empty() {
        let p = RegexProvider;
        assert!(p.query(&Query::new("re ")).await.is_empty());
    }

    #[tokio::test]
    async fn valid_pattern_only_yields_ok_candidate() {
        let p = RegexProvider;
        let out = p.query(&Query::new(r"re \d+")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("re::ok::"));
        assert!(out[0].title.contains("valid"));
    }

    #[tokio::test]
    async fn invalid_pattern_yields_error_candidate() {
        let p = RegexProvider;
        let out = p.query(&Query::new("re (unclosed")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Invalid regex");
    }

    #[tokio::test]
    async fn regex_alias_works() {
        let p = RegexProvider;
        let out = p.query(&Query::new(r"regex \w+")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("valid"));
    }

    #[tokio::test]
    async fn with_text_reports_match_count() {
        let p = RegexProvider;
        let out = p.query(&Query::new(r"re \d+ :: foo 12 bar 34 baz")).await;
        assert_eq!(out.len(), 1);
        let sub = out[0].subtitle.as_deref().unwrap();
        assert!(sub.contains("2 matches"), "got {sub:?}");
        assert!(sub.contains("12"));
    }

    #[tokio::test]
    async fn with_text_zero_matches_reported() {
        let p = RegexProvider;
        let out = p.query(&Query::new(r"re \d+ :: nothing here")).await;
        assert!(out[0].subtitle.as_deref().unwrap().contains("0 matches"));
    }

    #[tokio::test]
    async fn with_text_one_match() {
        let p = RegexProvider;
        let out = p.query(&Query::new(r"re \d+ :: 42 items")).await;
        let sub = out[0].subtitle.as_deref().unwrap();
        assert!(sub.contains("1 match"));
        assert!(!sub.contains("1 matches"));
    }

    #[tokio::test]
    async fn activate_valid_copies_pattern() {
        let p = RegexProvider;
        let out = p.query(&Query::new(r"re \d+")).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, r"\d+"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_match_copies_first_match() {
        let p = RegexProvider;
        let out = p.query(&Query::new(r"re \d+ :: foo 42 bar")).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "42"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = RegexProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn split_pattern_and_text_no_delimiter() {
        assert_eq!(split_pattern_and_text(r"\d+"), (r"\d+", None));
    }

    #[test]
    fn split_pattern_and_text_with_delimiter() {
        assert_eq!(
            split_pattern_and_text(r"\d+ :: 12 34"),
            (r"\d+", Some("12 34"))
        );
    }

    #[test]
    fn split_pattern_and_text_preserves_colons_in_pattern() {
        // `::` without surrounding spaces stays part of the pattern
        assert_eq!(
            split_pattern_and_text(r"ab::cd :: xy"),
            (r"ab::cd", Some("xy"))
        );
    }
}
