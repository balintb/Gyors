//! Calculator provider - arithmetic + unit conversions via `fend-core`
//!
//! Result always appears at top of the list (`bypass_rank = true`)
//! because ranker can't meaningfully fuzzy-match a numeric result
//! against user's expression

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct CalculatorProvider;

#[async_trait]
impl Provider for CalculatorProvider {
    fn id(&self) -> &str { "calc" }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        match evaluate(query.pattern()) {
            Some(result) => vec![make_candidate(query.pattern(), result)],
            None => vec![],
        }
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let result = id
            .strip_prefix("calc::")
            .ok_or_else(|| anyhow::anyhow!("invalid calc candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(result.to_string()))
    }
}

fn make_candidate(input: &str, result: String) -> Candidate {
    Candidate {
        id: format!("calc::{result}"),
        title: result.clone(),
        subtitle: Some(input.to_string()),
        icon: Icon::SfSymbol("function".into()),
        kind: CandidateKind::Calculation,
        actions: vec![Action::primary("Copy result")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn evaluate(input: &str) -> Option<String> {
    let input = input.trim();
    if !looks_like_math(input) {
        return None;
    }
    let mut ctx = fend_core::Context::new();
    let result = fend_core::evaluate(input, &mut ctx).ok()?;
    let main = result.get_main_result();
    if main.is_empty() || main == input {
        // Skip trivial echoes like "42" -> "42"
        return None;
    }
    Some(main.to_string())
}

fn looks_like_math(s: &str) -> bool {
    if s.is_empty() { return false; }
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    if !has_digit { return false; }
    // Require at least one operator, decimal, space, or unit-letter so pure
    // numeric inputs ("42") dont produce trivial "42 = 42" results
    s.chars().any(|c| {
        matches!(
            c,
            '+' | '-' | '*' | '/' | '^' | '%' | '(' | ')' | '.' | ' ' | '=' | '!'
        ) || c.is_ascii_alphabetic()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // evaluate()

    #[test] fn add() { assert_eq!(evaluate("2+2"), Some("4".into())); }
    #[test] fn subtract() { assert_eq!(evaluate("10-3"), Some("7".into())); }
    #[test] fn multiply() { assert_eq!(evaluate("3*4"), Some("12".into())); }
    #[test] fn divide() { assert_eq!(evaluate("20/4"), Some("5".into())); }
    #[test] fn parens() { assert_eq!(evaluate("(1+2)*3"), Some("9".into())); }
    #[test] fn trim_whitespace() { assert_eq!(evaluate("  2 + 2  "), Some("4".into())); }

    #[test]
    fn decimal_result() {
        let r = evaluate("10/4").unwrap();
        assert!(r.starts_with("2.5"), "got {r:?}");
    }

    #[test]
    fn unit_conversion_produces_result() {
        let r = evaluate("5 kg to lbs").expect("fend should convert units");
        assert!(r.contains("lb"), "got {r:?}");
    }

    #[test]
    fn non_math_is_none() {
        assert!(evaluate("hello").is_none());
        assert!(evaluate("safari").is_none());
    }

    #[test]
    fn empty_is_none() {
        assert!(evaluate("").is_none());
        assert!(evaluate("   ").is_none());
    }

    #[test]
    fn pure_number_is_none() {
        // "42" evaluates to "42" - filtered
        assert!(evaluate("42").is_none());
        assert!(evaluate("123").is_none());
    }

    #[test]
    fn trailing_operator_is_none() {
        assert!(evaluate("2+").is_none());
        assert!(evaluate("*5").is_none());
    }

    // looks_like_math()

    #[test]
    fn looks_like_math_requires_digit() {
        assert!(!looks_like_math(""));
        assert!(!looks_like_math("abc"));
        assert!(!looks_like_math("+-*"));
    }

    #[test]
    fn looks_like_math_requires_sigil_or_letter() {
        // Pure digits shouldn't look like math - they'd produce trivial echoes
        assert!(!looks_like_math("12345"));
    }

    #[test]
    fn looks_like_math_accepts_expressions() {
        assert!(looks_like_math("2+2"));
        assert!(looks_like_math("(1-2)"));
        assert!(looks_like_math("5 kg"));
        assert!(looks_like_math("3.14"));
    }

    // Provider impl

    #[tokio::test]
    async fn provider_yields_calc_candidate() {
        let p = CalculatorProvider;
        let out = p.query(&Query::new("2+2")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "4");
        assert_eq!(out[0].subtitle.as_deref(), Some("2+2"));
        assert_eq!(out[0].kind, CandidateKind::Calculation);
        assert!(out[0].bypass_rank);
        assert_eq!(out[0].actions.len(), 1);
        assert_eq!(out[0].actions[0].label, "Copy result");
    }

    #[tokio::test]
    async fn provider_skips_non_math() {
        let p = CalculatorProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
        assert!(p.query(&Query::new("")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_yields_copy_effect() {
        let p = CalculatorProvider;
        let eff = p.activate(&"calc::7".to_string(), "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "7"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_rejects_foreign_id() {
        let p = CalculatorProvider;
        assert!(p.activate(&"apps::Safari".to_string(), "default").await.is_err());
    }
}
