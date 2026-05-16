//! Permissive JSON normalizer. Accepts the JS-flavoured shape users
//! actually type in the launcher:
//!
//!   `{a:1, b:true, c:'hi'}` -> `{"a":1, "b":true, "c":"hi"}`
//!
//! Three relaxations over strict JSON:
//!   1. Unquoted object keys (must match `[A-Za-z_$][A-Za-z_$0-9]*`)
//!   2. Single-quoted strings (content preserved; inner `"` escaped)
//!   3. Trailing commas before `}` / `]` are dropped
//!
//! Strict JSON passes through unchanged - callers try `serde_json` on
//! original first, then on the normalized form on failure. The
//! tolerant path never runs on valid input, so theres no risk of
//! reshaping a document that was already right
//!
//! No crate dependency: the launcher's hot path wants predictable
//! latency on keystroke-sized input, and a hand-rolled pass over a
//! 120-char command line is measured in microseconds

/// Transform a loose-JSON-like string into strict JSON. Idempotent on
/// already-strict input within same round (because the three
/// relaxation rules never touch already-quoted / already-comma-free
/// structures). Use after your first `serde_json::from_str` has
/// failed; dont use as a pre-parse substitute when strict mode works
pub fn normalize(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    // Pass 1: swap single-quoted strings, quote unquoted keys
    let mut out = String::with_capacity(chars.len() + 16);
    let mut i = 0;
    let mut in_double = false;
    let mut in_single = false;
    while i < chars.len() {
        let c = chars[i];

        // Inside `"..."` - preserve verbatim, honour escapes
        if in_double {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_double = false;
            }
            i += 1;
            continue;
        }

        // Inside `'...'` - convert to `"..."` on the fly
        if in_single {
            if c == '\\' && i + 1 < chars.len() {
                // Escapes carry through unchanged - `\'` becomes `\'`
                // which stays valid inside rewritten string
                out.push('\\');
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '\'' {
                out.push('"');
                in_single = false;
                i += 1;
                continue;
            }
            if c == '"' {
                // A literal `"` inside a single-quoted string needs to
                // be escaped once we've rewrapped it in `"`
                out.push('\\');
                out.push('"');
                i += 1;
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }

        if c == '"' {
            out.push(c);
            in_double = true;
            i += 1;
            continue;
        }
        if c == '\'' {
            out.push('"');
            in_single = true;
            i += 1;
            continue;
        }

        // Unquoted-key detection: after `{` or `,`, if we see an
        // identifier followed by a `:`, wrap the identifier in `"`.
        // The key stays "unquoted if already quoted" - if user
        // wrote `{"a":1}` we never enter this branch
        if c == '{' || c == ',' {
            out.push(c);
            i += 1;
            let mut j = i;
            // Skip whitespace
            while j < chars.len() && chars[j].is_whitespace() {
                out.push(chars[j]);
                j += 1;
            }
            if j < chars.len() && is_ident_start(chars[j]) {
                let ident_start = j;
                while j < chars.len() && is_ident_continue(chars[j]) {
                    j += 1;
                }
                let ident_end = j;
                let mut k = j;
                while k < chars.len() && chars[k].is_whitespace() {
                    k += 1;
                }
                if k < chars.len() && chars[k] == ':' {
                    // Commit: emit quoted identifier, advance past it
                    out.push('"');
                    for c in &chars[ident_start..ident_end] {
                        out.push(*c);
                    }
                    out.push('"');
                    i = ident_end;
                    continue;
                }
            }
            // Not a key - nothing to rewrite, continue from i (already
            // past the `{`/`,` plus any whitespace we emitted)
            i = j;
            continue;
        }

        out.push(c);
        i += 1;
    }

    // Pass 2: strip trailing commas before `}` / `]`. Cheap state
    // machine; running it over the already-partially-normalized
    // output avoids re-walking input twice
    strip_trailing_commas(&out)
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_ascii_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

fn strip_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if escape {
            out.push(c);
            escape = false;
            i += 1;
            continue;
        }
        if in_string {
            if c == '\\' {
                out.push(c);
                escape = true;
                i += 1;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                // Drop the comma, keep whatever whitespace followed -
                // it still renders same to parser
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Convenience: try strict parse first, fall back to normalize -> parse.
/// Returns parsed `serde_json::Value` on either success path
pub fn parse_permissive(
    input: &str,
) -> Result<serde_json::Value, serde_json::Error> {
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(v) => Ok(v),
        Err(_) => serde_json::from_str(&normalize(input)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_json_passes_through_unchanged() {
        let s = r#"{"a":1,"b":"hi","c":[1,2]}"#;
        assert_eq!(normalize(s), s);
    }

    #[test]
    fn quotes_unquoted_object_keys() {
        let s = "{a:1, b:true}";
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v, json!({"a": 1, "b": true}));
    }

    #[test]
    fn single_quoted_strings_become_double_quoted() {
        let s = "{a:'hi'}";
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v, json!({"a": "hi"}));
    }

    #[test]
    fn embedded_double_in_single_quoted_is_escaped() {
        let s = r#"{a:'he said "hi"'}"#;
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v["a"], json!("he said \"hi\""));
    }

    #[test]
    fn trailing_commas_dropped_in_objects_and_arrays() {
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&normalize("{a:1,}")).unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&normalize("[1,2,3,]")).unwrap(),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn nested_structures() {
        let s = "{a:{b:1, c:[2,3,]},d:'x'}";
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v, json!({"a": {"b": 1, "c": [2, 3]}, "d": "x"}));
    }

    #[test]
    fn colons_inside_strings_dont_confuse_key_detection() {
        // The `a:b` inside a string must stay literal - the rewriter
        // must not treat `b` as a key just because a `:` follows
        let s = r#"{url:"https://example.com/p?q=1"}"#;
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v["url"], json!("https://example.com/p?q=1"));
    }

    #[test]
    fn whitespace_around_keys_preserved() {
        let s = "{\n  a : 1,\n  b : 2\n}";
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v, json!({"a": 1, "b": 2}));
    }

    #[test]
    fn underscore_and_dollar_are_valid_ident_starts() {
        let s = "{_hidden:1, $meta:2, plain3:3}";
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v["_hidden"], 1);
        assert_eq!(v["$meta"], 2);
        assert_eq!(v["plain3"], 3);
    }

    #[test]
    fn literal_true_false_null_are_not_quoted_as_keys() {
        // Values on the right side of `:` remain bare. The detector
        // only fires between `{`/`,` and `:`
        let s = "{a:true, b:false, c:null}";
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v, json!({"a": true, "b": false, "c": null}));
    }

    #[test]
    fn arrays_without_keys_unchanged() {
        let s = "[1, 2, 'three']";
        let v: serde_json::Value = serde_json::from_str(&normalize(s)).unwrap();
        assert_eq!(v, json!([1, 2, "three"]));
    }

    #[test]
    fn parse_permissive_handles_both_strict_and_loose() {
        assert_eq!(parse_permissive(r#"{"a":1}"#).unwrap(), json!({"a": 1}));
        assert_eq!(parse_permissive("{a:1}").unwrap(), json!({"a": 1}));
        assert_eq!(parse_permissive("{a:'hi',}").unwrap(), json!({"a": "hi"}));
    }

    #[test]
    fn parse_permissive_reports_errors_for_truly_broken_input() {
        assert!(parse_permissive("{a:").is_err());
        assert!(parse_permissive("{1:2}").is_err());  // numeric key isn't an identifier
    }
}
