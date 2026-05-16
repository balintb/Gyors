//! Small dev-utility generators
//!
//!   uuid       -> new UUIDv4
//!   uuid7      -> new UUIDv7 (time-ordered)
//!   passw      -> 20-char random password
//!   passw <n>  -> n-char random password (clamped to 4..=128)

use async_trait::async_trait;
use rand::Rng;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct GeneratorsProvider;

const PASSWORD_CHARSET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()-_=+[]{}";

const MIN_PASSWORD_LEN: usize = 4;
const MAX_PASSWORD_LEN: usize = 128;
const DEFAULT_PASSWORD_LEN: usize = 20;

#[async_trait]
impl Provider for GeneratorsProvider {
    fn id(&self) -> &str {
        "gen"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        match pattern {
            "uuid" | "uuid4" => return uuid_candidates(false),
            "uuid7" | "uuidv7" => return uuid_candidates(true),
            "passw" | "password" => return password_candidates(DEFAULT_PASSWORD_LEN),
            "lorem" => return lorem_candidates(1),
            _ => {}
        }
        if let Some(rest) = pattern
            .strip_prefix("passw ")
            .or_else(|| pattern.strip_prefix("password "))
        {
            if let Ok(len) = rest.trim().parse::<usize>() {
                return password_candidates(len.clamp(MIN_PASSWORD_LEN, MAX_PASSWORD_LEN));
            }
        }
        if let Some(rest) = pattern.strip_prefix("lorem ") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                return lorem_candidates(n.clamp(1, 20));
            }
        }
        vec![]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("gen::")
            .ok_or_else(|| anyhow::anyhow!("invalid gen candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

fn uuid_candidates(v7: bool) -> Vec<Candidate> {
    let uuid = if v7 {
        uuid::Uuid::now_v7()
    } else {
        uuid::Uuid::new_v4()
    };
    let s = uuid.to_string();
    let label = if v7 { "UUIDv7 (time-ordered)" } else { "UUIDv4 (random)" };
    vec![make_candidate(&s, label, "barcode.viewfinder")]
}

fn password_candidates(len: usize) -> Vec<Candidate> {
    let mut rng = rand::thread_rng();
    let s: String = (0..len)
        .map(|_| PASSWORD_CHARSET[rng.gen_range(0..PASSWORD_CHARSET.len())] as char)
        .collect();
    vec![make_candidate(
        &s,
        &format!("{len}-char random password"),
        "key.horizontal.fill",
    )]
}

const LOREM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

fn lorem_candidates(paragraphs: usize) -> Vec<Candidate> {
    let text = std::iter::repeat(LOREM)
        .take(paragraphs)
        .collect::<Vec<_>>()
        .join("\n\n");
    vec![make_candidate(&text, &format!("{paragraphs} paragraph{} of Lorem ipsum", if paragraphs == 1 { "" } else { "s" }), "text.alignleft")]
}

fn make_candidate(value: &str, label: &str, symbol: &str) -> Candidate {
    Candidate {
        id: format!("gen::{value}"),
        title: value.to_string(),
        subtitle: Some(label.to_string()),
        icon: Icon::SfSymbol(symbol.into()),
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
    async fn no_match_without_keyword() {
        let p = GeneratorsProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn uuid_yields_valid_uuid() {
        let p = GeneratorsProvider;
        let out = p.query(&Query::new("uuid")).await;
        assert_eq!(out.len(), 1);
        let uuid_str = out[0].title.clone();
        assert!(uuid::Uuid::parse_str(&uuid_str).is_ok());
    }

    #[tokio::test]
    async fn uuid_aliases_work() {
        let p = GeneratorsProvider;
        assert_eq!(p.query(&Query::new("uuid4")).await.len(), 1);
        assert_eq!(p.query(&Query::new("uuid7")).await.len(), 1);
        assert_eq!(p.query(&Query::new("uuidv7")).await.len(), 1);
    }

    #[tokio::test]
    async fn uuid_is_fresh_each_call() {
        let p = GeneratorsProvider;
        let a = p.query(&Query::new("uuid")).await;
        let b = p.query(&Query::new("uuid")).await;
        assert_ne!(a[0].title, b[0].title);
    }

    #[tokio::test]
    async fn default_password_length() {
        let p = GeneratorsProvider;
        let out = p.query(&Query::new("passw")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title.chars().count(), DEFAULT_PASSWORD_LEN);
    }

    #[tokio::test]
    async fn explicit_password_length() {
        let p = GeneratorsProvider;
        let out = p.query(&Query::new("passw 32")).await;
        assert_eq!(out[0].title.chars().count(), 32);
    }

    #[tokio::test]
    async fn password_length_clamped() {
        let p = GeneratorsProvider;
        let too_short = p.query(&Query::new("passw 1")).await;
        assert_eq!(too_short[0].title.chars().count(), MIN_PASSWORD_LEN);
        let too_long = p.query(&Query::new("passw 10000")).await;
        assert_eq!(too_long[0].title.chars().count(), MAX_PASSWORD_LEN);
    }

    #[tokio::test]
    async fn password_non_numeric_length_no_results() {
        let p = GeneratorsProvider;
        assert!(p.query(&Query::new("passw abc")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_copies_value() {
        let p = GeneratorsProvider;
        let out = p.query(&Query::new("uuid")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert!(uuid::Uuid::parse_str(&s).is_ok()),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lorem_default_one_paragraph() {
        let p = GeneratorsProvider;
        let out = p.query(&Query::new("lorem")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.to_lowercase().starts_with("lorem ipsum"));
        // One paragraph = no blank-line separators
        assert!(!out[0].title.contains("\n\n"));
    }

    #[tokio::test]
    async fn lorem_multi_paragraph() {
        let p = GeneratorsProvider;
        let out = p.query(&Query::new("lorem 3")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title.matches("\n\n").count(), 2);
    }

    #[tokio::test]
    async fn lorem_clamps_count() {
        let p = GeneratorsProvider;
        assert_eq!(p.query(&Query::new("lorem 0")).await[0].title.matches("\n\n").count(), 0);
        assert_eq!(p.query(&Query::new("lorem 500")).await[0].title.matches("\n\n").count(), 19);
    }

    #[tokio::test]
    async fn lorem_non_numeric_empty() {
        let p = GeneratorsProvider;
        assert!(p.query(&Query::new("lorem abc")).await.is_empty());
    }
}
