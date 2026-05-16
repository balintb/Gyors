//! Tiny jq subset for `jq <expr>` pipeline transform
//!
//! Covers inline-usage common case advertised on landing page
//! (`clip | jq .name | upper | copy`): dot-path field access, integer
//! index, and `[]` flatten operator. Multi-output filters get joined
//! back to newline-separated text so rest of pipeline can keep
//! treating value as a single string
//!
//! Out of scope (deliberately, to keep dep footprint small): pipe
//! inside filter, comma, `select`, arithmetic, recursive descent.
//! Swap in real `jaq` family if anyone needs those
//!
//! ## Grammar
//!
//! ```text
//! filter   := '.' | step+
//! step     := '.' ident             // .foo
//!           | '[' digit+ ']'        // [0]
//!           | '[' ']'               // [] - flatten
//! ident    := [A-Za-z_][A-Za-z0-9_]*
//! ```
//!
//! Whitespace allowed between tokens

use serde_json::Value;

/// Run `filter` over `input` (a JSON-serialisable string). Returns
/// joined results as text, ready for next pipeline stage. Empty
/// filter behaves as identity (`.`) so pipeline degrades to a
/// pretty-printer rather than failing closed
pub fn run(filter: &str, input: &str) -> Option<String> {
    let value: Value = serde_json::from_str(input.trim()).ok()?;
    let steps = parse_filter(filter)?;
    let outputs = apply_steps(&steps, &value);
    if outputs.is_empty() {
        return None;
    }
    Some(
        outputs
            .iter()
            .map(format_output)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[derive(Debug, PartialEq)]
enum Step {
    Field(String),
    Index(usize),
    Flatten,
}

fn parse_filter(filter: &str) -> Option<Vec<Step>> {
    let trimmed = filter.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Some(vec![]);
    }

    let bytes = trimmed.as_bytes();
    let mut i = 0;
    let mut steps = Vec::new();

    // Filter must start with `.` - reject `foo` or `[0]` standalone so
    // we dont silently accept malformed input user might expect a
    // meaningful error from. Pipeline layer surfaces `None` as a
    // "stage failed" row, which is right outcome here
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => {
                i += 1;
            }
            b'.' => {
                i += 1;
                // After `.`, expect an ident or end (`.` alone, or
                // followed by `[`). `.` then nothing means identity
                // for remainder, which is fine
                if i >= bytes.len() {
                    break;
                }
                if bytes[i] == b'[' {
                    continue; // index step picks it up next iteration
                }
                if !is_ident_start(bytes[i]) {
                    return None;
                }
                let start = i;
                while i < bytes.len() && is_ident_cont(bytes[i]) {
                    i += 1;
                }
                let name = std::str::from_utf8(&bytes[start..i]).ok()?.to_string();
                steps.push(Step::Field(name));
            }
            b'[' => {
                i += 1;
                if i >= bytes.len() {
                    return None;
                }
                if bytes[i] == b']' {
                    steps.push(Step::Flatten);
                    i += 1;
                    continue;
                }
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if start == i || i >= bytes.len() || bytes[i] != b']' {
                    return None;
                }
                let idx: usize = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
                steps.push(Step::Index(idx));
                i += 1;
            }
            _ => return None,
        }
    }
    Some(steps)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn apply_steps(steps: &[Step], value: &Value) -> Vec<Value> {
    let mut current = vec![value.clone()];
    for step in steps {
        let mut next: Vec<Value> = Vec::with_capacity(current.len());
        for v in current {
            match step {
                Step::Field(name) => {
                    if let Value::Object(map) = &v {
                        if let Some(child) = map.get(name) {
                            next.push(child.clone());
                        }
                    }
                }
                Step::Index(i) => {
                    if let Value::Array(arr) = &v {
                        if let Some(child) = arr.get(*i) {
                            next.push(child.clone());
                        }
                    }
                }
                Step::Flatten => {
                    if let Value::Array(arr) = &v {
                        next.extend(arr.iter().cloned());
                    }
                }
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

/// String outputs come through unquoted (jq's `-r` mode by default)
/// because result usually flows into another text transform or
/// clipboard. Containers + scalars use compact JSON so downstream
/// stage sees something it can re-parse if needed
fn format_output(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

//
// Driven by chain orchestrator: when user is mid-typing a `jq
// <partial>` filter, we peek source JSON and suggest UNIQUE key
// that extends partial dot-path. Autoclose candidate orchestrator
// emits is what Swift's `splitGhost` pass turns into greyed-out tail
// text
//
// Hot path. Called on every keystroke during chain typing. Layout:
//
//   1. Reject oversized sources (cap bytes, not chars - we're about
//      to parse).
//   2. Peek first non-whitespace byte - bail if not `{` or `[`, so we
//      never parse non-JSON bodies (calc results, snippets, etc).
//   3. Parse partial filter (dot-path only - bracket/array forms
//      aren't completable since they're typed by index, not by name).
//   4. Walk parsed JSON to committed path.
//   5. Collect keys with partial as prefix. If exactly one, emit it

/// Source size limit. Larger bodies skip autocomplete entirely -
/// parse cost crowds out other orchestrate work. Real clipboard
/// entries are tiny (config blobs, JWTs, log lines); anyone who can
/// hit this limit knows what they're piping and can type path
/// without help
const MAX_AUTOCOMPLETE_SOURCE_BYTES: usize = 256 * 1024;

/// Try to extend `partial_filter` (e.g. `.a`, `.foo.b`, `.foo.`) to
/// unique key that exists in `json_text`. Returns full completed
/// filter expression (e.g. `.ai`, `.foo.bar`) on a unique match,
/// `None` otherwise
///
/// "Unique" is strict: zero matches OR two+ matches both return None.
/// Autoclose mechanism only earns ghost text when theres no ambiguity
pub fn complete_dot_path(json_text: &str, partial_filter: &str) -> Option<String> {
    if json_text.len() > MAX_AUTOCOMPLETE_SOURCE_BYTES {
        return None;
    }
    let head = json_text.trim_start().as_bytes().first().copied()?;
    if head != b'{' && head != b'[' {
        return None;
    }
    let (committed, partial_token) = parse_partial_dotpath(partial_filter)?;
    let root: Value = serde_json::from_str(json_text.trim()).ok()?;
    let cursor = walk_committed_path(&root, &committed)?;
    let obj = cursor.as_object()?;
    let mut matching = obj
        .keys()
        .filter(|k| k.starts_with(partial_token) && is_ident(k))
        .take(2);
    let first = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    if first.len() <= partial_token.len() {
        // Exact prefix-equals-key: nothing to extend, no ghost text
        return None;
    }
    let mut out = String::with_capacity(partial_filter.len() + first.len());
    out.push('.');
    for step in &committed {
        out.push_str(step);
        out.push('.');
    }
    out.push_str(first);
    Some(out)
}

/// Parse a partial dot-path into (committed steps, current partial
/// token). Committed steps are keys we've already walked past;
/// partial token is what user is currently typing (may be empty when
/// a trailing `.` was just pressed)
///
/// Returns None for anything that isn't a clean dot-path:
/// - missing leading `.`
/// - non-ident segments (bracket indexing, quoted keys, etc.)
/// - empty segments in the middle (`..` recursive descent)
fn parse_partial_dotpath(expr: &str) -> Option<(Vec<&str>, &str)> {
    let trimmed = expr.trim_start();
    if !trimmed.starts_with('.') {
        return None;
    }
    let rest = &trimmed[1..];
    if rest.is_empty() {
        return Some((Vec::new(), ""));
    }
    let parts: Vec<&str> = rest.split('.').collect();
    let (last, head) = parts.split_last().expect("split has at least one part");
    for segment in head {
        if !is_ident(segment) {
            return None;
        }
    }
    if !last.is_empty() && !is_partial_ident(last) {
        return None;
    }
    Some((head.to_vec(), *last))
}

/// Jq-style ident: `[A-Za-z_][A-Za-z0-9_]*`. Used for both committed
/// segments (where whole thing must be a valid name) and as a prefix
/// filter on JSON keys
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A partial ident "looks like start of an ident" - same rule, just
/// expressed at prefix
fn is_partial_ident(s: &str) -> bool {
    is_ident(s)
}

fn walk_committed_path<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = root;
    for key in path {
        cur = cur.as_object()?.get(*key)?;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_pretty_returns_input() {
        let out = run(".", r#"{"a":1}"#).unwrap();
        assert_eq!(out, r#"{"a":1}"#);
    }

    #[test]
    fn empty_filter_treated_as_identity() {
        let out = run("", r#"[1,2,3]"#).unwrap();
        assert_eq!(out, "[1,2,3]");
    }

    #[test]
    fn dot_field_unwraps_string() {
        // Landing page example
        let out = run(".name", r#"{"name":"alice","age":30}"#).unwrap();
        assert_eq!(out, "alice");
    }

    #[test]
    fn dot_field_returns_number_as_json() {
        let out = run(".age", r#"{"name":"alice","age":30}"#).unwrap();
        assert_eq!(out, "30");
    }

    #[test]
    fn chained_field_access() {
        let out = run(".user.name", r#"{"user":{"name":"bob"}}"#).unwrap();
        assert_eq!(out, "bob");
    }

    #[test]
    fn missing_field_yields_no_output() {
        // Multi-output empty result - caller treats as "filter matched
        // nothing", which surfaces as a failed stage
        assert!(run(".nope", r#"{"a":1}"#).is_none());
    }

    #[test]
    fn index_step_picks_element() {
        let out = run(".[1]", r#"["a","b","c"]"#).unwrap();
        assert_eq!(out, "b");
    }

    #[test]
    fn field_then_index() {
        let out = run(".users[0]", r#"{"users":["a","b"]}"#).unwrap();
        assert_eq!(out, "a");
    }

    #[test]
    fn index_out_of_bounds_no_output() {
        assert!(run(".[5]", r#"["a","b"]"#).is_none());
    }

    #[test]
    fn flatten_returns_newline_separated() {
        let out = run(".[]", r#"["a","b","c"]"#).unwrap();
        assert_eq!(out, "a\nb\nc");
    }

    #[test]
    fn field_then_flatten() {
        let out = run(".tags[]", r#"{"tags":["x","y"]}"#).unwrap();
        assert_eq!(out, "x\ny");
    }

    #[test]
    fn flatten_then_field_picks_each() {
        let out = run(".[].name", r#"[{"name":"a"},{"name":"b"}]"#).unwrap();
        assert_eq!(out, "a\nb");
    }

    #[test]
    fn invalid_json_returns_none() {
        assert!(run(".name", "not json").is_none());
    }

    #[test]
    fn malformed_filter_returns_none() {
        assert!(run(".[abc", r#"[1,2]"#).is_none());
        assert!(run("name", r#"{"name":1}"#).is_none());
        assert!(run(".[", r#"[1]"#).is_none());
    }

    #[test]
    fn flatten_on_non_array_no_output() {
        assert!(run(".[]", r#"{"a":1}"#).is_none());
    }

    #[test]
    fn nested_object_compact_json() {
        let out = run(".user", r#"{"user":{"id":1,"name":"a"}}"#).unwrap();
        // Object stays as compact JSON so next stage can re-parse
        assert!(out.contains("\"id\":1"));
        assert!(out.contains("\"name\":\"a\""));
    }

    #[test]
    fn null_value_serialises_as_null() {
        let out = run(".x", r#"{"x":null}"#).unwrap();
        assert_eq!(out, "null");
    }

    #[test]
    fn underscored_field_name() {
        let out = run(".first_name", r#"{"first_name":"a"}"#).unwrap();
        assert_eq!(out, "a");
    }

    #[test]
    fn whitespace_inside_filter_tolerated() {
        let out = run(" .name ", r#"{"name":"a"}"#).unwrap();
        assert_eq!(out, "a");
    }


    /// Mirrors user's actual config blob from bug report
    const SAMPLE_CONFIG: &str = r#"{
        "ai": {"provider": "apple", "router_enabled": true},
        "hotkey": "opt+shift+space",
        "terminal_app": "iTerm2",
        "theme": "neon"
    }"#;

    #[test]
    fn complete_top_level_unique_prefix() {
        // `.a` -> `.ai` (only top-level key starting with `a`)
        let out = complete_dot_path(SAMPLE_CONFIG, ".a");
        assert_eq!(out.as_deref(), Some(".ai"));
    }

    #[test]
    fn complete_top_level_ambiguous_returns_none() {
        // `.t` matches both `terminal_app` and `theme` - no autoclose
        let out = complete_dot_path(SAMPLE_CONFIG, ".t");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_nested_dotpath() {
        // `.ai.p` -> `.ai.provider` (only `ai` child starting with `p`)
        let out = complete_dot_path(SAMPLE_CONFIG, ".ai.p");
        assert_eq!(out.as_deref(), Some(".ai.provider"));
    }

    #[test]
    fn complete_trailing_dot_offers_unique_child() {
        // `.foo.` with foo having only one child suggests that child
        let body = r#"{"foo": {"only_one": 1}}"#;
        let out = complete_dot_path(body, ".foo.");
        assert_eq!(out.as_deref(), Some(".foo.only_one"));
    }

    #[test]
    fn complete_trailing_dot_with_many_children_returns_none() {
        // `.ai.` has two children - no autoclose
        let out = complete_dot_path(SAMPLE_CONFIG, ".ai.");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_no_match_returns_none() {
        let out = complete_dot_path(SAMPLE_CONFIG, ".zzz");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_exact_match_no_extension_returns_none() {
        // User has already typed full key; nothing to ghost
        let out = complete_dot_path(SAMPLE_CONFIG, ".theme");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_with_root_array_returns_none() {
        // We only suggest object keys; arrays use index syntax
        let out = complete_dot_path(r#"[1,2,3]"#, ".x");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_bracket_in_partial_skipped() {
        // `.foo[0].b` mixes index and dot - skip to keep parser
        // simple; user is past what we can usefully complete inline
        let body = r#"{"foo": [{"bar": 1}]}"#;
        let out = complete_dot_path(body, ".foo[0].b");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_non_json_source_returns_none() {
        let out = complete_dot_path("hello world", ".a");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_oversized_source_skipped() {
        // 257KB of JSON triggers cap. Big object body so leading-byte
        // precheck still passes
        let payload: String = "x".repeat(257 * 1024);
        let body = format!(r#"{{"data": "{payload}"}}"#);
        let out = complete_dot_path(&body, ".d");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_no_leading_dot_returns_none() {
        let out = complete_dot_path(SAMPLE_CONFIG, "ai");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_dot_alone_with_unique_top_level() {
        // Synthetic JSON with a single top-level key - bare `.` should
        // suggest it
        let out = complete_dot_path(r#"{"only": 1}"#, ".");
        assert_eq!(out.as_deref(), Some(".only"));
    }

    #[test]
    fn complete_skips_keys_with_non_ident_chars() {
        // Jq syntax can't reach `weird-key` via dot-path without
        // quoting; we dont suggest a completion that can't be safely
        // committed by Tab
        let body = r#"{"weird-key": 1, "alpha": 2}"#;
        let out = complete_dot_path(body, ".w");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_walks_through_non_object_returns_none() {
        // `.ai.provider.x` - provider is a string, can't go deeper
        let out = complete_dot_path(SAMPLE_CONFIG, ".ai.provider.x");
        assert_eq!(out, None);
    }

    #[test]
    fn complete_preserves_intermediate_path_in_output() {
        // Make sure head segments aren't dropped from returned filter
        // - we need FULL completed expression so Swift ghost-tail can
        // diff against what's typed
        let out = complete_dot_path(SAMPLE_CONFIG, ".ai.r").unwrap();
        assert!(out.starts_with(".ai."), "lost head segment: {out}");
        assert_eq!(out, ".ai.router_enabled");
    }
}
