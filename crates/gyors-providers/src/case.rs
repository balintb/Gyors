//! Case converter - `case [kind] <text>`
//!
//! Kinds: `camel`, `pascal` (aliased `upper-camel`), `snake`, `kebab`,
//! `constant` (aliased `shouty-snake`), `title`, `upper`, `lower`, `slug`
//!
//! `slug` produces a URL-friendly slug: lowercase, ASCII-alphanumeric only,
//! single-hyphen separators, no leading/trailing hyphens
//!
//! With no kind specified, all conversions are listed so user can pick

use async_trait::async_trait;
use heck::{
    ToKebabCase, ToLowerCamelCase, ToShoutySnakeCase, ToSnakeCase, ToTitleCase, ToUpperCamelCase,
};
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct CaseProvider;

#[async_trait]
impl Provider for CaseProvider {
    fn id(&self) -> &str {
        "case"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(rest) = pattern.strip_prefix("case ") else {
            return vec![];
        };
        let rest = rest.trim();
        if rest.is_empty() {
            return vec![];
        }
        // `case <kind>` with no text -> no output (waiting for input)
        if is_known_kind(rest) {
            return vec![];
        }

        if let Some((first, text)) = rest.split_once(char::is_whitespace) {
            if is_known_kind(first) {
                let text = text.trim();
                if text.is_empty() {
                    return vec![];
                }
                if let Some(converted) = apply_case(first, text) {
                    return vec![make_candidate(first, &converted)];
                }
            }
        }
        // No explicit kind (or first token isn't one) - show all conversions
        // of entire remainder
        all_conversions(rest)
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let rest = id
            .strip_prefix("case::")
            .ok_or_else(|| anyhow::anyhow!("invalid case candidate id: {id}"))?;
        let (_kind, value) = rest
            .split_once("::")
            .ok_or_else(|| anyhow::anyhow!("malformed case id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

const KINDS: &[&str] = &[
    "camel", "pascal", "snake", "kebab", "constant", "title", "upper", "lower", "slug",
];

fn is_known_kind(s: &str) -> bool {
    matches!(
        s,
        "camel"
            | "pascal"
            | "upper-camel"
            | "snake"
            | "kebab"
            | "constant"
            | "shouty-snake"
            | "title"
            | "upper"
            | "lower"
            | "slug"
    )
}

fn apply_case(kind: &str, input: &str) -> Option<String> {
    match kind {
        "camel" => Some(input.to_lower_camel_case()),
        "pascal" | "upper-camel" => Some(input.to_upper_camel_case()),
        "snake" => Some(input.to_snake_case()),
        "kebab" => Some(input.to_kebab_case()),
        "constant" | "shouty-snake" => Some(input.to_shouty_snake_case()),
        "title" => Some(input.to_title_case()),
        "upper" => Some(input.to_uppercase()),
        "lower" => Some(input.to_lowercase()),
        "slug" => Some(slugify(input)),
        _ => None,
    }
}

/// URL-safe slug: lowercase ASCII alphanumerics joined by single hyphens.
/// Non-ASCII characters are dropped (no transliteration); use `kebab` if
/// you need to preserve Unicode word boundaries
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_hyphen = true;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn all_conversions(text: &str) -> Vec<Candidate> {
    KINDS
        .iter()
        .filter_map(|k| apply_case(k, text).map(|v| (*k, v)))
        .map(|(k, v)| make_candidate(k, &v))
        .collect()
}

fn make_candidate(kind: &str, value: &str) -> Candidate {
    Candidate {
        id: format!("case::{kind}::{value}"),
        title: value.to_string(),
        subtitle: Some(format!("{kind} case")),
        icon: Icon::SfSymbol("textformat".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_case_keyword() {
        let p = CaseProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_case_with_empty_text_empty() {
        let p = CaseProvider;
        assert!(p.query(&Query::new("case ")).await.is_empty());
        assert!(p.query(&Query::new("case    ")).await.is_empty());
    }

    #[tokio::test]
    async fn all_conversions_when_no_kind_given() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case hello world")).await;
        assert_eq!(out.len(), KINDS.len());
        let titles: Vec<_> = out.iter().map(|c| c.title.clone()).collect();
        assert!(titles.contains(&"helloWorld".to_string()));
        assert!(titles.contains(&"HelloWorld".to_string()));
        assert!(titles.contains(&"hello_world".to_string()));
        assert!(titles.contains(&"hello-world".to_string()));
        assert!(titles.contains(&"HELLO_WORLD".to_string()));
    }

    #[tokio::test]
    async fn explicit_kind_returns_one() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case snake Hello World")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "hello_world");
    }

    #[tokio::test]
    async fn pascal_and_upper_camel_equivalent() {
        let p = CaseProvider;
        let a = p.query(&Query::new("case pascal hello world")).await;
        let b = p.query(&Query::new("case upper-camel hello world")).await;
        assert_eq!(a[0].title, b[0].title);
    }

    #[tokio::test]
    async fn kebab_case() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case kebab HelloWorldThing")).await;
        assert_eq!(out[0].title, "hello-world-thing");
    }

    #[tokio::test]
    async fn upper_preserves_words() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case upper hello world")).await;
        assert_eq!(out[0].title, "HELLO WORLD");
    }

    #[tokio::test]
    async fn lower_preserves_words() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case lower HELLO WORLD")).await;
        assert_eq!(out[0].title, "hello world");
    }

    #[tokio::test]
    async fn unknown_kind_falls_back_to_text() {
        let p = CaseProvider;
        // "hello" isn't a known kind, so whole remainder is the text -> all conversions
        let out = p.query(&Query::new("case hello world")).await;
        assert_eq!(out.len(), KINDS.len());
    }

    #[tokio::test]
    async fn explicit_kind_with_no_text_empty() {
        let p = CaseProvider;
        assert!(p.query(&Query::new("case snake ")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_copies_value() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case snake HelloWorld")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "hello_world"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_works_for_each_kind() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case hello world")).await;
        for c in out {
            let effect = p.activate(&c.id, "default").await.unwrap();
            match effect {
                Effect::CopyToClipboard(s) => assert_eq!(s, c.title),
                other => panic!("expected CopyToClipboard, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = CaseProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn is_known_kind_table() {
        for k in [
            "camel",
            "pascal",
            "upper-camel",
            "snake",
            "kebab",
            "constant",
            "shouty-snake",
            "title",
            "upper",
            "lower",
            "slug",
        ] {
            assert!(is_known_kind(k), "expected {} to be a known kind", k);
        }
        assert!(!is_known_kind(""));
        assert!(!is_known_kind("random"));
        assert!(!is_known_kind("Pascal")); // case-sensitive for simplicity
    }

    #[test]
    fn apply_case_table() {
        assert_eq!(apply_case("camel", "hello world").as_deref(), Some("helloWorld"));
        assert_eq!(apply_case("snake", "HelloWorld").as_deref(), Some("hello_world"));
        assert_eq!(apply_case("upper", "hi").as_deref(), Some("HI"));
        assert_eq!(apply_case("slug", "Hello, World!").as_deref(), Some("hello-world"));
        assert_eq!(apply_case("bogus", "x"), None);
    }

    #[tokio::test]
    async fn slug_basic() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case slug Hello, World!")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "hello-world");
    }

    #[tokio::test]
    async fn slug_collapses_runs_of_separators() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case slug  Foo   --  Bar  ")).await;
        assert_eq!(out[0].title, "foo-bar");
    }

    #[tokio::test]
    async fn slug_drops_non_ascii() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case slug Café au lait")).await;
        // accented chars get dropped, leaves "caf-au-lait" - not "cafe-au-lait"
        assert_eq!(out[0].title, "caf-au-lait");
    }

    #[tokio::test]
    async fn slug_trims_leading_and_trailing_separators() {
        let p = CaseProvider;
        let out = p.query(&Query::new("case slug --hello world--")).await;
        assert_eq!(out[0].title, "hello-world");
    }

    #[test]
    fn slugify_pure_table() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("a"), "a");
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("AAA   BBB"), "aaa-bbb");
        assert_eq!(slugify("123 numbers"), "123-numbers");
        assert_eq!(slugify("!@#$%^"), "");
        assert_eq!(slugify("---x---"), "x");
    }

    #[tokio::test]
    async fn slug_included_in_all_conversions() {
        // When no kind is given, slug is one of the rendered options
        let p = CaseProvider;
        let out = p.query(&Query::new("case Hello World!")).await;
        assert!(out.iter().any(|c| c.title == "hello-world"));
    }
}
