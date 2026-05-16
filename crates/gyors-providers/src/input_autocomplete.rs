//! Auto-close brackets for JSON-style inputs typed into query
//!
//! Watches the known structured-data keywords (`json `, `jq `,
//! `json2yaml ` ...) and, when the tail has unbalanced `{` / `[`, emits a
//! hint candidate offering the closed form. Because the id is
//! `hint::...`, Tab commits the completion via existing autocomplete
//! path (see `ViewModel.tryAutoComplete`), and Enter likewise fires
//! `Effect::SetInput` without dismissing panel
//!
//! Scope is intentionally narrow:
//!   - Only brackets (`{}`, `[]`) - quotes and commas are too
//!     ambiguous to guess safely.
//!   - Skip chars inside string literals, respecting `\"` escapes.
//!   - Skip entirely when we detect a mismatched close (`}` with no
//!     matching open) - that's a typo we dont want to hide

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct InputAutocompleteProvider;

/// Keywords whose tail is bracket-structured input we can usefully
/// balance. `strip_prefix` on each; first match wins
const KEYWORDS: &[&str] = &[
    "json2yaml ",
    "json2toml ",
    "yaml2json ",
    "yaml2toml ",
    "toml2json ",
    "toml2yaml ",
    "json ",
    "jq ",
];

#[async_trait]
impl Provider for InputAutocompleteProvider {
    fn id(&self) -> &str {
        "autoclose"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some((keyword, tail)) = KEYWORDS
            .iter()
            .find_map(|kw| pattern.strip_prefix(kw).map(|t| (*kw, t)))
        else {
            return vec![];
        };
        // Ignore purely-whitespace tails - user hasn't started
        // typing the document yet
        if tail.trim().is_empty() {
            return vec![];
        }
        let Some(closing) = missing_closers(tail) else {
            return vec![];
        };
        let full = format!("{keyword}{tail}{closing}");
        // Title leads with the balanced input so `precision_bonus` sees
        // it as a prefix match of whatever user has typed so far
        // (tier 300k) - that keeps this hint above sibling bypass rows
        // like "Invalid JSON" (substring tier 200k at best)
        let title: String = flatten(&full, 80);
        vec![Candidate {
            // Id must start with our provider id (`autoclose`) so the
            // registry's `::`-prefix dispatch routes activation back to
            // this provider. Using `hint::` would hand it to the
            // CommandHintsProvider, which would cheerfully re-emit
            // `SetInput("autoclose::<stuff> ")` and splat the internal
            // prefix into user's query field
            id: format!("autoclose::{full}"),
            title,
            subtitle: Some(format!("Close brackets ({closing}) · ⇥ complete")),
            icon: Icon::SfSymbol("text.append".into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("Complete")],
            search_text: String::new(),
            // bypass_rank so we sit in the prioritized group above
            // normal fuzzy results; precision tier still decides
            // placement within that group
            bypass_rank: true,
        }]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let full = id
            .strip_prefix("autoclose::")
            .ok_or_else(|| anyhow::anyhow!("invalid autoclose id: {id}"))?;
        Ok(Effect::SetInput(full.to_string()))
    }
}

/// Flatten newlines/tabs to spaces and clip to `max` chars (`...` if
/// truncated) so candidate title renders on one row even
/// when user pastes a multi-line JSON document
fn flatten(s: &str, max: usize) -> String {
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

/// Return the closing-bracket tail needed to balance `s`, or `None` if
/// input is already balanced / unbalanceable (mismatched close /
/// unterminated string)
pub fn missing_closers(s: &str) -> Option<String> {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    for c in s.chars() {
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            match c {
                '\\' => escape = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']'
                // Mismatched close -> refuse to guess. Returning None
                // means we stay silent rather than emit noise for input
                // user clearly hasn't finished composing
                if stack.pop() != Some(c) => {
                    return None;
                }
            _ => {}
        }
    }
    // Unterminated string: we can't reliably close brackets without
    // knowing how long the string should be
    if in_string {
        return None;
    }
    if stack.is_empty() {
        return None;
    }
    // Stack stores expected closers in order of appearance. Closing
    // them happens innermost-first, i.e. reverse stack
    Some(stack.into_iter().rev().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_object_needs_nothing() {
        assert_eq!(missing_closers(r#"{"a":1}"#), None);
    }

    #[test]
    fn unclosed_object_returns_brace() {
        assert_eq!(missing_closers(r#"{"a":1"#).as_deref(), Some("}"));
    }

    #[test]
    fn unclosed_array_returns_bracket() {
        assert_eq!(missing_closers("[1, 2").as_deref(), Some("]"));
    }

    #[test]
    fn nested_returns_innermost_first() {
        // {"a":[1,2 -> close array first, then object: ]}
        assert_eq!(missing_closers(r#"{"a":[1,2"#).as_deref(), Some("]}"));
    }

    #[test]
    fn string_with_brace_is_ignored() {
        // The `{` inside the string literal shouldn't count
        assert_eq!(missing_closers(r#"{"a":"{"#), None);
    }

    #[test]
    fn escaped_quote_in_string_doesnt_end_string() {
        // {"a":"\"" -> the escaped " doesn't close the string, real "
        // does. Overall braces balance
        assert_eq!(missing_closers(r#"{"a":"\""}"#), None);
    }

    #[test]
    fn mismatched_close_returns_none() {
        // `{]` is a typo, not something we should try to "complete"
        assert_eq!(missing_closers("{]"), None);
    }

    #[test]
    fn stray_close_returns_none() {
        assert_eq!(missing_closers("}"), None);
    }

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = InputAutocompleteProvider;
        assert!(p.query(&Query::new(r#"{"a":1"#)).await.is_empty());
    }

    #[tokio::test]
    async fn json_keyword_with_balanced_input_silent() {
        let p = InputAutocompleteProvider;
        assert!(p.query(&Query::new(r#"json {"a":1}"#)).await.is_empty());
    }

    #[tokio::test]
    async fn json_keyword_with_unclosed_emits_hint() {
        let p = InputAutocompleteProvider;
        let out = p.query(&Query::new(r#"json {"a":1"#)).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("autoclose::"));
        // Title is the full balanced input so precision ranking lifts
        // it above sibling bypass_rank rows (e.g. "Invalid JSON")
        assert_eq!(out[0].title, r#"json {"a":1}"#);
        let sub = out[0].subtitle.as_deref().unwrap();
        assert!(sub.contains("Close brackets"));
    }

    #[tokio::test]
    async fn id_routes_to_this_provider_not_hints() {
        // REGRESSION: previous id was `hint::autoclose::...` which the
        // registry's `::`-prefix dispatch sent to CommandHintsProvider,
        // producing a `SetInput("autoclose::... ")` that splatted the
        // internal prefix into user's query. The fix keeps id
        // routing tied to this provider's own id
        let p = InputAutocompleteProvider;
        let out = p.query(&Query::new("json {")).await;
        assert!(out[0].id.starts_with("autoclose::"));
    }

    #[tokio::test]
    async fn activate_emits_setinput_with_closed_form() {
        let p = InputAutocompleteProvider;
        let out = p.query(&Query::new(r#"json {"a":1"#)).await;
        match p.activate(&out[0].id, "default").await.unwrap() {
            Effect::SetInput(s) => assert_eq!(s, r#"json {"a":1}"#),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn format_conversion_keyword_gets_closure_too() {
        let p = InputAutocompleteProvider;
        let out = p.query(&Query::new(r#"json2toml {"a":[1,2"#)).await;
        assert_eq!(out.len(), 1);
        match p.activate(&out[0].id, "default").await.unwrap() {
            Effect::SetInput(s) => assert_eq!(s, r#"json2toml {"a":[1,2]}"#),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn whitespace_only_tail_no_hint() {
        let p = InputAutocompleteProvider;
        assert!(p.query(&Query::new("json   ")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = InputAutocompleteProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }
}
