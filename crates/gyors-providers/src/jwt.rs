//! JWT decoder. Takes a `jwt <token>` query, parses three
//! dot-separated segments, base64url-decodes header and payload, and
//! surfaces them as preview-able candidates. Does *not* verify
//! signatures - that needs a key and a library-of-one's-choice; the
//! value here is diagnostic (what's in the token right now?), not
//! cryptographic
//!
//! Candidates emitted (on success):
//!   1. Payload - default action copies the pretty-printed JSON,
//!      preview opens the inline renderer with syntax highlight.
//!   2. Header  - same actions, smaller JSON
//!
//! On failure, a single guidance row names parse error so the
//! user can spot a truncated paste or wrong segment count instantly

use async_trait::async_trait;
use base64::prelude::*;
use chrono::{TimeZone, Utc};
use hmac::{Hmac, Mac};
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use serde_json::Value;
use sha2::Sha256;

pub struct JwtProvider;

const DECODE_KW: &str = "jwt ";
const ENCODE_KW: &str = "newjwt";
const ERROR_ID: &str = "jwt::error";
const NEEDS_SECRET_ID: &str = "jwt::needs-secret";
const MAX_SUBTITLE_CHARS: usize = 120;

type HmacSha256 = Hmac<Sha256>;

#[async_trait]
impl Provider for JwtProvider {
    fn id(&self) -> &str {
        "jwt"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        // `newjwt ...` - build + sign a fresh token
        if pattern == ENCODE_KW || pattern.starts_with(&format!("{ENCODE_KW} ")) {
            let rest = pattern
                .strip_prefix(ENCODE_KW)
                .map(|s| s.trim())
                .unwrap_or("");
            return encode_candidates(rest);
        }
        // `jwt <token>` - decode
        if let Some(rest) = pattern.strip_prefix(DECODE_KW) {
            let token = rest.trim();
            if token.is_empty() {
                return vec![];
            }
            return match parse(token) {
                Ok(parsed) => build_candidates(&parsed, token),
                Err(reason) => vec![error_candidate(&reason)],
            };
        }
        vec![]
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> anyhow::Result<Effect> {
        if id == ERROR_ID {
            return Ok(Effect::None);
        }
        if id == NEEDS_SECRET_ID {
            // One-keystroke setup: prefill `config jwt.secret` so the
            // ConfigProvider's row takes over. Enter on that row walks the
            // user into value input. Everything user needs
            // stays reachable from the keyboard - no Finder hop, no
            // hand-editing config.json required
            return Ok(Effect::SetInput("config jwt.secret ".into()));
        }
        let rest = id
            .strip_prefix("jwt::")
            .ok_or_else(|| anyhow::anyhow!("invalid jwt candidate id: {id}"))?;
        // `jwt::token::<b64(signed-token)>` - freshly signed, no parse needed
        if let Some(encoded) = rest.strip_prefix("token::") {
            let token = decode_id_token(encoded)
                .ok_or_else(|| anyhow::anyhow!("corrupt token payload in id"))?;
            match action {
                "default" => return Ok(Effect::CopyToClipboard(token)),
                "preview" => {
                    let parsed =
                        parse(&token).map_err(|e| anyhow::anyhow!("re-parse self-signed: {e}"))?;
                    let text = format!(
                        "# Token\n\n{token}\n\n# Payload\n\n{}\n\n# Header\n\n{}",
                        serde_json::to_string_pretty(&parsed.payload)?,
                        serde_json::to_string_pretty(&parsed.header)?,
                    );
                    return Ok(Effect::ShowText {
                        text,
                        label: "Signed JWT".into(),
                        language: Some("markdown".into()),
                        editable_path: None,
                    });
                }
                other => anyhow::bail!("unknown action for newjwt: {other}"),
            }
        }
        let (part, token_b64) = rest
            .split_once("::")
            .ok_or_else(|| anyhow::anyhow!("malformed jwt id: {id}"))?;
        let token = decode_id_token(token_b64)
            .ok_or_else(|| anyhow::anyhow!("unable to decode token payload in id"))?;
        let parsed = parse(&token).map_err(|e| anyhow::anyhow!("re-parse jwt: {e}"))?;
        let (value, label) = match part {
            "payload" => (&parsed.payload, "JWT payload"),
            "header" => (&parsed.header, "JWT header"),
            other => anyhow::bail!("unknown jwt candidate part: {other}"),
        };
        let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        match action {
            "default" => Ok(Effect::CopyToClipboard(pretty)),
            "preview" => Ok(Effect::ShowText {
                text: pretty,
                label: label.to_string(),
                language: Some("json".into()),
                editable_path: None,
            }),
            other => anyhow::bail!("unknown action for jwt: {other}"),
        }
    }
}

#[derive(Debug)]
pub struct ParsedJwt {
    pub header: Value,
    pub payload: Value,
}

/// Split token -> decode header + payload. Signature is kept untouched
/// (we never verify it) so we dont drag a crypto dependency in for a
/// tool whose value is inspection
pub fn parse(token: &str) -> Result<ParsedJwt, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected 3 dot-separated segments, got {}",
            parts.len()
        ));
    }
    let header_bytes = b64url_decode(parts[0]).map_err(|e| format!("header base64: {e}"))?;
    let payload_bytes = b64url_decode(parts[1]).map_err(|e| format!("payload base64: {e}"))?;
    let header: Value =
        serde_json::from_slice(&header_bytes).map_err(|e| format!("header JSON: {e}"))?;
    let payload: Value =
        serde_json::from_slice(&payload_bytes).map_err(|e| format!("payload JSON: {e}"))?;
    Ok(ParsedJwt { header, payload })
}

/// Tolerant base64url decode: JWTs may or may not include `=` padding
/// depending on the issuer. Strip trailing `=`, then decode with the
/// NO_PAD alphabet so either form works
fn b64url_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let trimmed = s.trim_end_matches('=');
    BASE64_URL_SAFE_NO_PAD.decode(trimmed)
}

fn build_candidates(parsed: &ParsedJwt, token: &str) -> Vec<Candidate> {
    // URL-safe encode original token into the id so activate
    // path can round-trip - avoids keeping state between query and
    // activate, and sidesteps the `::` splitter
    let id_token = BASE64_URL_SAFE_NO_PAD.encode(token.as_bytes());
    vec![
        payload_candidate(parsed, &id_token),
        header_candidate(parsed, &id_token),
    ]
}

fn payload_candidate(parsed: &ParsedJwt, id_token: &str) -> Candidate {
    let summary = summarize_payload(&parsed.payload);
    Candidate {
        id: format!("jwt::payload::{id_token}"),
        title: "JWT payload".into(),
        subtitle: Some(truncate(&summary, MAX_SUBTITLE_CHARS)),
        icon: Icon::SfSymbol("key.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Copy payload JSON"),
            Action::new("preview", "Preview"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn header_candidate(parsed: &ParsedJwt, id_token: &str) -> Candidate {
    let alg = parsed
        .header
        .get("alg")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let typ = parsed
        .header
        .get("typ")
        .and_then(Value::as_str)
        .unwrap_or("JWT");
    Candidate {
        id: format!("jwt::header::{id_token}"),
        title: "JWT header".into(),
        subtitle: Some(format!("alg={alg} · typ={typ}")),
        icon: Icon::SfSymbol("lock.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Copy header JSON"),
            Action::new("preview", "Preview"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn error_candidate(reason: &str) -> Candidate {
    Candidate {
        id: ERROR_ID.into(),
        title: "Invalid JWT".into(),
        subtitle: Some(truncate(reason, MAX_SUBTITLE_CHARS)),
        icon: Icon::SfSymbol("exclamationmark.triangle.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Human-readable one-liner of the most interesting claims. Covers
/// the 80% - `sub`, `iss`, `exp` with expiry status - and skips
/// noise. Users who want the full thing hit -> to preview
fn summarize_payload(payload: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(sub) = payload.get("sub").and_then(Value::as_str) {
        parts.push(format!("sub={sub}"));
    }
    if let Some(iss) = payload.get("iss").and_then(Value::as_str) {
        parts.push(format!("iss={iss}"));
    }
    if let Some(exp) = payload.get("exp").and_then(Value::as_i64) {
        parts.push(format_expiry(exp));
    } else if let Some(iat) = payload.get("iat").and_then(Value::as_i64) {
        // No exp but an iat - still useful to show the token's age
        parts.push(format!("iat={}", format_timestamp(iat)));
    }
    if parts.is_empty() {
        return "(no standard claims)".into();
    }
    parts.join(" · ")
}

fn format_expiry(exp: i64) -> String {
    let when = format_timestamp(exp);
    let now = Utc::now().timestamp();
    if exp < now {
        format!("exp={when} (expired)")
    } else {
        format!("exp={when}")
    }
}

fn format_timestamp(ts: i64) -> String {
    match Utc.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
        _ => ts.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

fn decode_id_token(encoded: &str) -> Option<String> {
    let bytes = BASE64_URL_SAFE_NO_PAD.decode(encoded).ok()?;
    String::from_utf8(bytes).ok()
}


/// Top-level DX goal: typing `newjwt sub=alice exp=1h iss=foo` should
/// feel like sketching on a whiteboard. Claims mirror JWT standard
/// names; duration shortcuts (`1h`, `2d`, `5m`) beat clicking through
/// a date picker; numeric and boolean values auto-coerce; any unknown
/// key becomes a custom claim
///
/// Live feedback: candidate subtitle shows the signed token's
/// alg + exp in human form so user sees validity at a glance
fn encode_candidates(claims_text: &str) -> Vec<Candidate> {
    let secret = match load_jwt_secret() {
        Some(s) if !s.is_empty() => s,
        _ => return vec![needs_secret_candidate()],
    };

    let now = Utc::now().timestamp();
    let (claims, parse_errors) = parse_claims(claims_text, now);

    // Always stamp iat unless user explicitly overrode it -
    // matches behaviour users expect from jwt.io-style tools
    let mut claims = claims;
    claims.entry("iat").or_insert(Value::from(now));

    let header = serde_json::json!({ "alg": "HS256", "typ": "JWT" });
    let payload = Value::Object(claims.into_iter().collect());

    let token = match sign_hs256(&header, &payload, secret.as_bytes()) {
        Ok(t) => t,
        Err(e) => return vec![error_candidate(&format!("sign: {e}"))],
    };
    let mut out = vec![signed_candidate(&token, &payload)];
    if !parse_errors.is_empty() {
        out.insert(0, claims_warning_candidate(&parse_errors));
    }
    out
}

fn signed_candidate(token: &str, payload: &Value) -> Candidate {
    let id_token = BASE64_URL_SAFE_NO_PAD.encode(token.as_bytes());
    let summary = summarize_payload(payload);
    Candidate {
        id: format!("jwt::token::{id_token}"),
        title: "Signed JWT (HS256)".into(),
        subtitle: Some(truncate(&summary, MAX_SUBTITLE_CHARS)),
        icon: Icon::SfSymbol("key.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Copy signed token"),
            Action::new("preview", "Preview token + claims"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn needs_secret_candidate() -> Candidate {
    Candidate {
        id: NEEDS_SECRET_ID.into(),
        title: "newjwt: set jwt.secret first".into(),
        subtitle: Some("Press ↵ to prefill `set jwt.secret <value>`, or edit config.json".into()),
        icon: Icon::SfSymbol("exclamationmark.shield".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Prefill setup")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn claims_warning_candidate(errors: &[String]) -> Candidate {
    let joined = errors.join(" · ");
    Candidate {
        id: "jwt::warning".into(),
        title: format!("Couldn't parse {} claim(s)", errors.len()),
        subtitle: Some(truncate(&joined, MAX_SUBTITLE_CHARS)),
        icon: Icon::SfSymbol("exclamationmark.triangle".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Split a `k1=v1 k2=v2` string into a claim map. Values are coerced
/// by shape: `true/false` -> bool, integer-looking -> number, otherwise
/// string. Duration shortcuts (`1h`, `30m`, `2d`) on `exp`/`nbf`/`iat`
/// resolve relative to `now_ts`
///
/// Returns (claims, parse_errors). A parse error *never* prevents
/// signing - we drop the bad pair and surface error as a warning
/// row so user sees both the token and what was wrong in a single
/// frame
pub fn parse_claims(text: &str, now_ts: i64) -> (serde_json::Map<String, Value>, Vec<String>) {
    let mut claims = serde_json::Map::new();
    let mut errs: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        let Some((key, raw)) = token.split_once('=') else {
            errs.push(format!("`{token}`: expected key=value"));
            continue;
        };
        let key = key.trim();
        let raw = raw.trim();
        if key.is_empty() {
            errs.push("empty claim name".into());
            continue;
        }
        let value = coerce_claim_value(key, raw, now_ts);
        claims.insert(key.to_string(), value);
    }
    (claims, errs)
}

fn coerce_claim_value(key: &str, raw: &str, now_ts: i64) -> Value {
    // Time-like claims accept durations first: `exp=1h` is almost
    // always what user meant. Absolute unix seconds still work
    if matches!(key, "exp" | "nbf" | "iat") {
        if let Some(d) = parse_duration(raw) {
            return Value::from(now_ts + d);
        }
    }
    if raw == "now" {
        return Value::from(now_ts);
    }
    if raw == "true" {
        return Value::Bool(true);
    }
    if raw == "false" {
        return Value::Bool(false);
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Value::from(n);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::String(raw.to_string()));
    }
    // Strip surrounding quotes so `iss="example.com"` works too
    let s = raw.trim_matches(|c| c == '"' || c == '\'');
    Value::String(s.to_string())
}

/// Parse `1s`, `5m`, `2h`, `3d`, `1w` into seconds. Returns None on
/// anything else - caller falls back to other coercions
pub fn parse_duration(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let (digits, unit) = s.split_at(
        s.chars()
            .take_while(|c| c.is_ascii_digit())
            .map(|c| c.len_utf8())
            .sum(),
    );
    if digits.is_empty() {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    let multiplier: i64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    Some(n.saturating_mul(multiplier))
}

/// HS256 signer: base64url(header) + "." + base64url(payload) signed
/// with HMAC-SHA256 over same dot-joined bytes, appended as the
/// third segment
fn sign_hs256(header: &Value, payload: &Value, secret: &[u8]) -> Result<String, String> {
    let header_b64 = encode_segment(header).map_err(|e| format!("encode header: {e}"))?;
    let payload_b64 = encode_segment(payload).map_err(|e| format!("encode payload: {e}"))?;
    let signing_input = format!("{header_b64}.{payload_b64}");
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(secret).map_err(|e| format!("hmac init: {e}"))?;
    mac.update(signing_input.as_bytes());
    let sig = mac.finalize().into_bytes();
    let sig_b64 = BASE64_URL_SAFE_NO_PAD.encode(sig);
    Ok(format!("{signing_input}.{sig_b64}"))
}

fn encode_segment(value: &Value) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(value)?;
    Ok(BASE64_URL_SAFE_NO_PAD.encode(&bytes))
}

/// Read `jwt.secret` from `~/Library/Application Support/Gyors/config.json`.
/// Returns None when the key is missing so provider can emit the
/// "set me up" candidate instead of signing a token with an empty key
/// (which would be technically valid but cryptographically useless)
fn load_jwt_secret() -> Option<String> {
    let cfg = crate::config::load_config();
    crate::config::get_dotted(&cfg, "jwt.secret")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)] // test serialization uses sync Mutex on $HOME
mod tests {
    use super::*;

    fn encode_segment<T: serde::Serialize>(value: &T) -> String {
        let bytes = serde_json::to_vec(value).unwrap();
        BASE64_URL_SAFE_NO_PAD.encode(bytes)
    }

    fn make_token(header: serde_json::Value, payload: serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            encode_segment(&header),
            encode_segment(&payload),
            "fakesignaturebytes"
        )
    }

    #[test]
    fn parse_rejects_non_three_part_token() {
        let err = parse("only.two").unwrap_err();
        assert!(err.contains("3 dot-separated"), "got {err}");
    }

    #[test]
    fn parse_accepts_padded_and_unpadded_segments() {
        // Most JWTs omit padding but some issuers include it. Both
        // must decode - this pins the tolerant path
        let header = serde_json::json!({"alg":"HS256","typ":"JWT"});
        let payload = serde_json::json!({"sub":"u"});
        let no_pad = format!(
            "{}.{}.sig",
            BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
            BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap()),
        );
        let padded = format!(
            "{}.{}.sig",
            BASE64_URL_SAFE.encode(serde_json::to_vec(&header).unwrap()),
            BASE64_URL_SAFE.encode(serde_json::to_vec(&payload).unwrap()),
        );
        assert!(parse(&no_pad).is_ok());
        assert!(parse(&padded).is_ok());
    }

    #[test]
    fn parse_returns_decoded_header_and_payload() {
        let token = make_token(
            serde_json::json!({"alg":"HS256","typ":"JWT"}),
            serde_json::json!({"sub":"alice","iss":"example"}),
        );
        let out = parse(&token).unwrap();
        assert_eq!(out.header.get("alg").and_then(Value::as_str), Some("HS256"));
        assert_eq!(
            out.payload.get("sub").and_then(Value::as_str),
            Some("alice")
        );
    }

    #[test]
    fn parse_reports_garbage_base64() {
        let err = parse("###.###.###").unwrap_err();
        assert!(err.contains("base64"), "got {err}");
    }

    #[test]
    fn parse_reports_bad_header_json() {
        // Valid base64 but decoded bytes aren't JSON
        let bad = BASE64_URL_SAFE_NO_PAD.encode(b"not json at all");
        let payload = BASE64_URL_SAFE_NO_PAD.encode(br#"{"sub":"a"}"#);
        let token = format!("{bad}.{payload}.sig");
        let err = parse(&token).unwrap_err();
        assert!(err.contains("header JSON"), "got {err}");
    }

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = JwtProvider;
        assert!(p.query(&Query::new("eyJhbGc")).await.is_empty());
    }

    #[tokio::test]
    async fn keyword_with_empty_tail_no_candidates() {
        let p = JwtProvider;
        assert!(p.query(&Query::new("jwt ")).await.is_empty());
    }

    #[tokio::test]
    async fn valid_token_emits_payload_and_header_rows() {
        let p = JwtProvider;
        let token = make_token(
            serde_json::json!({"alg":"HS256","typ":"JWT"}),
            serde_json::json!({"sub":"alice","iss":"example","exp":i64::MAX / 2}),
        );
        let out = p.query(&Query::new(format!("jwt {token}"))).await;
        assert_eq!(out.len(), 2, "payload + header");
        assert_eq!(out[0].title, "JWT payload");
        assert_eq!(out[1].title, "JWT header");
        let header_sub = out[1].subtitle.as_deref().unwrap();
        assert!(header_sub.contains("alg=HS256"));
    }

    #[tokio::test]
    async fn invalid_token_emits_single_error_row() {
        let p = JwtProvider;
        let out = p.query(&Query::new("jwt garbage")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, ERROR_ID);
    }

    #[tokio::test]
    async fn activate_payload_copies_pretty_json() {
        let p = JwtProvider;
        let token = make_token(
            serde_json::json!({"alg":"none"}),
            serde_json::json!({"sub":"bob"}),
        );
        let out = p.query(&Query::new(format!("jwt {token}"))).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => {
                assert!(s.contains('\n'), "pretty-printed: {s}");
                assert!(s.contains("\"sub\""));
                assert!(s.contains("\"bob\""));
            }
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_preview_shows_json_with_syntax_language() {
        let p = JwtProvider;
        let token = make_token(
            serde_json::json!({"alg":"HS256"}),
            serde_json::json!({"sub":"c"}),
        );
        let out = p.query(&Query::new(format!("jwt {token}"))).await;
        let effect = p.activate(&out[0].id, "preview").await.unwrap();
        match effect {
            Effect::ShowText {
                text,
                label,
                language,
                editable_path,
            } => {
                assert!(text.contains("\"sub\""));
                assert_eq!(label, "JWT payload");
                assert_eq!(language.as_deref(), Some("json"));
                assert!(editable_path.is_none());
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_header_also_works() {
        let p = JwtProvider;
        let token = make_token(
            serde_json::json!({"alg":"RS256","typ":"JWT"}),
            serde_json::json!({}),
        );
        let out = p.query(&Query::new(format!("jwt {token}"))).await;
        let effect = p.activate(&out[1].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => {
                assert!(s.contains("\"alg\""));
                assert!(s.contains("\"RS256\""));
            }
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_error_candidate_is_noop() {
        let p = JwtProvider;
        let effect = p.activate(&ERROR_ID.to_string(), "default").await.unwrap();
        assert!(matches!(effect, Effect::None));
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = JwtProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn summarize_shows_sub_iss_exp_status() {
        let past = serde_json::json!({
            "sub":"u", "iss":"example", "exp": 0i64,
        });
        let s = summarize_payload(&past);
        assert!(s.contains("sub=u"));
        assert!(s.contains("iss=example"));
        assert!(s.contains("(expired)"), "got {s}");
    }

    #[test]
    fn summarize_falls_back_for_empty_claims() {
        let s = summarize_payload(&serde_json::json!({}));
        assert!(s.contains("no standard claims"));
    }

    #[test]
    fn summarize_shows_iat_when_no_exp() {
        let payload = serde_json::json!({"iat": 1_600_000_000i64});
        let s = summarize_payload(&payload);
        assert!(s.contains("iat="), "got {s}");
    }


    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("1s"), Some(1));
        assert_eq!(parse_duration("30m"), Some(1800));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("3d"), Some(259_200));
        assert_eq!(parse_duration("1w"), Some(604_800));
    }

    #[test]
    fn parse_duration_rejects_garbage() {
        assert!(parse_duration("hello").is_none());
        assert!(parse_duration("10x").is_none());
        assert!(parse_duration("").is_none());
    }

    #[test]
    fn parse_claims_string_duration_bool_coerce() {
        let (claims, errs) = parse_claims("sub=alice exp=1h role=admin vip=true count=42", 1_000);
        assert!(errs.is_empty(), "clean input: {errs:?}");
        assert_eq!(claims.get("sub").unwrap().as_str(), Some("alice"));
        assert_eq!(claims.get("exp").unwrap().as_i64(), Some(1_000 + 3_600));
        assert_eq!(claims.get("role").unwrap().as_str(), Some("admin"));
        assert_eq!(claims.get("vip").unwrap().as_bool(), Some(true));
        assert_eq!(claims.get("count").unwrap().as_i64(), Some(42));
    }

    #[test]
    fn parse_claims_reports_malformed_but_keeps_going() {
        let (claims, errs) = parse_claims("sub=alice notapair exp=1h =nobare", 0);
        assert!(claims.contains_key("sub"));
        assert!(claims.contains_key("exp"));
        assert_eq!(errs.len(), 2, "two malformed tokens: {errs:?}");
    }

    #[test]
    fn parse_claims_absolute_now() {
        let (claims, _) = parse_claims("iat=now", 1_700_000_000);
        assert_eq!(claims.get("iat").unwrap().as_i64(), Some(1_700_000_000));
    }

    #[test]
    fn parse_claims_strips_quoted_strings() {
        let (claims, _) = parse_claims("iss=\"example.com\"", 0);
        assert_eq!(claims.get("iss").unwrap().as_str(), Some("example.com"));
    }

    #[test]
    fn sign_hs256_produces_verifiable_three_part_token() {
        let header = serde_json::json!({"alg":"HS256","typ":"JWT"});
        let payload = serde_json::json!({"sub":"alice","iat":1_000});
        let token = sign_hs256(&header, &payload, b"supersecret").unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "three segments: {token}");
        // Round-trip through our own parse path
        let back = parse(&token).unwrap();
        assert_eq!(
            back.payload.get("sub").and_then(Value::as_str),
            Some("alice")
        );
        // Verify HMAC by re-signing with same secret and comparing
        let rebuilt = sign_hs256(&header, &payload, b"supersecret").unwrap();
        assert_eq!(rebuilt, token, "HS256 is deterministic per (input,secret)");
    }

    #[test]
    fn sign_hs256_changes_with_secret() {
        let header = serde_json::json!({"alg":"HS256"});
        let payload = serde_json::json!({"sub":"x"});
        let a = sign_hs256(&header, &payload, b"one").unwrap();
        let b = sign_hs256(&header, &payload, b"two").unwrap();
        assert_ne!(a, b, "different secrets → different signatures");
    }

    /// HOME overrides are process-wide, so tests that touch the real
    /// config path must run serially. Shared mutex across all
    /// config-sensitive jwt tests in this module
    fn home_override_guard() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[tokio::test]
    async fn newjwt_without_secret_emits_setup_row() {
        let _guard = home_override_guard();
        let td = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("HOME", td.path());
        }
        let p = JwtProvider;
        let out = p.query(&Query::new("newjwt sub=alice")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, NEEDS_SECRET_ID);
    }

    #[tokio::test]
    async fn newjwt_with_secret_emits_signed_token_row() {
        let _guard = home_override_guard();
        let td = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("HOME", td.path());
        }
        let gyors_dir = td
            .path()
            .join("Library")
            .join("Application Support")
            .join("Gyors");
        std::fs::create_dir_all(&gyors_dir).unwrap();
        std::fs::write(
            gyors_dir.join("config.json"),
            r#"{"jwt":{"secret":"testkey"}}"#,
        )
        .unwrap();

        let p = JwtProvider;
        let out = p.query(&Query::new("newjwt sub=alice exp=1h")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Signed JWT (HS256)");
        assert!(out[0].id.starts_with("jwt::token::"));
    }

    #[tokio::test]
    async fn newjwt_activate_default_copies_token() {
        let _guard = home_override_guard();
        let td = tempfile::TempDir::new().unwrap();
        unsafe {
            std::env::set_var("HOME", td.path());
        }
        let gyors_dir = td
            .path()
            .join("Library")
            .join("Application Support")
            .join("Gyors");
        std::fs::create_dir_all(&gyors_dir).unwrap();
        std::fs::write(
            gyors_dir.join("config.json"),
            r#"{"jwt":{"secret":"testkey"}}"#,
        )
        .unwrap();

        let p = JwtProvider;
        let out = p.query(&Query::new("newjwt sub=alice")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(token) => {
                assert_eq!(token.split('.').count(), 3);
                let parsed = parse(&token).unwrap();
                assert_eq!(
                    parsed.payload.get("sub").and_then(Value::as_str),
                    Some("alice")
                );
                assert!(parsed.payload.get("iat").is_some(), "iat auto-stamped");
            }
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn newjwt_needs_secret_activation_primes_setup() {
        let p = JwtProvider;
        let effect = p
            .activate(&NEEDS_SECRET_ID.to_string(), "default")
            .await
            .unwrap();
        match effect {
            Effect::SetInput(s) => {
                assert!(s.starts_with("config "), "routes via config: {s:?}");
                assert!(s.contains("jwt.secret"), "carries the key name: {s:?}");
            }
            other => panic!("expected SetInput, got {other:?}"),
        }
    }
}
