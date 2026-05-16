//! Multi-stage chain pipelines: `note helo | upper | copy`
//!
//! Complements the classic 2-stage chain (`base | action`) by piping
//! a TEXT value through a series of transforms and into a sink. The
//! classic path still runs when a single-stage hint matches one of
//! the base candidate's actions; this module only kicks in when
//! stages match the transform/sink keyword registry
//!
//! ## Single source of truth
//!
//! Every transform and sink lives as a single `StageDef` entry in
//! [`STAGES`]. Dropdown autocomplete (`prefix_match`), execution
//! (`apply_transform` / `apply_sink`), and classification
//! (`classify_stage`) all read from that one table - adding a new
//! sink is a one-line append, and dropdown picks it up
//! automatically. This is enforced by the
//! `every_stage_resolves_through_apply_*` regression tests
//!
//! ## Data model
//!
//! ```text
//!     Source (notes, ...)  ->  ChainValue::Text  ->  Transform  ->  ...  ->  Sink
//! ```
//!
//! Each `Transform` is a `&str -> Option<String>` function pointer;
//! each `Sink` is a `&str -> Option<Effect>` function pointer. The
//! `Option` lets a runner reject malformed input (bad base64,
//! non-UTF-8, ...) without forcing registry to model error states

use base64::prelude::*;
use gyors_core::Effect;
use heck::{
    ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutyKebabCase, ToSnakeCase, ToTitleCase,
};
use sha2::{Digest, Sha256};

/// Name user types after a pipe. Matched case-insensitively
pub type StageKeyword = str;

/// Classify a stage keyword for UI labelling (subtitle of the
/// confirm row, unknown-stage helper text)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    Transform,
    Sink,
    Unknown,
}

/// Pure runner for a stage. Function pointers (no captures) so the
/// table remains `'static` and `Copy`. Transforms keep value as
/// text; sinks materialise the final `Effect`
///
/// `TransformArgs` is the arg-bearing variant: user types
/// `<canonical> <args>` (e.g. `jq .name`) and the runner receives the
/// args string + the piped text. Used for stages whose behaviour is
/// driven by an inline expression rather than being a fixed function
#[derive(Clone, Copy)]
pub enum StageRunner {
    Transform(fn(&str) -> Option<String>),
    TransformArgs(fn(args: &str, text: &str) -> Option<String>),
    Sink(fn(&str) -> Option<Effect>),
}

/// One row in the pipeline registry
///
/// The single registry pattern is deliberate: the alternative (one
/// match-statement for `apply_*`, a separate `STAGES` array for
/// dropdown UI) silently drifted - adding `summarize` to `apply_sink`
/// without touching `STAGES` shipped a sink that worked when typed
/// blindly but never appeared in dropdown. Keeping execution and
/// UI in lockstep is the whole point of the refactor
#[derive(Clone, Copy)]
pub struct StageDef {
    /// Canonical keyword dropdown displays + the chain
    /// substitutes when user picks this row
    pub canonical: &'static str,
    /// Other keywords that resolve to same runner. Drop-down
    /// suggestions still surface only canonical name; aliases
    /// just save user from remembering the exact spelling
    pub aliases: &'static [&'static str],
    pub kind: StageKind,
    /// Short human-readable description for the subtitle
    pub description: &'static str,
    /// Pure function that produces staged value or the final
    /// effect
    pub runner: StageRunner,
}

impl StageDef {
    /// True when `keyword` matches this entry's canonical name OR
    /// any of its aliases (case-insensitive). For arg-bearing
    /// stages (`TransformArgs`), also matches `<name> <args>`
    pub fn matches(&self, keyword: &str) -> bool {
        self.match_arg_span(keyword).is_some()
    }

    /// True if this stage takes inline arguments after keyword.
    /// Used by autocomplete + chain-spec substitution to preserve
    /// user-typed args when completing canonical name
    pub fn takes_args(&self) -> bool {
        matches!(self.runner, StageRunner::TransformArgs(_))
    }

    /// Returns the trimmed args slice when `keyword` matches in the
    /// `<name> <args>` form. Empty string when `keyword` is just the
    /// canonical/alias (e.g. `jq` with no filter). `None` when this
    /// stage doesn't match at all
    ///
    /// The returned slice borrows from `keyword`'s bytes, lined up
    /// past the matched canonical or alias - we rely on canonicals
    /// being lowercase ASCII so byte offset is identical between
    /// original and the lowercased comparison string
    pub fn extract_args<'a>(&self, keyword: &'a str) -> Option<&'a str> {
        self.match_arg_span(keyword)
    }

    fn match_arg_span<'a>(&self, keyword: &'a str) -> Option<&'a str> {
        let trimmed = keyword.trim();
        let lower = trimmed.to_ascii_lowercase();
        for name in std::iter::once(self.canonical).chain(self.aliases.iter().copied()) {
            let n = name.to_ascii_lowercase();
            if lower == n {
                return Some("");
            }
            if self.takes_args() {
                let prefix = format!("{} ", n);
                if lower.starts_with(&prefix) {
                    // `name` is ASCII-lowercase by invariant; the
                    // matched bytes in `trimmed` are byte-equivalent
                    // in length to `n`, so slicing past `name.len()`
                    // is safe and preserves the args' original case
                    return Some(trimmed[name.len()..].trim_start());
                }
            }
        }
        None
    }
}

//
// Free functions kept short and obvious. Each runner is referenced
// once from the STAGES table below; co-locating them keeps the
// "what does this stage do?" answer one click away from the
// registration line

fn run_upper(s: &str) -> Option<String> { Some(s.to_uppercase()) }
fn run_lower(s: &str) -> Option<String> { Some(s.to_lowercase()) }
fn run_title(s: &str) -> Option<String> { Some(s.to_title_case()) }
fn run_capitalize(s: &str) -> Option<String> { Some(capitalize_first(s)) }
fn run_snake(s: &str) -> Option<String> { Some(s.to_snake_case()) }
fn run_kebab(s: &str) -> Option<String> { Some(s.to_kebab_case()) }
fn run_camel(s: &str) -> Option<String> { Some(s.to_lower_camel_case()) }
fn run_pascal(s: &str) -> Option<String> { Some(s.to_pascal_case()) }
fn run_constant(s: &str) -> Option<String> {
    Some(s.to_shouty_kebab_case().replace('-', "_"))
}
fn run_rev(s: &str) -> Option<String> { Some(s.chars().rev().collect()) }
fn run_trim(s: &str) -> Option<String> { Some(s.trim().to_string()) }
fn run_chop(s: &str) -> Option<String> {
    Some(s.chars().filter(|c| !c.is_whitespace()).collect())
}
fn run_b64(s: &str) -> Option<String> { Some(BASE64_STANDARD.encode(s.as_bytes())) }
fn run_unb64(s: &str) -> Option<String> {
    let bytes = BASE64_STANDARD.decode(s.trim()).ok()?;
    String::from_utf8(bytes).ok()
}
fn run_url(s: &str) -> Option<String> { Some(url_encode(s)) }
fn run_unurl(s: &str) -> Option<String> { url_decode(s) }
fn run_md5(s: &str) -> Option<String> { Some(format!("{:x}", md5::compute(s.as_bytes()))) }
fn run_sha1(s: &str) -> Option<String> {
    use sha1::{Digest as Sha1Digest, Sha1};
    let mut h = Sha1::new();
    h.update(s.as_bytes());
    Some(hex_encode(&h.finalize()))
}
fn run_sha256(s: &str) -> Option<String> {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    Some(hex_encode(&h.finalize()))
}
fn run_count_chars(s: &str) -> Option<String> { Some(s.chars().count().to_string()) }
fn run_count_words(s: &str) -> Option<String> {
    Some(s.split_whitespace().count().to_string())
}
fn run_jq(args: &str, text: &str) -> Option<String> {
    gyors_providers::jq_filter::run(args, text)
}

// Sinks
fn sink_copy(s: &str) -> Option<Effect> { Some(Effect::CopyToClipboard(s.into())) }
fn sink_show(s: &str) -> Option<Effect> {
    Some(Effect::ShowText {
        text: s.into(),
        label: "Pipeline output".into(),
        language: None,
        editable_path: None,
    })
}
fn sink_qr(s: &str) -> Option<Effect> {
    let png = gyors_providers::qr::generate_qr_png(s).ok()?;
    Some(Effect::ShowImagePng(BASE64_STANDARD.encode(&png)))
}
fn sink_qrcopy(s: &str) -> Option<Effect> {
    let png = gyors_providers::qr::generate_qr_png(s).ok()?;
    Some(Effect::CopyImagePng(BASE64_STANDARD.encode(&png)))
}
#[cfg(feature = "ai")]
fn sink_ai_ask(s: &str) -> Option<Effect> { Some(Effect::AskAi(s.into())) }
// Each AI-transform verb is its own runner so registry can hand
// off a `fn(&str) -> Option<Effect>` pointer without captures. Each
// runner reuses the curated instruction from the AiTransformsProvider
// so `summarize | ...` and `summarize <text>` produce identical model
// output
#[cfg(feature = "ai")]
macro_rules! ai_verb_sink {
    ($name:ident, $verb:literal) => {
        fn $name(s: &str) -> Option<Effect> {
            let v = gyors_providers::ai_transforms::verb_for($verb)?;
            Some(Effect::AiTransform {
                text: s.into(),
                instruction: v.instruction.to_string(),
            })
        }
    };
}
#[cfg(feature = "ai")]
ai_verb_sink!(sink_ai_summarize, "summarize");
#[cfg(feature = "ai")]
ai_verb_sink!(sink_ai_tldr, "tldr");
#[cfg(feature = "ai")]
ai_verb_sink!(sink_ai_explain, "explain");
#[cfg(feature = "ai")]
ai_verb_sink!(sink_ai_rewrite, "rewrite");
#[cfg(feature = "ai")]
ai_verb_sink!(sink_ai_fix, "fix");
#[cfg(feature = "ai")]
ai_verb_sink!(sink_ai_shorten, "shorten");
#[cfg(feature = "ai")]
ai_verb_sink!(sink_ai_expand, "expand");

/// THE registry. Order is roughly "what users reach for first":
/// Case transforms, then encodings, then hashes, then sinks. Adding
/// a stage = appending one entry here. Both dropdown and the
/// execution path read from this list
pub const STAGES: &[StageDef] = &[
    StageDef {
        canonical: "upper",
        aliases: &["uppercase", "upcase"],
        kind: StageKind::Transform,
        description: "UPPERCASE",
        runner: StageRunner::Transform(run_upper),
    },
    StageDef {
        canonical: "lower",
        aliases: &["lowercase", "downcase"],
        kind: StageKind::Transform,
        description: "lowercase",
        runner: StageRunner::Transform(run_lower),
    },
    StageDef {
        canonical: "title",
        aliases: &["titlecase"],
        kind: StageKind::Transform,
        description: "Title Case",
        runner: StageRunner::Transform(run_title),
    },
    StageDef {
        canonical: "capitalize",
        aliases: &[],
        kind: StageKind::Transform,
        description: "Capitalize first letter",
        runner: StageRunner::Transform(run_capitalize),
    },
    StageDef {
        canonical: "snake",
        aliases: &["snakecase"],
        kind: StageKind::Transform,
        description: "snake_case",
        runner: StageRunner::Transform(run_snake),
    },
    StageDef {
        canonical: "kebab",
        aliases: &["kebabcase", "dash"],
        kind: StageKind::Transform,
        description: "kebab-case",
        runner: StageRunner::Transform(run_kebab),
    },
    StageDef {
        canonical: "camel",
        aliases: &["camelcase"],
        kind: StageKind::Transform,
        description: "camelCase",
        runner: StageRunner::Transform(run_camel),
    },
    StageDef {
        canonical: "pascal",
        aliases: &["pascalcase"],
        kind: StageKind::Transform,
        description: "PascalCase",
        runner: StageRunner::Transform(run_pascal),
    },
    StageDef {
        canonical: "constant",
        aliases: &["screamingsnake"],
        kind: StageKind::Transform,
        description: "CONSTANT_CASE",
        runner: StageRunner::Transform(run_constant),
    },
    StageDef {
        canonical: "rev",
        aliases: &["reverse"],
        kind: StageKind::Transform,
        description: "Reverse characters",
        runner: StageRunner::Transform(run_rev),
    },
    StageDef {
        canonical: "trim",
        aliases: &[],
        kind: StageKind::Transform,
        description: "Trim whitespace",
        runner: StageRunner::Transform(run_trim),
    },
    StageDef {
        canonical: "chop",
        aliases: &["stripwhitespace"],
        kind: StageKind::Transform,
        description: "Remove all whitespace",
        runner: StageRunner::Transform(run_chop),
    },
    StageDef {
        canonical: "b64",
        aliases: &["base64", "encode64"],
        kind: StageKind::Transform,
        description: "Base64 encode",
        runner: StageRunner::Transform(run_b64),
    },
    StageDef {
        canonical: "unb64",
        aliases: &["decode64", "b64decode"],
        kind: StageKind::Transform,
        description: "Base64 decode",
        runner: StageRunner::Transform(run_unb64),
    },
    StageDef {
        canonical: "url",
        aliases: &["urlencode"],
        kind: StageKind::Transform,
        description: "URL-encode",
        runner: StageRunner::Transform(run_url),
    },
    StageDef {
        canonical: "unurl",
        aliases: &["urldecode"],
        kind: StageKind::Transform,
        description: "URL-decode",
        runner: StageRunner::Transform(run_unurl),
    },
    StageDef {
        canonical: "md5",
        aliases: &[],
        kind: StageKind::Transform,
        description: "MD5 hash",
        runner: StageRunner::Transform(run_md5),
    },
    StageDef {
        canonical: "sha1",
        aliases: &[],
        kind: StageKind::Transform,
        description: "SHA-1 hash",
        runner: StageRunner::Transform(run_sha1),
    },
    StageDef {
        canonical: "sha256",
        aliases: &[],
        kind: StageKind::Transform,
        description: "SHA-256 hash",
        runner: StageRunner::Transform(run_sha256),
    },
    StageDef {
        canonical: "count",
        aliases: &["countchars", "length", "len"],
        kind: StageKind::Transform,
        description: "Character count",
        runner: StageRunner::Transform(run_count_chars),
    },
    StageDef {
        canonical: "countwords",
        aliases: &["words"],
        kind: StageKind::Transform,
        description: "Word count",
        runner: StageRunner::Transform(run_count_words),
    },
    StageDef {
        canonical: "jq",
        aliases: &[],
        kind: StageKind::Transform,
        description: "JSON filter (dot-path / index / [])",
        runner: StageRunner::TransformArgs(run_jq),
    },
    StageDef {
        canonical: "copy",
        aliases: &["clipboard", "pasteboard"],
        kind: StageKind::Sink,
        description: "Copy to clipboard",
        runner: StageRunner::Sink(sink_copy),
    },
    StageDef {
        canonical: "show",
        aliases: &["preview", "display"],
        kind: StageKind::Sink,
        description: "Show as inline preview",
        runner: StageRunner::Sink(sink_show),
    },
    StageDef {
        canonical: "qr",
        aliases: &["qrcode"],
        kind: StageKind::Sink,
        description: "Render text as QR code (preview)",
        runner: StageRunner::Sink(sink_qr),
    },
    StageDef {
        canonical: "qrcopy",
        aliases: &["qr-copy"],
        kind: StageKind::Sink,
        description: "Render text as QR code (copy PNG)",
        runner: StageRunner::Sink(sink_qrcopy),
    },
    #[cfg(feature = "ai")]
    StageDef {
        canonical: "ai",
        aliases: &["ask"],
        kind: StageKind::Sink,
        description: "Ask AI (pipe text as prompt)",
        runner: StageRunner::Sink(sink_ai_ask),
    },
    #[cfg(feature = "ai")]
    StageDef {
        canonical: "summarize",
        aliases: &[],
        kind: StageKind::Sink,
        description: "Summarize via AI",
        runner: StageRunner::Sink(sink_ai_summarize),
    },
    #[cfg(feature = "ai")]
    StageDef {
        canonical: "tldr",
        aliases: &[],
        kind: StageKind::Sink,
        description: "One-sentence TL;DR via AI",
        runner: StageRunner::Sink(sink_ai_tldr),
    },
    #[cfg(feature = "ai")]
    StageDef {
        canonical: "explain",
        aliases: &[],
        kind: StageKind::Sink,
        description: "Explain in plain English via AI",
        runner: StageRunner::Sink(sink_ai_explain),
    },
    #[cfg(feature = "ai")]
    StageDef {
        canonical: "rewrite",
        aliases: &[],
        kind: StageKind::Sink,
        description: "Clearer rewrite via AI",
        runner: StageRunner::Sink(sink_ai_rewrite),
    },
    #[cfg(feature = "ai")]
    StageDef {
        canonical: "fix",
        aliases: &[],
        kind: StageKind::Sink,
        description: "Fix grammar/spelling via AI",
        runner: StageRunner::Sink(sink_ai_fix),
    },
    #[cfg(feature = "ai")]
    StageDef {
        canonical: "shorten",
        aliases: &[],
        kind: StageKind::Sink,
        description: "Shorten to ~half via AI",
        runner: StageRunner::Sink(sink_ai_shorten),
    },
    #[cfg(feature = "ai")]
    StageDef {
        canonical: "expand",
        aliases: &[],
        kind: StageKind::Sink,
        description: "Expand with detail via AI",
        runner: StageRunner::Sink(sink_ai_expand),
    },
];

/// Find the StageDef whose canonical OR alias matches `keyword`
pub fn find_stage(keyword: &StageKeyword) -> Option<&'static StageDef> {
    let kw = keyword.trim();
    if kw.is_empty() {
        return None;
    }
    STAGES.iter().find(|s| s.matches(kw))
}

/// Apply a transform by keyword. Returns `None` when keyword
/// isn't a transform - caller decides whether to fall through to
/// sink lookup or to flag "unknown stage"
pub fn apply_transform(keyword: &StageKeyword, text: &str) -> Option<String> {
    let stage = find_stage(keyword)?;
    match stage.runner {
        StageRunner::Transform(f) => f(text),
        StageRunner::TransformArgs(f) => {
            let args = stage.extract_args(keyword).unwrap_or("");
            f(args, text)
        }
        StageRunner::Sink(_) => None,
    }
}

/// Apply a sink by keyword. Returns `None` when keyword isn't a
/// sink - caller may try transforms
pub fn apply_sink(keyword: &StageKeyword, text: &str) -> Option<Effect> {
    let stage = find_stage(keyword)?;
    match stage.runner {
        StageRunner::Sink(f) => f(text),
        StageRunner::Transform(_) | StageRunner::TransformArgs(_) => None,
    }
}

pub fn classify_stage(keyword: &StageKeyword) -> StageKind {
    match find_stage(keyword) {
        Some(s) => s.kind,
        None => StageKind::Unknown,
    }
}

/// All stages whose canonical keyword OR any alias starts with
/// `partial`. Empty or whitespace-only input returns every stage -
/// used when user has just typed a trailing pipe and hasn't
/// named next stage. Each stage appears at most once even when
/// both canonical and an alias would match
///
/// For arg-bearing stages (`jq <expr>`), also keep the entry in the
/// list once user has finished typing the name and moved on to
/// the args - otherwise dropdown vanishes the moment they hit
/// space, which feels broken when they're mid-filter
pub fn prefix_match(partial: &str) -> Vec<&'static StageDef> {
    let p = partial.trim().to_ascii_lowercase();
    if p.is_empty() {
        return STAGES.iter().collect();
    }
    STAGES
        .iter()
        .filter(|s| {
            let stem_hit = s.canonical.to_ascii_lowercase().starts_with(&p)
                || s.aliases
                    .iter()
                    .any(|a| a.to_ascii_lowercase().starts_with(&p));
            if stem_hit {
                return true;
            }
            if s.takes_args() {
                let has_arg_prefix = |name: &str| {
                    let with_space = format!("{} ", name.to_ascii_lowercase());
                    p.starts_with(&with_space)
                };
                if has_arg_prefix(s.canonical)
                    || s.aliases.iter().any(|a| has_arg_prefix(a))
                {
                    return true;
                }
            }
            false
        })
        .collect()
}

/// Execute the full pipeline: accepts the already-extracted source
/// text and the stage list; returns final Effect (from the sink)
///
/// Rules:
/// - All stages except the last must be transforms.
/// - The last stage may be a sink OR a transform. If it's a transform,
///   the output is auto-shown via the `show` sink - saves user
///   from typing `| show` explicitly on a preview pipeline.
/// - Empty trailing stage (user typed a pipe but hasn't named the
///   next stage yet) is ignored, treating the pipeline as "complete
///   up to the last non-empty stage"
pub fn execute(source_text: String, stages: &[String]) -> Result<Effect, PipelineError> {
    let non_empty: Vec<&String> = stages.iter().filter(|s| !s.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return Ok(Effect::ShowText {
            text: source_text,
            label: "Pipeline output".into(),
            language: None,
            editable_path: None,
        });
    }
    let (last, rest) = non_empty.split_last().unwrap();
    let mut value = source_text;
    for kw in rest {
        match apply_transform(kw, &value) {
            Some(next) => value = next,
            None => {
                if apply_sink(kw, &value).is_some() {
                    return Err(PipelineError::SinkMidPipeline(kw.to_string()));
                }
                return Err(PipelineError::UnknownStage(kw.to_string()));
            }
        }
    }
    // Last stage: sink preferred, transform accepted (auto-wrapped in show)
    if let Some(effect) = apply_sink(last, &value) {
        return Ok(effect);
    }
    if let Some(transformed) = apply_transform(last, &value) {
        return Ok(Effect::ShowText {
            text: transformed,
            label: "Pipeline output".into(),
            language: None,
            editable_path: None,
        });
    }
    Err(PipelineError::UnknownStage((*last).to_string()))
}

#[derive(Debug)]
pub enum PipelineError {
    UnknownStage(String),
    SinkMidPipeline(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStage(s) => write!(f, "unknown pipeline stage `{s}`"),
            Self::SinkMidPipeline(s) => {
                write!(f, "sink `{s}` must be the last stage")
            }
        }
    }
}

impl std::error::Error for PipelineError {}


fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// RFC 3986 unreserved-plus-a-bit URL encoding. Deliberately
/// minimal - `gyors-providers` already has a fuller encoder, but we
/// dont want a cross-crate dep chain just for this stage
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02X}"));
            }
        }
    }
    out
}

fn url_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_digit(bytes[i + 1])?;
            let lo = hex_digit(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn transforms_basic_cases() {
        assert_eq!(apply_transform("upper", "hi").as_deref(), Some("HI"));
        assert_eq!(apply_transform("lower", "HI").as_deref(), Some("hi"));
        assert_eq!(apply_transform("rev", "abc").as_deref(), Some("cba"));
        assert_eq!(apply_transform("trim", "  x  ").as_deref(), Some("x"));
    }

    #[test]
    fn transforms_case_aliases() {
        assert_eq!(apply_transform("uppercase", "hi").as_deref(), Some("HI"));
        assert_eq!(apply_transform("UPCASE", "hi").as_deref(), Some("HI"));
        assert_eq!(apply_transform("lowercase", "HI").as_deref(), Some("hi"));
    }

    #[test]
    fn transforms_case_conversions() {
        assert_eq!(apply_transform("snake", "Hello World").as_deref(), Some("hello_world"));
        assert_eq!(apply_transform("kebab", "Hello World").as_deref(), Some("hello-world"));
        assert_eq!(apply_transform("pascal", "hello world").as_deref(), Some("HelloWorld"));
        assert_eq!(apply_transform("camel", "hello world").as_deref(), Some("helloWorld"));
        assert_eq!(apply_transform("title", "hello world").as_deref(), Some("Hello World"));
    }

    #[test]
    fn transforms_encodings() {
        assert_eq!(apply_transform("b64", "hi").as_deref(), Some("aGk="));
        assert_eq!(apply_transform("unb64", "aGk=").as_deref(), Some("hi"));
        assert_eq!(apply_transform("url", "a b&c").as_deref(), Some("a%20b%26c"));
        assert_eq!(apply_transform("unurl", "a%20b%26c").as_deref(), Some("a b&c"));
    }

    #[test]
    fn transforms_hashes() {
        assert_eq!(
            apply_transform("md5", "").as_deref(),
            Some("d41d8cd98f00b204e9800998ecf8427e"),
        );
        assert_eq!(
            apply_transform("sha256", "").as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        );
    }

    #[test]
    fn transforms_counts() {
        assert_eq!(apply_transform("count", "hello").as_deref(), Some("5"));
        assert_eq!(apply_transform("countwords", "a b c").as_deref(), Some("3"));
    }

    #[test]
    fn unknown_transforms() {
        assert!(apply_transform("xyzzy", "text").is_none());
    }


    #[test]
    fn sinks_produce_correct_effects() {
        match apply_sink("copy", "payload") {
            Some(Effect::CopyToClipboard(s)) => assert_eq!(s, "payload"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
        match apply_sink("show", "payload") {
            Some(Effect::ShowText { text, .. }) => assert_eq!(text, "payload"),
            other => panic!("expected ShowText, got {other:?}"),
        }
        match apply_sink("preview", "p") {
            Some(Effect::ShowText { .. }) => {}
            other => panic!("preview aliased to ShowText, got {other:?}"),
        }
        assert!(apply_sink("unknown", "x").is_none());
    }

    #[test]
    fn ai_sinks_produce_ai_effects() {
        match apply_sink("ai", "what time?") {
            Some(Effect::AskAi(s)) => assert_eq!(s, "what time?"),
            other => panic!("expected AskAi, got {other:?}"),
        }
        match apply_sink("ask", "what time?") {
            Some(Effect::AskAi(s)) => assert_eq!(s, "what time?"),
            other => panic!("expected AskAi for `ask` alias, got {other:?}"),
        }
        match apply_sink("summarize", "long body") {
            Some(Effect::AiTransform { text, instruction }) => {
                assert_eq!(text, "long body");
                assert!(
                    instruction.to_ascii_lowercase().contains("summarize"),
                    "expected curated summarize prompt, got: {instruction}"
                );
            }
            other => panic!("expected AiTransform, got {other:?}"),
        }
    }


    #[test]
    fn classify_distinguishes_kinds() {
        assert_eq!(classify_stage("upper"), StageKind::Transform);
        assert_eq!(classify_stage("copy"), StageKind::Sink);
        assert_eq!(classify_stage("ai"), StageKind::Sink);
        assert_eq!(classify_stage("summarize"), StageKind::Sink);
        assert_eq!(classify_stage("xyzzy"), StageKind::Unknown);
        assert_eq!(classify_stage(""), StageKind::Unknown);
        assert_eq!(classify_stage("  show  "), StageKind::Sink);
    }


    #[test]
    fn execute_single_sink() {
        match execute("payload".into(), &["copy".into()]).unwrap() {
            Effect::CopyToClipboard(s) => assert_eq!(s, "payload"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn execute_transform_then_sink() {
        match execute("hello".into(), &["upper".into(), "copy".into()]).unwrap() {
            Effect::CopyToClipboard(s) => assert_eq!(s, "HELLO"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn execute_chains_multiple_transforms() {
        match execute(
            "  hi  ".into(),
            &["trim".into(), "upper".into(), "copy".into()],
        )
        .unwrap()
        {
            Effect::CopyToClipboard(s) => assert_eq!(s, "HI"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn execute_trailing_transform_auto_wraps_in_show() {
        match execute("hello".into(), &["upper".into()]).unwrap() {
            Effect::ShowText { text, .. } => assert_eq!(text, "HELLO"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn execute_empty_stages_yields_show_of_source() {
        match execute("hello".into(), &["".into()]).unwrap() {
            Effect::ShowText { text, .. } => assert_eq!(text, "hello"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unknown_stages_earn_error() {
        let err = execute("x".into(), &["xyzzy".into(), "copy".into()]).unwrap_err();
        match err {
            PipelineError::UnknownStage(s) => assert_eq!(s, "xyzzy"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn sinks_only_come_last() {
        let err = execute("x".into(), &["copy".into(), "upper".into()]).unwrap_err();
        match err {
            PipelineError::SinkMidPipeline(s) => assert_eq!(s, "copy"),
            other => panic!("got {other:?}"),
        }
    }


    #[test]
    fn prefix_match_empty_returns_everything() {
        let all = prefix_match("");
        assert_eq!(all.len(), STAGES.len());
        let ws_only = prefix_match("   ");
        assert_eq!(ws_only.len(), STAGES.len());
    }

    #[test]
    fn prefix_match_narrows_by_canonical_prefix() {
        let matches = prefix_match("upp");
        assert!(matches.iter().any(|s| s.canonical == "upper"));
        assert!(
            matches.iter().all(|s| s.canonical.starts_with("upp")),
            "non-matching stage leaked: {:?}",
            matches.iter().map(|s| s.canonical).collect::<Vec<_>>()
        );
    }

    #[test]
    fn prefix_match_case_insensitive() {
        assert!(prefix_match("UPP").iter().any(|s| s.canonical == "upper"));
        assert!(prefix_match("CoP").iter().any(|s| s.canonical == "copy"));
    }

    #[test]
    fn prefix_match_unknown_returns_empty() {
        assert!(prefix_match("xyzzy").is_empty());
    }

    /// Regression for user's reported bug - `> ls -la | ai` showed
    /// "Unknown stage `ai`" because `apply_sink` knew about `ai` but
    /// `STAGES` (and therefore `prefix_match`) didn't. Pin the
    /// invariant: dropdown must surface every AI sink
    #[test]
    fn prefix_match_finds_every_ai_sink() {
        for kw in ["ai", "ask", "summarize", "tldr", "explain", "rewrite", "fix", "shorten", "expand"] {
            let matches = prefix_match(kw);
            assert!(
                matches.iter().any(|s| s.matches(kw)),
                "prefix_match({kw:?}) missed it - would render `Unknown stage` in dropdown"
            );
        }
    }

    /// Round-trip exhaustiveness: every entry in STAGES must resolve
    /// through the corresponding `apply_*` function. Catches a future
    /// contributor who adds a row but forgets the runner
    #[test]
    fn every_stage_resolves_through_apply() {
        for stage in STAGES {
            let input = sample_input_for(stage.canonical);
            let keyword = sample_keyword_for(stage);
            let ok = match stage.runner {
                StageRunner::Transform(_) | StageRunner::TransformArgs(_) => {
                    apply_transform(&keyword, input).is_some()
                }
                StageRunner::Sink(_) => apply_sink(&keyword, input).is_some(),
            };
            assert!(
                ok,
                "STAGE `{}` ({:?}) failed to resolve via apply_{} for input {input:?}",
                stage.canonical,
                stage.kind,
                match stage.runner {
                    StageRunner::Transform(_) | StageRunner::TransformArgs(_) => "transform",
                    StageRunner::Sink(_) => "sink",
                }
            );
        }
    }

    /// Per-canonical fixture input. Decode-style stages need their
    /// own encoded sample; `jq` needs valid JSON. Everything else
    /// runs against the literal string `sample`
    fn sample_input_for(canonical: &str) -> &'static str {
        match canonical {
            "unb64" => "c2FtcGxl",
            "unurl" => "a%20b",
            "jq" => r#"{"a":1}"#,
            _ => "sample",
        }
    }

    /// For arg-bearing stages the apply tests need a complete
    /// `canonical <args>` keyword string - bare `jq` with no filter
    /// would still parse but jq with empty filter is identity, so
    /// pass an explicit `.` to make the assertion intentional
    fn sample_keyword_for(stage: &StageDef) -> String {
        if stage.takes_args() {
            format!("{} .", stage.canonical)
        } else {
            stage.canonical.to_string()
        }
    }

    /// Round-trip exhaustiveness in the OTHER direction: every alias
    /// should resolve through `apply_*` AND classify identically to
    /// its canonical name. Catches an alias that points at the wrong
    /// kind, or a typo that leaves an alias unreachable. Uses
    /// per-canonical sample text so decode-style runners (unb64,
    /// unurl) get inputs they can actually parse
    #[test]
    fn every_alias_resolves_to_its_canonical_kind() {
        for stage in STAGES {
            let input = sample_input_for(stage.canonical);
            for alias in stage.aliases {
                assert_eq!(
                    classify_stage(alias),
                    stage.kind,
                    "alias `{alias}` → unexpected kind (expected {:?} from canonical `{}`)",
                    stage.kind,
                    stage.canonical,
                );
                let keyword = if stage.takes_args() {
                    format!("{alias} .")
                } else {
                    alias.to_string()
                };
                let resolved = match stage.runner {
                    StageRunner::Transform(_) | StageRunner::TransformArgs(_) => {
                        apply_transform(&keyword, input).is_some()
                    }
                    StageRunner::Sink(_) => apply_sink(&keyword, input).is_some(),
                };
                assert!(
                    resolved,
                    "alias `{alias}` (canonical `{}`) didn't resolve for input {input:?} - silent dead alias",
                    stage.canonical
                );
            }
        }
    }


    #[test]
    fn jq_keyword_with_filter_matches_canonical() {
        assert_eq!(classify_stage("jq .name"), StageKind::Transform);
        assert_eq!(classify_stage("jq"), StageKind::Transform);
    }

    #[test]
    fn jq_runs_dot_field_filter() {
        let out = apply_transform("jq .name", r#"{"name":"alice"}"#).unwrap();
        assert_eq!(out, "alice");
    }

    #[test]
    fn jq_runs_index_filter() {
        let out = apply_transform("jq .[1]", r#"["a","b","c"]"#).unwrap();
        assert_eq!(out, "b");
    }

    #[test]
    fn jq_with_invalid_json_returns_none() {
        // Non-JSON input must NOT silently succeed - the pipeline
        // would otherwise treat garbage as next stage's input
        assert!(apply_transform("jq .name", "not json").is_none());
    }

    #[test]
    fn execute_pipeline_with_jq_then_upper_then_copy() {
        // The landing-page example, end-to-end
        let eff = execute(
            r#"{"name":"alice"}"#.into(),
            &["jq .name".into(), "upper".into(), "copy".into()],
        )
        .unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "ALICE"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[test]
    fn prefix_match_keeps_jq_while_typing_filter() {
        // User has typed `jq .na` - dropdown row for jq must
        // still appear, otherwise it vanishes mid-keystroke as soon
        // as they hit space
        let matches = prefix_match("jq .na");
        assert!(
            matches.iter().any(|s| s.canonical == "jq"),
            "jq dropped from prefix_match when args were partial"
        );
    }

    #[test]
    fn extract_args_returns_typed_filter() {
        let stage = find_stage("jq").expect("jq registered");
        assert_eq!(stage.extract_args("jq"), Some(""));
        assert_eq!(stage.extract_args("jq .name"), Some(".name"));
        assert_eq!(stage.extract_args("JQ .Name"), Some(".Name"));
        assert_eq!(stage.extract_args("  jq   .a.b  "), Some(".a.b"));
    }

    #[test]
    fn extract_args_none_for_non_arg_stages() {
        let upper = find_stage("upper").expect("upper registered");
        // `upper foo` is not a valid invocation - matches() must
        // refuse it and extract_args must agree
        assert_eq!(upper.extract_args("upper"), Some(""));
        assert_eq!(upper.extract_args("upper foo"), None);
    }

    /// No two entries can claim same canonical or alias -
    /// otherwise `find_stage` would silently return the first match
    /// and the second entry's runner would be unreachable
    #[test]
    fn canonicals_and_aliases_are_unique_across_stages() {
        let mut seen: std::collections::HashMap<String, &'static str> =
            std::collections::HashMap::new();
        for stage in STAGES {
            let kw = stage.canonical.to_ascii_lowercase();
            assert!(
                !seen.contains_key(&kw),
                "duplicate canonical `{}` (also in `{}`)",
                stage.canonical,
                seen[&kw]
            );
            seen.insert(kw, stage.canonical);
            for alias in stage.aliases {
                let lower = alias.to_ascii_lowercase();
                assert!(
                    !seen.contains_key(&lower),
                    "alias `{alias}` collides with canonical/alias `{}` from stage `{}`",
                    seen[&lower],
                    stage.canonical
                );
                seen.insert(lower, stage.canonical);
            }
        }
    }
}
