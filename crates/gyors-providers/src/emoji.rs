//! Emoji picker. Triggered by `emoji <search>` or the shorthand `:<search>`
//!
//! Uses the bundled `emojis` crate for a complete Unicode emoji database
//! (~3600 entries). Matches against both `name()` and `shortcodes()`.
//! Activation copies the emoji character to clipboard

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct EmojiProvider;

const RESULT_LIMIT: usize = 15;

/// A few sensible defaults when user types just `emoji` with no filter
const DEFAULT_EMOJIS: &[&str] = &[
    "😀", "❤️", "👍", "🎉", "🔥", "💯", "✅", "❓", "🚀", "🙏",
];

#[async_trait]
impl Provider for EmojiProvider {
    fn id(&self) -> &str {
        "emoji"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(filter) = parse_emoji_query(pattern) else {
            return vec![];
        };
        if filter.is_empty() {
            return DEFAULT_EMOJIS
                .iter()
                .filter_map(|c| emojis::get(c))
                .map(to_candidate)
                .collect();
        }
        let filter_lower = filter.to_lowercase();
        emojis::iter()
            .filter(|e| emoji_matches(e, &filter_lower))
            .take(RESULT_LIMIT)
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let emoji = id
            .strip_prefix("emoji::")
            .ok_or_else(|| anyhow::anyhow!("invalid emoji candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(emoji.to_string()))
    }
}

/// Parse input to the effective emoji filter. Returns None when the
/// user hasn't invoked the emoji provider
fn parse_emoji_query(pattern: &str) -> Option<&str> {
    // `emoji` keyword, possibly with a filter
    if pattern == "emoji" {
        return Some("");
    }
    if let Some(rest) = pattern.strip_prefix("emoji ") {
        return Some(rest.trim());
    }
    // `:<alpha>` shorthand - avoids colliding with arbitrary user input
    // by requiring an alphabetic first character after the colon
    if let Some(rest) = pattern.strip_prefix(':') {
        let first_alpha = rest.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false);
        if first_alpha {
            return Some(rest.trim());
        }
    }
    None
}

fn emoji_matches(e: &emojis::Emoji, filter_lower: &str) -> bool {
    if e.name().to_lowercase().contains(filter_lower) {
        return true;
    }
    e.shortcodes()
        .any(|sc| sc.to_lowercase().contains(filter_lower))
}

fn to_candidate(e: &emojis::Emoji) -> Candidate {
    let shortcode = e.shortcodes().next();
    let subtitle = shortcode.map(|s| format!(":{s}:"));
    Candidate {
        id: format!("emoji::{}", e.as_str()),
        title: e.name().to_string(),
        subtitle,
        icon: Icon::Glyph(e.as_str().to_string()),
        kind: CandidateKind::Snippet,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_outside_keyword() {
        let p = EmojiProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
        assert!(p.query(&Query::new("")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_shows_defaults() {
        let p = EmojiProvider;
        let out = p.query(&Query::new("emoji")).await;
        assert!(!out.is_empty());
        assert!(out.len() <= DEFAULT_EMOJIS.len());
    }

    #[tokio::test]
    async fn filter_by_name() {
        let p = EmojiProvider;
        let out = p.query(&Query::new("emoji rocket")).await;
        assert!(!out.is_empty());
        // Emoji char now lives in the icon, not title
        assert!(out.iter().any(|c| c.id == "emoji::🚀"));
    }

    #[tokio::test]
    async fn colon_shorthand() {
        let p = EmojiProvider;
        let out = p.query(&Query::new(":rocket")).await;
        assert!(!out.is_empty());
        assert!(out.iter().any(|c| c.id == "emoji::🚀"));
    }

    #[tokio::test]
    async fn icon_is_glyph_containing_emoji_char() {
        let p = EmojiProvider;
        let out = p.query(&Query::new("emoji rocket")).await;
        let rocket = out.iter().find(|c| c.id == "emoji::🚀").unwrap();
        match &rocket.icon {
            Icon::Glyph(s) => assert_eq!(s, "🚀"),
            other => panic!("expected Glyph, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn colon_only_alphabetic() {
        // ":1" or ":(" shouldn't trigger - user probably meant something else
        let p = EmojiProvider;
        assert!(p.query(&Query::new(":1")).await.is_empty());
        assert!(p.query(&Query::new(":(")).await.is_empty());
    }

    #[tokio::test]
    async fn results_limited() {
        let p = EmojiProvider;
        let out = p.query(&Query::new("emoji face")).await;
        assert!(out.len() <= RESULT_LIMIT);
    }

    #[tokio::test]
    async fn activate_returns_emoji_char() {
        let p = EmojiProvider;
        let out = p.query(&Query::new("emoji rocket")).await;
        let id = out.iter().find(|c| c.id == "emoji::🚀").unwrap().id.clone();
        let effect = p.activate(&id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "🚀"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = EmojiProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn parse_emoji_query_cases() {
        assert_eq!(parse_emoji_query("emoji"), Some(""));
        assert_eq!(parse_emoji_query("emoji cat"), Some("cat"));
        assert_eq!(parse_emoji_query(":smile"), Some("smile"));
        assert_eq!(parse_emoji_query("hello"), None);
        assert_eq!(parse_emoji_query(""), None);
        assert_eq!(parse_emoji_query(":"), None);
        assert_eq!(parse_emoji_query(":123"), None);
    }

    #[test]
    fn case_insensitive_matching() {
        let e = emojis::get("🚀").unwrap();
        assert!(emoji_matches(e, "rocket"));
        assert!(emoji_matches(e, "ROCKET".to_lowercase().as_str()));
    }
}
