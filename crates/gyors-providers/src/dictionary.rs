//! Dictionary / definitions. `def <word>` opens the macOS Dictionary app
//! for that word via the `dict://` URL scheme

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct DictionaryProvider;

#[async_trait]
impl Provider for DictionaryProvider {
    fn id(&self) -> &str {
        "def"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(rest) = query.pattern().strip_prefix("def ") else {
            return vec![];
        };
        let word = rest.trim();
        if word.is_empty() {
            return vec![];
        }
        vec![Candidate {
            id: format!("def::{word}"),
            title: format!("Define: {word}"),
            subtitle: Some("Look up in macOS Dictionary".into()),
            icon: Icon::SfSymbol("book.fill".into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("Look up")],
            search_text: String::new(),
            bypass_rank: true,
        }]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let word = id
            .strip_prefix("def::")
            .ok_or_else(|| anyhow::anyhow!("invalid def candidate id: {id}"))?;
        let encoded = urlencoding::encode(word);
        Ok(Effect::OpenUrl(format!("dict://{encoded}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = DictionaryProvider;
        assert!(p.query(&Query::new("serendipity")).await.is_empty());
    }

    #[tokio::test]
    async fn def_keyword_with_no_word_empty() {
        let p = DictionaryProvider;
        assert!(p.query(&Query::new("def ")).await.is_empty());
        assert!(p.query(&Query::new("def")).await.is_empty());
    }

    #[tokio::test]
    async fn def_produces_candidate() {
        let p = DictionaryProvider;
        let out = p.query(&Query::new("def serendipity")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Define: serendipity");
        assert_eq!(out[0].kind, CandidateKind::Action);
        assert!(out[0].bypass_rank);
    }

    #[tokio::test]
    async fn activate_opens_dict_url() {
        let p = DictionaryProvider;
        let out = p.query(&Query::new("def rust")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::OpenUrl(u) => {
                assert!(u.starts_with("dict://"));
                assert!(u.contains("rust"));
            }
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_url_encodes_word() {
        let p = DictionaryProvider;
        let out = p.query(&Query::new("def two words")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        if let Effect::OpenUrl(u) = effect {
            // Space should be %20
            assert!(u.contains("%20"), "got {u}");
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = DictionaryProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }
}
