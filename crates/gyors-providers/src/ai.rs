//! AI query provider. Surfaces a single "Ask AI: <question>" candidate when
//! user types `ai <q>` or `ask <q>`. Activation dispatches
//! `Effect::AskAi(question)` - Swift shell handles actual HTTP
//! call via its `AiClient` (Ollama / OpenAI / Anthropic per `config.json`)

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct AiProvider;

const MAX_TITLE_LEN: usize = 80;

#[async_trait]
impl Provider for AiProvider {
    fn id(&self) -> &str {
        "ai"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(question) = parse_ai_query(query.pattern()) else {
            return vec![];
        };
        if question.is_empty() {
            return vec![];
        }
        vec![Candidate {
            id: format!("ai::{question}"),
            title: format!("Ask AI: {}", truncate(question, MAX_TITLE_LEN)),
            subtitle: Some("Send to your configured AI provider".into()),
            icon: Icon::SfSymbol("sparkles".into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("Ask")],
            search_text: String::new(),
            bypass_rank: true,
        }]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let question = id
            .strip_prefix("ai::")
            .ok_or_else(|| anyhow::anyhow!("invalid ai candidate id: {id}"))?;
        Ok(Effect::AskAi(question.to_string()))
    }
}

fn parse_ai_query(pattern: &str) -> Option<&str> {
    if let Some(rest) = pattern.strip_prefix("ai ") {
        return Some(rest.trim());
    }
    if let Some(rest) = pattern.strip_prefix("ask ") {
        return Some(rest.trim());
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.into()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = AiProvider;
        assert!(p.query(&Query::new("what is love")).await.is_empty());
    }

    #[tokio::test]
    async fn ai_keyword_with_empty_question_no_match() {
        let p = AiProvider;
        assert!(p.query(&Query::new("ai ")).await.is_empty());
        assert!(p.query(&Query::new("ask   ")).await.is_empty());
    }

    #[tokio::test]
    async fn ai_keyword_produces_candidate() {
        let p = AiProvider;
        let out = p.query(&Query::new("ai why is the sky blue")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.starts_with("Ask AI:"));
        assert!(out[0].bypass_rank);
    }

    #[tokio::test]
    async fn ask_keyword_equivalent() {
        let p = AiProvider;
        let a = p.query(&Query::new("ai hello")).await;
        let b = p.query(&Query::new("ask hello")).await;
        assert_eq!(a[0].title, b[0].title);
    }

    #[tokio::test]
    async fn activate_yields_askai_effect() {
        let p = AiProvider;
        let out = p.query(&Query::new("ai tell me a joke")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::AskAi(q) => assert_eq!(q, "tell me a joke"),
            other => panic!("expected AskAi, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = AiProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn parse_ai_query_cases() {
        assert_eq!(parse_ai_query("ai hello"), Some("hello"));
        assert_eq!(parse_ai_query("ask hello"), Some("hello"));
        assert_eq!(parse_ai_query("ai   trimmed   "), Some("trimmed"));
        assert_eq!(parse_ai_query("aim"), None); // "aim " would match; "aim" doesn't
        assert_eq!(parse_ai_query(""), None);
        assert_eq!(parse_ai_query("hello"), None);
    }
}
