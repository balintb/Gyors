//! JSON validation + pretty-printing. Keyword `json <text>`
//!
//! - Valid input -> two candidates: pretty-printed (default action copies
//!   it) and minified.
//! - Invalid input -> a single candidate showing parse error

use crate::loose_json;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct JsonProvider;

#[async_trait]
impl Provider for JsonProvider {
    fn id(&self) -> &str {
        "json"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(input) = strip_keyword(pattern) else {
            return vec![];
        };
        let input = input.trim();
        if input.is_empty() {
            return vec![];
        }

        // Permissive parse: strict JSON first, then JS-style loose
        // (unquoted keys, single-quoted strings, trailing commas).
        // Lets user type `json {a:1, b:'hi'}` without shifting
        // into quote-hunting mode
        match loose_json::parse_permissive(input) {
            Ok(value) => {
                let pretty =
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| input.to_string());
                let minified = serde_json::to_string(&value).unwrap_or_else(|_| input.to_string());
                vec![
                    candidate(
                        &format!("json::pretty::{pretty}"),
                        "Pretty-printed JSON",
                        &format!("↵ copy · → preview · {}", format_preview(&pretty)),
                        "curlybraces",
                        "Copy Pretty",
                        true,
                    ),
                    candidate(
                        &format!("json::min::{minified}"),
                        "Minified JSON",
                        &format!("↵ copy · → preview · {}", format_preview(&minified)),
                        "square.stack.3d.down.right",
                        "Copy Minified",
                        true,
                    ),
                ]
            }
            Err(e) => vec![candidate(
                "json::error",
                "Invalid JSON",
                &e.to_string(),
                "exclamationmark.triangle.fill",
                "Copy Error",
                false,
            )],
        }
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> anyhow::Result<Effect> {
        if id == "json::error" {
            return Ok(Effect::None);
        }
        let (value, label) = if let Some(v) = id.strip_prefix("json::pretty::") {
            (v, "Pretty JSON")
        } else if let Some(v) = id.strip_prefix("json::min::") {
            (v, "Minified JSON")
        } else {
            anyhow::bail!("invalid json candidate id: {id}");
        };
        match action {
            "default" => Ok(Effect::CopyToClipboard(value.to_string())),
            "preview" => Ok(Effect::ShowText {
                text: value.to_string(),
                label: label.to_string(),
                language: Some("json".into()),
                editable_path: None,
            }),
            other => anyhow::bail!("unknown action for json: {other}"),
        }
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    s.strip_prefix("json ").or_else(|| s.strip_prefix("jq "))
}

fn format_preview(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect();
    if flat.chars().count() > 120 {
        let head: String = flat.chars().take(120).collect();
        format!("{head}…")
    } else {
        flat
    }
}

fn candidate(
    id: &str,
    title: &str,
    subtitle: &str,
    symbol: &str,
    action_label: &str,
    previewable: bool,
) -> Candidate {
    let mut actions = vec![Action::primary(action_label)];
    if previewable {
        actions.push(Action::new("preview", "Preview"));
    }
    Candidate {
        id: id.into(),
        title: title.into(),
        subtitle: Some(subtitle.into()),
        icon: Icon::SfSymbol(symbol.into()),
        kind: CandidateKind::Action,
        actions,
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = JsonProvider;
        assert!(p.query(&Query::new("hello world")).await.is_empty());
    }

    #[tokio::test]
    async fn keyword_with_no_input_empty() {
        let p = JsonProvider;
        assert!(p.query(&Query::new("json ")).await.is_empty());
    }

    #[tokio::test]
    async fn valid_json_yields_pretty_and_min() {
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"json {"a":1}"#)).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "Pretty-printed JSON");
        assert_eq!(out[1].title, "Minified JSON");
    }

    #[tokio::test]
    async fn invalid_json_shows_error() {
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"json {"a":}"#)).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "json::error");
        assert!(out[0].title.contains("Invalid"));
    }

    #[tokio::test]
    async fn jq_alias_also_works() {
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"jq [1,2,3]"#)).await;
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn pretty_contains_newlines_but_subtitle_is_flat() {
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"json {"a":1,"b":2}"#)).await;
        let pretty = &out[0];
        assert!(pretty.id.contains('\n'), "id has the raw pretty JSON");
        let sub = pretty.subtitle.as_deref().unwrap();
        assert!(!sub.contains('\n'), "subtitle flattened");
    }

    #[tokio::test]
    async fn activate_pretty_copies_formatted_output() {
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"json {"a":1}"#)).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => {
                assert!(s.contains('\n'), "pretty output has newlines: {s:?}");
                assert!(s.contains("\"a\""));
            }
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_min_copies_compact_output() {
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"json {"a":1}"#)).await;
        let eff = p.activate(&out[1].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => {
                assert!(!s.contains('\n'), "compact: {s:?}");
                assert_eq!(s, r#"{"a":1}"#);
            }
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_error_is_noop() {
        let p = JsonProvider;
        let eff = p
            .activate(&"json::error".to_string(), "default")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::None));
    }

    #[tokio::test]
    async fn activate_unknown_prefix_errors() {
        let p = JsonProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_preview_shows_text_inline() {
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"json {"a":1}"#)).await;
        let eff = p.activate(&out[0].id, "preview").await.unwrap();
        match eff {
            Effect::ShowText {
                text,
                label,
                language,
                editable_path,
            } => {
                assert!(text.contains('\n'));
                assert_eq!(label, "Pretty JSON");
                assert_eq!(language.as_deref(), Some("json"));
                assert!(editable_path.is_none(), "json previews aren't editable");
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn candidates_expose_preview_action() {
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"json {"a":1}"#)).await;
        for c in &out {
            assert!(
                c.actions.iter().any(|a| a.id == "preview"),
                "candidate {:?} missing preview action",
                c.id
            );
        }
    }

    #[tokio::test]
    async fn error_candidate_has_no_preview_action() {
        // Previewing a parse error makes no sense - title already
        // shows "Invalid JSON" and the subtitle carries error text
        let p = JsonProvider;
        let out = p.query(&Query::new(r#"json {"a":}"#)).await;
        assert!(!out[0].actions.iter().any(|a| a.id == "preview"));
    }
}
