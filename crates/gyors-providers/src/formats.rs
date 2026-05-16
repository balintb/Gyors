//! Pairwise format converters between JSON / YAML / TOML / CSV
//!
//! Keywords: `json2yaml`, `yaml2json`, `json2toml`, `toml2json`,
//! `yaml2toml`, `toml2yaml`, `csv2json`, `json2csv`. Each takes the rest
//! of query as input document and emits a single candidate with
//! the converted output. Invalid input surfaces parser error instead
//! so user isn't left guessing

use crate::loose_json;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct FormatConverterProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Yaml,
    Toml,
    Csv,
}

#[async_trait]
impl Provider for FormatConverterProvider {
    fn id(&self) -> &str {
        "fmt"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some((from, to, input)) = parse(pattern) else { return vec![]; };
        let input = input.trim();
        if input.is_empty() {
            return vec![];
        }
        match convert(input, from, to) {
            Ok(converted) => vec![candidate_ok(&converted, from, to)],
            Err(e) => vec![candidate_err(&e, from, to)],
        }
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> anyhow::Result<Effect> {
        if id == "fmt::error" {
            return Ok(Effect::None);
        }
        let rest = id
            .strip_prefix("fmt::ok::")
            .ok_or_else(|| anyhow::anyhow!("invalid fmt candidate id: {id}"))?;
        // Id format: `fmt::ok::<from>-<to>::<output>`
        let (tag, value) = rest
            .split_once("::")
            .ok_or_else(|| anyhow::anyhow!("missing tag in fmt id: {id}"))?;
        match action {
            "default" => Ok(Effect::CopyToClipboard(value.to_string())),
            "preview" => {
                // Target format drives syntax highlighting. Parse it
                // back out of the tag (same encoding used in the id)
                let language = tag
                    .split_once('-')
                    .map(|(_, to)| to.to_string());
                Ok(Effect::ShowText {
                    text: value.to_string(),
                    label: tag_to_label(tag),
                    language,
                    editable_path: None,
                })
            }
            other => anyhow::bail!("unknown action for fmt: {other}"),
        }
    }
}

fn tag_for(from: Format, to: Format) -> String {
    format!("{}-{}", short_name(from), short_name(to))
}

fn short_name(f: Format) -> &'static str {
    match f {
        Format::Json => "json",
        Format::Yaml => "yaml",
        Format::Toml => "toml",
        Format::Csv => "csv",
    }
}

fn tag_to_label(tag: &str) -> String {
    // Tag = "<from>-<to>" where each side is json/yaml/toml. Fallback to
    // the raw tag if the split ever doesn't match - keeps the preview
    // functional even if the id scheme shifts
    if let Some((from, to)) = tag.split_once('-') {
        format!("{} → {}", from.to_uppercase(), to.to_uppercase())
    } else {
        tag.to_string()
    }
}

/// Recognise a `<from>2<to> <input>` prefix and return parsed tuple.
/// Input may contain anything - we pass it verbatim to the converter
pub fn parse(s: &str) -> Option<(Format, Format, &str)> {
    for (prefix, from, to) in KEYWORDS {
        if let Some(rest) = s.strip_prefix(*prefix) {
            if rest.starts_with(char::is_whitespace) || rest.is_empty() {
                return Some((*from, *to, rest));
            }
        }
    }
    None
}

const KEYWORDS: &[(&str, Format, Format)] = &[
    ("json2yaml", Format::Json, Format::Yaml),
    ("yaml2json", Format::Yaml, Format::Json),
    ("json2toml", Format::Json, Format::Toml),
    ("toml2json", Format::Toml, Format::Json),
    ("yaml2toml", Format::Yaml, Format::Toml),
    ("toml2yaml", Format::Toml, Format::Yaml),
    ("csv2json", Format::Csv, Format::Json),
    ("json2csv", Format::Json, Format::Csv),
];

/// Convert `input` from `from` to `to`. Errors are user-facing strings
pub fn convert(input: &str, from: Format, to: Format) -> Result<String, String> {
    // Route through serde_json::Value as the intermediate representation -
    // every format we support round-trips through it
    let value: serde_json::Value = match from {
        // Permissive JSON accepts `{a:1, b:'hi'}` in addition to the
        // strict form - same UX as the `json` provider. YAML/TOML
        // already tolerate unquoted keys natively
        Format::Json => loose_json::parse_permissive(input)
            .map_err(|e| format!("JSON parse: {e}"))?,
        Format::Yaml => {
            let y: serde_yaml::Value = serde_yaml::from_str(input)
                .map_err(|e| format!("YAML parse: {e}"))?;
            serde_json::to_value(&y).map_err(|e| format!("YAML→JSON: {e}"))?
        }
        Format::Toml => {
            let t: toml::Value = input.parse::<toml::Value>()
                .map_err(|e| format!("TOML parse: {e}"))?;
            serde_json::to_value(&t).map_err(|e| format!("TOML→JSON: {e}"))?
        }
        Format::Csv => csv_to_json_value(input).map_err(|e| format!("CSV parse: {e}"))?,
    };

    match to {
        Format::Json => serde_json::to_string_pretty(&value)
            .map_err(|e| format!("JSON emit: {e}")),
        Format::Yaml => serde_yaml::to_string(&value).map_err(|e| format!("YAML emit: {e}")),
        Format::Toml => {
            // Toml crate refuses to serialize non-object roots. Wrap arrays
            // / scalars into `{ value = ... }` so user still gets a
            // usable TOML document rather than a cryptic error
            let wrapped = match &value {
                serde_json::Value::Object(_) => value.clone(),
                other => serde_json::json!({ "value": other }),
            };
            let t: toml::Value = serde_json::from_value(wrapped)
                .map_err(|e| format!("TOML convert: {e}"))?;
            toml::to_string_pretty(&t).map_err(|e| format!("TOML emit: {e}"))
        }
        Format::Csv => json_value_to_csv(&value),
    }
}

/// Parse a CSV document (header row required) into a JSON array of objects.
/// Each row becomes an object whose keys are the header column names.
/// Numeric fields are kept as numbers; everything else is a string
fn csv_to_json_value(input: &str) -> Result<serde_json::Value, String> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(input.as_bytes());
    let headers = reader
        .headers()
        .map_err(|e| format!("CSV header: {e}"))?
        .clone();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| format!("CSV row: {e}"))?;
        let mut obj = serde_json::Map::new();
        for (i, field) in record.iter().enumerate() {
            let key = headers.get(i).unwrap_or("").to_string();
            obj.insert(key, parse_scalar(field));
        }
        rows.push(serde_json::Value::Object(obj));
    }
    Ok(serde_json::Value::Array(rows))
}

/// Best-effort scalar coercion: integers, floats, booleans, then string.
/// Empty strings stay as empty strings (not null) - preserves shape so a
/// round-trip back to CSV doesn't lose columns
fn parse_scalar(s: &str) -> serde_json::Value {
    let trimmed = s.trim();
    if let Ok(i) = trimmed.parse::<i64>() {
        return serde_json::Value::Number(i.into());
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return serde_json::Value::Number(n);
        }
    }
    match trimmed {
        "true" => serde_json::Value::Bool(true),
        "false" => serde_json::Value::Bool(false),
        _ => serde_json::Value::String(s.to_string()),
    }
}

/// Render a JSON array of objects as CSV. The header is the union of
/// keys from the first row (subsequent rows missing a key emit empty
/// fields). Non-array or non-object inputs error out - for those, prefer
/// JSON<->YAML/TOML routes
fn json_value_to_csv(value: &serde_json::Value) -> Result<String, String> {
    let arr = value
        .as_array()
        .ok_or_else(|| "expected JSON array of objects for CSV emit".to_string())?;
    if arr.is_empty() {
        return Ok(String::new());
    }
    let first = arr[0]
        .as_object()
        .ok_or_else(|| "first row isn't an object".to_string())?;
    let headers: Vec<String> = first.keys().cloned().collect();

    let mut wtr = csv::WriterBuilder::new().from_writer(vec![]);
    wtr.write_record(&headers).map_err(|e| format!("CSV header emit: {e}"))?;
    for (idx, row) in arr.iter().enumerate() {
        let obj = row
            .as_object()
            .ok_or_else(|| format!("row {idx} isn't an object"))?;
        let fields: Vec<String> = headers
            .iter()
            .map(|h| match obj.get(h) {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            })
            .collect();
        wtr.write_record(&fields).map_err(|e| format!("CSV row emit: {e}"))?;
    }
    let bytes = wtr.into_inner().map_err(|e| format!("CSV finalize: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("CSV utf8: {e}"))
}

fn candidate_ok(converted: &str, from: Format, to: Format) -> Candidate {
    // Encode the direction into the id so `activate` can rebuild the
    // preview label without having to reparse original query
    let tag = tag_for(from, to);
    Candidate {
        id: format!("fmt::ok::{tag}::{converted}"),
        title: format!("Converted {} → {}", name(from), name(to)),
        subtitle: Some(format!("↵ copy · → preview · {}", flatten_preview(converted))),
        icon: Icon::SfSymbol("arrow.left.arrow.right.square".into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Copy"),
            Action::new("preview", "Preview"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn candidate_err(err: &str, from: Format, to: Format) -> Candidate {
    Candidate {
        id: "fmt::error".into(),
        title: format!("Couldn't convert {} → {}", name(from), name(to)),
        subtitle: Some(flatten_preview(err)),
        icon: Icon::SfSymbol("exclamationmark.triangle.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn name(f: Format) -> &'static str {
    match f {
        Format::Json => "JSON",
        Format::Yaml => "YAML",
        Format::Toml => "TOML",
        Format::Csv => "CSV",
    }
}

fn flatten_preview(s: &str) -> String {
    let flat: String = s.chars().map(|c| if c == '\n' || c == '\t' { ' ' } else { c }).collect();
    if flat.chars().count() > 120 {
        let head: String = flat.chars().take(120).collect();
        format!("{head}…")
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_known_keywords() {
        assert!(matches!(parse("json2yaml {\"a\":1}"), Some((Format::Json, Format::Yaml, _))));
        assert!(matches!(parse("yaml2json a: 1"), Some((Format::Yaml, Format::Json, _))));
        assert!(matches!(parse("toml2json x=1"), Some((Format::Toml, Format::Json, _))));
    }

    #[test]
    fn parse_rejects_unknown_prefix() {
        assert!(parse("foo2bar {}").is_none());
        assert!(parse("json2yaml_trimmed").is_none());
    }

    #[test]
    fn parse_requires_space_after_keyword() {
        assert!(parse("json2yamlfoo").is_none());
        assert!(parse("json2yaml ").is_some());
    }

    #[test]
    fn json_to_yaml_roundtrips_simple_object() {
        let out = convert(r#"{"a":1,"b":"hi"}"#, Format::Json, Format::Yaml).unwrap();
        assert!(out.contains("a: 1"));
        assert!(out.contains("b: hi"));
    }

    #[test]
    fn yaml_to_json_parses() {
        let out = convert("a: 1\nb: hi\n", Format::Yaml, Format::Json).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "hi");
    }

    #[test]
    fn toml_to_json_parses() {
        let out = convert("a = 1\nb = \"hi\"\n", Format::Toml, Format::Json).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "hi");
    }

    #[test]
    fn json_to_toml_wraps_scalar_roots() {
        let out = convert("42", Format::Json, Format::Toml).unwrap();
        assert!(out.contains("value = 42"));
    }

    #[test]
    fn convert_surfaces_parse_errors() {
        let err = convert(r#"{"a":}"#, Format::Json, Format::Yaml).unwrap_err();
        assert!(err.contains("JSON parse"));
    }

    #[tokio::test]
    async fn query_empty_without_keyword() {
        let p = FormatConverterProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn query_empty_with_keyword_but_no_input() {
        let p = FormatConverterProvider;
        assert!(p.query(&Query::new("json2yaml ")).await.is_empty());
    }

    #[tokio::test]
    async fn query_converts_json_to_yaml() {
        let p = FormatConverterProvider;
        let out = p.query(&Query::new(r#"json2yaml {"a":1}"#)).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("JSON → YAML"));
        assert!(out[0].id.starts_with("fmt::ok::"));
    }

    #[tokio::test]
    async fn query_shows_error_on_bad_input() {
        let p = FormatConverterProvider;
        let out = p.query(&Query::new(r#"json2yaml {"a":}"#)).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "fmt::error");
    }

    #[tokio::test]
    async fn activate_ok_copies_conversion() {
        let p = FormatConverterProvider;
        let out = p.query(&Query::new(r#"json2yaml {"a":1}"#)).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert!(s.contains("a: 1")),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_preview_shows_text_inline() {
        let p = FormatConverterProvider;
        let out = p.query(&Query::new(r#"json2yaml {"a":1}"#)).await;
        let eff = p.activate(&out[0].id, "preview").await.unwrap();
        match eff {
            Effect::ShowText { text, label, language, editable_path } => {
                assert!(text.contains("a: 1"));
                assert_eq!(label, "JSON → YAML");
                assert_eq!(language.as_deref(), Some("yaml"));
                assert!(editable_path.is_none(), "format conversions aren't editable");
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn candidate_exposes_preview_and_copy_actions() {
        let p = FormatConverterProvider;
        let out = p.query(&Query::new(r#"json2toml {"a":1}"#)).await;
        assert!(out[0].actions.iter().any(|a| a.id == "default"));
        assert!(out[0].actions.iter().any(|a| a.id == "preview"));
    }

    #[tokio::test]
    async fn activate_unknown_action_errors() {
        let p = FormatConverterProvider;
        let out = p.query(&Query::new(r#"json2yaml {"a":1}"#)).await;
        assert!(p.activate(&out[0].id, "bogus").await.is_err());
    }

    #[tokio::test]
    async fn activate_error_is_noop() {
        let p = FormatConverterProvider;
        let eff = p.activate(&"fmt::error".to_string(), "default").await.unwrap();
        assert!(matches!(eff, Effect::None));
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = FormatConverterProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }


    #[test]
    fn csv_to_json_basic() {
        let csv = "a,b,c\n1,2,3\n4,5,6";
        let json = convert(csv, Format::Csv, Format::Json).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["a"], 1);
        assert_eq!(arr[0]["b"], 2);
        assert_eq!(arr[0]["c"], 3);
        assert_eq!(arr[1]["a"], 4);
    }

    #[test]
    fn csv_to_json_keeps_strings() {
        let csv = "name,city\nAlice,Paris\nBob,Tokyo";
        let json = convert(csv, Format::Csv, Format::Json).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["name"], "Alice");
        assert_eq!(v[1]["city"], "Tokyo");
    }

    #[test]
    fn csv_to_json_handles_quoted_commas() {
        let csv = "name,note\nAlice,\"hi, there\"\nBob,plain";
        let json = convert(csv, Format::Csv, Format::Json).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["note"], "hi, there");
    }

    #[test]
    fn csv_to_json_parses_booleans() {
        let csv = "a,b\ntrue,false";
        let json = convert(csv, Format::Csv, Format::Json).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["a"], true);
        assert_eq!(v[0]["b"], false);
    }

    #[test]
    fn csv_to_json_floats() {
        let csv = "x\n1.5";
        let json = convert(csv, Format::Csv, Format::Json).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!((v[0]["x"].as_f64().unwrap() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn json_to_csv_basic() {
        let json = r#"[{"a":1,"b":2},{"a":3,"b":4}]"#;
        let csv = convert(json, Format::Json, Format::Csv).unwrap();
        // The csv writer emits \r\n by default; normalise for assertion
        let lines: Vec<&str> = csv.split_terminator(['\r', '\n']).filter(|l| !l.is_empty()).collect();
        assert_eq!(lines[0], "a,b");
        assert_eq!(lines[1], "1,2");
        assert_eq!(lines[2], "3,4");
    }

    #[test]
    fn json_to_csv_quotes_commas() {
        let json = r#"[{"a":"hi, there"}]"#;
        let csv = convert(json, Format::Json, Format::Csv).unwrap();
        assert!(csv.contains("\"hi, there\""), "got: {csv}");
    }

    #[test]
    fn json_to_csv_empty_array() {
        let csv = convert("[]", Format::Json, Format::Csv).unwrap();
        assert!(csv.is_empty());
    }

    #[test]
    fn json_to_csv_rejects_non_array() {
        let err = convert(r#"{"a":1}"#, Format::Json, Format::Csv).unwrap_err();
        assert!(err.contains("array of objects"));
    }

    #[test]
    fn json_to_csv_rejects_non_object_rows() {
        let err = convert(r#"[1,2,3]"#, Format::Json, Format::Csv).unwrap_err();
        assert!(err.contains("isn't an object"));
    }

    #[test]
    fn csv_json_roundtrip() {
        let original = "name,age\nAlice,30\nBob,25";
        let json = convert(original, Format::Csv, Format::Json).unwrap();
        let csv = convert(&json, Format::Json, Format::Csv).unwrap();
        let lines: Vec<&str> = csv.split_terminator(['\r', '\n']).filter(|l| !l.is_empty()).collect();
        // serde_json::Map sorts keys alphabetically by default - the
        // round-trip preserves all data but reorders columns. Use the
        // `preserve_order` feature on serde_json if column order matters
        // downstream
        assert_eq!(lines[0], "age,name");
        assert_eq!(lines[1], "30,Alice");
        assert_eq!(lines[2], "25,Bob");
    }

    #[tokio::test]
    async fn query_csv2json_keyword() {
        let p = FormatConverterProvider;
        let out = p.query(&Query::new("csv2json a,b\n1,2")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("CSV → JSON"));
    }

    #[tokio::test]
    async fn query_json2csv_keyword() {
        let p = FormatConverterProvider;
        let out = p.query(&Query::new(r#"json2csv [{"a":1,"b":2}]"#)).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("JSON → CSV"));
    }

    #[test]
    fn parse_scalar_table() {
        assert_eq!(parse_scalar("42"), serde_json::json!(42));
        assert_eq!(parse_scalar("-7"), serde_json::json!(-7));
        assert_eq!(parse_scalar("1.5"), serde_json::json!(1.5));
        assert_eq!(parse_scalar("true"), serde_json::json!(true));
        assert_eq!(parse_scalar("false"), serde_json::json!(false));
        assert_eq!(parse_scalar("hello"), serde_json::json!("hello"));
        assert_eq!(parse_scalar(""), serde_json::json!(""));
        // Trim affects parse but original-string is preserved on fallback
        assert_eq!(parse_scalar(" hi "), serde_json::json!(" hi "));
    }
}
