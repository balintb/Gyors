//! Preset AI transforms on clipboard or inline text
//!
//! Triggers on verb keywords (`summarize`, `explain`, `rewrite`, `fix`,
//! `shorten`, `expand`, `tldr`, `translate <lang>`). Each verb carries
//! a hand-tuned instruction so user types one keyword and gets the
//! result without writing a prompt
//!
//! Source resolution:
//!   - `<verb>` (no tail) -> latest clipboard item
//!   - `<verb> <text>` -> use the inline text
//!   - `translate <lang>` / `translate <lang> <text>` -> same but with
//!     the first trailing token consumed as the target language
//!
//! Activation emits `Effect::AiTransform { text, instruction }`. The
//! Swift shell runs it through user's configured AI backend and
//! shows result in existing `aiResult` inline view
//!
//! Provider keeps a handle on index so it can read the most
//! recent clipboard item without a Swift roundtrip - same trick
//! `ClipboardProvider` uses

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use gyors_index::Index;
use std::sync::Arc;

pub struct AiTransformsProvider {
    index: Arc<Index>,
}

impl AiTransformsProvider {
    pub fn new(index: Arc<Index>) -> Self {
        Self { index }
    }
}

const PREVIEW_CHARS: usize = 60;
const TITLE_CHARS: usize = 80;

/// A preset transform. Invariant: `keyword` has no trailing space and
/// matches lowercased, so parsing can treat `summarize` and
/// `Summarize ` identically
pub struct Verb {
    pub keyword: &'static str,
    pub icon: &'static str,
    pub label: &'static str,
    pub instruction: &'static str,
    /// When `true`, the first whitespace-delimited token after the
    /// keyword is consumed as an argument (e.g. target language)
    pub takes_arg: bool,
}

/// Lookup an AI-transform verb by its canonical keyword (lowercased).
/// Used by the pipeline-sink path so `text | summarize` reuses the same
/// instruction keyword-triggered provider would emit. Returns
/// `None` for `translate` because the pipeline syntax doesn't carry
/// a per-stage argument (the target language)
pub fn verb_for(keyword: &str) -> Option<&'static Verb> {
    let lower = keyword.to_ascii_lowercase();
    VERBS
        .iter()
        .find(|v| v.keyword == lower && !v.takes_arg)
}

// Short, opinionated system prompts. Kept deliberately terse: the
// user's target model may be small (local Ollama), and long preambles
// inflate latency without changing output much
const VERBS: &[Verb] = &[
    Verb {
        keyword: "summarize",
        icon: "text.redaction",
        label: "Summarize",
        instruction: "Summarize the following text in 2-4 sentences. \
                      Preserve key facts; drop filler.",
        takes_arg: false,
    },
    Verb {
        keyword: "tldr",
        icon: "text.redaction",
        label: "TL;DR",
        instruction: "Write a one-sentence TL;DR of the following text.",
        takes_arg: false,
    },
    Verb {
        keyword: "explain",
        icon: "questionmark.bubble",
        label: "Explain",
        instruction: "Explain the following clearly in plain English. \
                      Assume a smart non-expert. No preamble.",
        takes_arg: false,
    },
    Verb {
        keyword: "rewrite",
        icon: "pencil.and.outline",
        label: "Rewrite",
        instruction: "Rewrite the following to be clearer and more \
                      concise. Preserve meaning and tone. Return only \
                      the rewrite.",
        takes_arg: false,
    },
    Verb {
        keyword: "fix",
        icon: "checkmark.seal",
        label: "Fix grammar",
        instruction: "Fix grammar and spelling in the following text. \
                      Preserve voice. Return only the corrected text.",
        takes_arg: false,
    },
    Verb {
        keyword: "shorten",
        icon: "arrow.down.right.and.arrow.up.left",
        label: "Shorten",
        instruction: "Shorten the following text. Preserve meaning. \
                      Return only the shortened text.",
        takes_arg: false,
    },
    Verb {
        keyword: "expand",
        icon: "arrow.up.left.and.arrow.down.right",
        label: "Expand",
        instruction: "Expand the following text with relevant detail \
                      and examples. Keep the tone. Return only the \
                      expanded text.",
        takes_arg: false,
    },
    Verb {
        keyword: "translate",
        icon: "character.bubble",
        label: "Translate",
        // Filled in at activation time with the concrete target language
        instruction: "",
        takes_arg: true,
    },
];

/// Parsed trigger: which verb, what text to transform, and (for
/// verbs with `takes_arg`) the captured argument
struct Parsed {
    verb: &'static Verb,
    arg: Option<String>,
    text: Option<String>,
}

/// Split `pattern` into (verb, arg, text). Returns `None` if the
/// pattern doesn't start with a known verb. `arg` is always `None` for
/// verbs whose `takes_arg` is false - even if user typed extra
/// tokens, those become part of `text`
fn parse(pattern: &str) -> Option<Parsed> {
    let lower = pattern.to_lowercase();
    for verb in VERBS {
        // Accept `<kw>` exactly (no tail) or `<kw> ...` - case-insensitive
        // on keyword, tail preserved verbatim
        if lower == verb.keyword {
            return Some(Parsed { verb, arg: None, text: None });
        }
        if let Some(rest) = lower.strip_prefix(&format!("{} ", verb.keyword)) {
            // Reindex into original `pattern` so we preserve case
            // (crucial when user's input is the text to transform)
            let rest_start = verb.keyword.len() + 1;
            let rest_orig = pattern[rest_start..].trim_start();
            if !rest.trim_start().starts_with(' ') {
                // Normal path. Fall through to parse arg + text below
            }
            if verb.takes_arg {
                // Pull the first whitespace-delimited token as the
                // argument. Remainder (if any) becomes inline text
                let mut parts = rest_orig.splitn(2, char::is_whitespace);
                let arg = parts.next().map(|s| s.to_string()).filter(|s| !s.is_empty());
                let text = parts
                    .next()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                return Some(Parsed { verb, arg, text });
            }
            let text = if rest_orig.is_empty() {
                None
            } else {
                Some(rest_orig.to_string())
            };
            return Some(Parsed { verb, arg: None, text });
        }
    }
    None
}

/// Compose the concrete instruction from verb + arg, e.g.
/// "Translate the following into spanish." for `translate spanish`
fn resolve_instruction(verb: &Verb, arg: Option<&str>) -> String {
    if verb.takes_arg {
        // Translate needs a language. No language -> a friendly
        // instruction that asks the model to pick
        let lang = arg.unwrap_or("English");
        format!(
            "Translate the following text into {lang}. Return only the \
             translation, no preface."
        )
    } else {
        verb.instruction.to_string()
    }
}

#[async_trait]
impl Provider for AiTransformsProvider {
    fn id(&self) -> &str {
        "ait"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(parsed) = parse(query.pattern()) else {
            return vec![];
        };

        // Resolve text source: inline first, else most recent clipboard
        let (text, source_label) = match parsed.text.as_deref() {
            Some(t) => (t.to_string(), "input".to_string()),
            None => match self.index.clipboard_recent(1).ok().and_then(|v| v.into_iter().next()) {
                Some(item) => (item.content, "clipboard".to_string()),
                None => {
                    // Nothing to transform. Emit a guidance candidate so
                    // user understands why nothing happened
                    return vec![empty_clipboard_hint(parsed.verb)];
                }
            },
        };

        // For the translate verb, demand a language argument before we
        // commit to call - otherwise we'd silently default to
        // English and surprise user
        if parsed.verb.takes_arg && parsed.arg.is_none() {
            return vec![needs_arg_hint(parsed.verb)];
        }

        let preview = truncate(&text.replace('\n', " "), PREVIEW_CHARS);
        let arg_suffix = parsed
            .arg
            .as_deref()
            .map(|a| format!(" → {a}"))
            .unwrap_or_default();
        // Encode both text and instruction into the id so `activate`
        // doesn't need to re-parse / re-read clipboard (which
        // could have changed between query and activate)
        let id_payload = format!(
            "{}\n{}\n{}",
            parsed.verb.keyword,
            parsed.arg.as_deref().unwrap_or(""),
            text,
        );
        let id = format!("ait::{}::{}", parsed.verb.keyword, base64_like(&id_payload));

        vec![Candidate {
            id,
            title: format!(
                "{}{}: {}",
                parsed.verb.label,
                arg_suffix,
                truncate(&preview, TITLE_CHARS.saturating_sub(parsed.verb.label.len() + 4)),
            ),
            subtitle: Some(format!(
                "source: {source_label} · {} chars · ↵ ask AI",
                text.chars().count()
            )),
            icon: Icon::SfSymbol(parsed.verb.icon.into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("Ask AI")],
            search_text: String::new(),
            bypass_rank: true,
        }]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let rest = id
            .strip_prefix("ait::")
            .ok_or_else(|| anyhow::anyhow!("invalid ait id: {id}"))?;
        // Guidance candidates are activated as no-ops
        if rest.starts_with("__hint__::") {
            return Ok(Effect::None);
        }
        let (keyword, payload) = rest
            .split_once("::")
            .ok_or_else(|| anyhow::anyhow!("malformed ait id: {id}"))?;
        let verb = VERBS
            .iter()
            .find(|v| v.keyword == keyword)
            .ok_or_else(|| anyhow::anyhow!("unknown ait verb: {keyword}"))?;
        let decoded = decode_base64_like(payload)
            .ok_or_else(|| anyhow::anyhow!("corrupt ait payload"))?;
        // Decoded = "<keyword>\n<arg>\n<text>"
        let mut lines = decoded.splitn(3, '\n');
        let _ = lines.next();
        let arg = lines.next().unwrap_or("");
        let text = lines.next().unwrap_or("");
        let instruction = resolve_instruction(
            verb,
            if arg.is_empty() { None } else { Some(arg) },
        );
        Ok(Effect::AiTransform {
            text: text.to_string(),
            instruction,
        })
    }
}

fn empty_clipboard_hint(verb: &Verb) -> Candidate {
    Candidate {
        id: format!("ait::__hint__::empty-clipboard::{}", verb.keyword),
        title: format!("{}: clipboard is empty", verb.label),
        subtitle: Some(format!("Type `{}` followed by text to transform", verb.keyword)),
        icon: Icon::SfSymbol("sparkles".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn needs_arg_hint(verb: &Verb) -> Candidate {
    Candidate {
        id: format!("ait::__hint__::needs-arg::{}", verb.keyword),
        title: format!("{}: specify a language", verb.label),
        subtitle: Some(format!("e.g. `{} spanish` - applies to clipboard", verb.keyword)),
        icon: Icon::SfSymbol("sparkles".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

// A tiny base-64-ish encoder so payload survives the `::` splitter
// used elsewhere in the id space. We just URL-safe-b64 without
// padding; no security intent, only id-safety
fn base64_like(s: &str) -> String {
    use base64::prelude::*;
    BASE64_URL_SAFE_NO_PAD.encode(s.as_bytes())
}

fn decode_base64_like(s: &str) -> Option<String> {
    use base64::prelude::*;
    let bytes = BASE64_URL_SAFE_NO_PAD.decode(s).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gyors_index::Index;

    fn fresh_provider() -> (AiTransformsProvider, Arc<Index>) {
        let idx = Arc::new(Index::in_memory().unwrap());
        (AiTransformsProvider::new(Arc::clone(&idx)), idx)
    }

    #[test]
    fn parse_bare_verb_clipboard_path() {
        let p = parse("summarize").unwrap();
        assert_eq!(p.verb.keyword, "summarize");
        assert!(p.text.is_none());
        assert!(p.arg.is_none());
    }

    #[test]
    fn parse_inline_text() {
        let p = parse("summarize This is my text").unwrap();
        assert_eq!(p.verb.keyword, "summarize");
        assert_eq!(p.text.as_deref(), Some("This is my text"));
    }

    #[test]
    fn parse_translate_pulls_language() {
        let p = parse("translate spanish hola amigo").unwrap();
        assert_eq!(p.arg.as_deref(), Some("spanish"));
        assert_eq!(p.text.as_deref(), Some("hola amigo"));
    }

    #[test]
    fn parse_translate_bare_language_no_text() {
        let p = parse("translate french").unwrap();
        assert_eq!(p.arg.as_deref(), Some("french"));
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_is_case_insensitive_on_keyword() {
        assert!(parse("Summarize hi").is_some());
        assert!(parse("TLDR hi").is_some());
    }

    #[test]
    fn parse_rejects_unknown_verb() {
        assert!(parse("bogus hi").is_none());
        assert!(parse("summarize_").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn parse_bare_keyword_without_space_matches() {
        // `tldr` with no text uses clipboard
        assert!(parse("tldr").is_some());
    }

    #[tokio::test]
    async fn bare_verb_with_empty_clipboard_emits_guidance() {
        let (p, _) = fresh_provider();
        let out = p.query(&Query::new("summarize")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.contains("__hint__::empty-clipboard"));
    }

    #[tokio::test]
    async fn bare_verb_with_clipboard_uses_it() {
        let (p, idx) = fresh_provider();
        idx.record_clipboard("some long text to summarize", now_secs()).unwrap();
        let out = p.query(&Query::new("summarize")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.starts_with("Summarize:"));
        assert!(out[0].subtitle.as_deref().unwrap().contains("source: clipboard"));
    }

    #[tokio::test]
    async fn inline_text_preferred_over_clipboard() {
        let (p, idx) = fresh_provider();
        idx.record_clipboard("CLIPBOARD", now_secs()).unwrap();
        let out = p.query(&Query::new("summarize INLINE")).await;
        assert!(out[0].title.contains("INLINE"));
        assert!(out[0].subtitle.as_deref().unwrap().contains("source: input"));
    }

    #[tokio::test]
    async fn translate_without_language_prompts_for_one() {
        let (p, idx) = fresh_provider();
        idx.record_clipboard("hola", now_secs()).unwrap();
        let out = p.query(&Query::new("translate")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.contains("__hint__::needs-arg"));
    }

    #[tokio::test]
    async fn translate_with_language_and_inline_text() {
        let (p, _) = fresh_provider();
        let out = p.query(&Query::new("translate spanish hello world")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("→ spanish"));
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::AiTransform { text, instruction } => {
                assert_eq!(text, "hello world");
                assert!(instruction.to_lowercase().contains("spanish"));
            }
            other => panic!("expected AiTransform, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_carries_text_and_instruction() {
        let (p, _) = fresh_provider();
        let out = p.query(&Query::new("summarize one two three")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::AiTransform { text, instruction } => {
                assert_eq!(text, "one two three");
                assert!(instruction.to_lowercase().contains("summarize"));
            }
            other => panic!("expected AiTransform, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_guidance_candidate_is_noop() {
        let (p, _) = fresh_provider();
        let out = p.query(&Query::new("summarize")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        assert!(matches!(effect, Effect::None));
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let (p, _) = fresh_provider();
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn id_uses_base64_payload_so_colons_in_text_survive() {
        // Clipboard text containing `::` must not break id routing
        let (p, _) = fresh_provider();
        let out = p.query(&Query::new("explain foo::bar::baz")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        if let Effect::AiTransform { text, .. } = effect {
            assert_eq!(text, "foo::bar::baz");
        } else {
            panic!("expected AiTransform");
        }
    }

    #[tokio::test]
    async fn all_verbs_parseable() {
        // Smoke test: every declared verb is actually reachable
        for v in VERBS {
            let pattern = if v.takes_arg {
                format!("{} en body", v.keyword)
            } else {
                format!("{} body", v.keyword)
            };
            assert!(parse(&pattern).is_some(), "verb {} unparseable", v.keyword);
        }
    }

    fn now_secs() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}
