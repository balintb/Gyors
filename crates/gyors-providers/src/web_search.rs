//! Web search provider. Keyword-triggered shortcuts that open a search URL
//! in default browser
//!
//! `<keyword> <query>` (where `<keyword>` is the first whitespace-separated
//! token) produces a "Search <engine> for ..." candidate whose activation
//! emits `Effect::OpenUrl`

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct WebSearchProvider;

struct Engine {
    keyword: &'static str,
    label: &'static str,
    url_template: &'static str,
    symbol: &'static str,
}

const ENGINES: &[Engine] = &[
    Engine { keyword: "g", label: "Google", url_template: "https://www.google.com/search?q=%s", symbol: "magnifyingglass" },
    Engine { keyword: "ddg", label: "DuckDuckGo", url_template: "https://duckduckgo.com/?q=%s", symbol: "magnifyingglass.circle" },
    Engine { keyword: "gh", label: "GitHub", url_template: "https://github.com/search?q=%s&type=code", symbol: "chevron.left.forwardslash.chevron.right" },
    Engine { keyword: "so", label: "Stack Overflow", url_template: "https://stackoverflow.com/search?q=%s", symbol: "questionmark.circle" },
    Engine { keyword: "yt", label: "YouTube", url_template: "https://www.youtube.com/results?search_query=%s", symbol: "play.rectangle.fill" },
    Engine { keyword: "npm", label: "npm", url_template: "https://www.npmjs.com/search?q=%s", symbol: "shippingbox.fill" },
    Engine { keyword: "w", label: "Wikipedia", url_template: "https://en.wikipedia.org/wiki/Special:Search?search=%s", symbol: "book.fill" },
    Engine { keyword: "docs", label: "docs.rs", url_template: "https://docs.rs/releases/search?query=%s", symbol: "shield.fill" },
];

#[async_trait]
impl Provider for WebSearchProvider {
    fn id(&self) -> &str {
        "web"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some((keyword, rest)) = query.pattern().split_once(char::is_whitespace) else {
            return vec![];
        };
        let q = rest.trim();
        if q.is_empty() {
            return vec![];
        }
        let Some(engine) = ENGINES.iter().find(|e| e.keyword == keyword) else {
            return vec![];
        };
        let encoded = urlencoding::encode(q);
        let url = engine.url_template.replace("%s", &encoded);
        vec![make_candidate(engine, q, url)]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let url = id
            .strip_prefix("web::")
            .ok_or_else(|| anyhow::anyhow!("invalid web candidate id: {id}"))?;
        Ok(Effect::OpenUrl(url.to_string()))
    }
}

fn make_candidate(engine: &Engine, query: &str, url: String) -> Candidate {
    Candidate {
        id: format!("web::{url}"),
        title: format!("Search {} for \"{}\"", engine.label, query),
        subtitle: Some(url),
        icon: Icon::SfSymbol(engine.symbol.into()),
        kind: CandidateKind::Web,
        actions: vec![Action::primary("Open in Browser")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_keyword_no_candidate() {
        let p = WebSearchProvider;
        assert!(p.query(&Query::new("")).await.is_empty());
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn unknown_keyword_no_candidate() {
        let p = WebSearchProvider;
        assert!(p.query(&Query::new("xxx foo bar")).await.is_empty());
    }

    #[tokio::test]
    async fn keyword_with_no_query_no_candidate() {
        let p = WebSearchProvider;
        // Just "g " with trailing space counts as empty query
        assert!(p.query(&Query::new("g ")).await.is_empty());
        assert!(p.query(&Query::new("g   ")).await.is_empty());
    }

    #[tokio::test]
    async fn google_search() {
        let p = WebSearchProvider;
        let out = p.query(&Query::new("g rust async")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Search Google for \"rust async\"");
        assert_eq!(out[0].kind, CandidateKind::Web);
        assert!(out[0].bypass_rank);
        let url = out[0].subtitle.as_ref().unwrap();
        assert!(url.starts_with("https://www.google.com/search?q="));
        assert!(url.contains("rust"));
    }

    #[tokio::test]
    async fn special_chars_url_encoded() {
        let p = WebSearchProvider;
        let out = p.query(&Query::new("g rust & async?")).await;
        let url = out[0].subtitle.as_ref().unwrap();
        // '&' -> %26, '?' -> %3F, space -> %20
        assert!(url.contains("%26"));
        assert!(url.contains("%3F"));
        assert!(url.contains("%20"));
    }

    #[tokio::test]
    async fn github_keyword() {
        let p = WebSearchProvider;
        let out = p.query(&Query::new("gh swift-bridge")).await;
        assert_eq!(out.len(), 1);
        let url = out[0].subtitle.as_ref().unwrap();
        assert!(url.starts_with("https://github.com/search?q="));
    }

    #[tokio::test]
    async fn all_engines_produce_results() {
        let p = WebSearchProvider;
        for engine in ENGINES {
            let q = format!("{} hello", engine.keyword);
            let out = p.query(&Query::new(&q)).await;
            assert_eq!(out.len(), 1, "engine {} produced no result", engine.keyword);
        }
    }

    #[tokio::test]
    async fn activate_emits_openurl() {
        let p = WebSearchProvider;
        let out = p.query(&Query::new("g hi")).await;
        let id = out[0].id.clone();
        let effect = p.activate(&id, "default").await.unwrap();
        match effect {
            Effect::OpenUrl(u) => assert!(u.contains("google.com")),
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_rejects_foreign_id() {
        let p = WebSearchProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }
}
