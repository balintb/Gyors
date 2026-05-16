//! Encoding / hashing utilities. Keyword-triggered, result copyable
//!
//! | keyword                      | operation                          |
//! |------------------------------|------------------------------------|
//! | `b64 <text>`                 | base64 encode                      |
//! | `b64d <text>`                | base64 decode (UTF-8)              |
//! | `url <text>`                 | URL-encode                         |
//! | `urld <text>`                | URL-decode                         |
//! | `htmlescape <text>`          | escape HTML entities               |
//! | `htmlunescape <text>`        | decode HTML entities               |
//! | `jsonescape <text>`          | escape for inclusion in JSON       |
//! | `jsonunescape <text>`        | decode JSON-string escapes         |
//! | `rot13 <text>`               | ROT13 (self-inverse Caesar shift)  |
//! | `caesar <n> <text>`          | Caesar cipher with arbitrary shift |
//! | `hmac <algo> <key> <text>`   | HMAC of <text> using <key>         |
//! | `md5 <text>`                 | MD5 hex digest                     |
//! | `sha1 <text>`                | SHA-1 hex digest                   |
//! | `sha256 <text>`              | SHA-256 hex digest                 |

use async_trait::async_trait;
use base64::prelude::*;
use gyors_core::{
    Action, Candidate, CandidateId, CandidateKind, Effect, Icon, KeywordSpec, Provider, Query,
};
use hmac::{Hmac, Mac};
use sha1::Digest as _;

pub struct EncodingProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Base64Encode,
    Base64Decode,
    UrlEncode,
    UrlDecode,
    HtmlEscape,
    HtmlUnescape,
    JsonEscape,
    JsonUnescape,
    Rot13,
    Md5,
    Sha1,
    Sha256,
    Sha3_256,
    Sha3_512,
    Blake3,
}

impl Op {
    fn label(self) -> &'static str {
        match self {
            Op::Base64Encode => "base64",
            Op::Base64Decode => "base64 → text",
            Op::UrlEncode => "URL-encoded",
            Op::UrlDecode => "URL-decoded",
            Op::HtmlEscape => "HTML-escaped",
            Op::HtmlUnescape => "HTML-unescaped",
            Op::JsonEscape => "JSON-escaped",
            Op::JsonUnescape => "JSON-unescaped",
            Op::Rot13 => "ROT13",
            Op::Md5 => "MD5",
            Op::Sha1 => "SHA-1",
            Op::Sha256 => "SHA-256",
            Op::Sha3_256 => "SHA3-256",
            Op::Sha3_512 => "SHA3-512",
            Op::Blake3 => "BLAKE3",
        }
    }
}

fn parse_op(kw: &str) -> Option<Op> {
    match kw {
        "b64" | "base64" => Some(Op::Base64Encode),
        "b64d" | "base64d" => Some(Op::Base64Decode),
        "url" => Some(Op::UrlEncode),
        "urld" => Some(Op::UrlDecode),
        "htmlescape" | "htmlencode" | "html" => Some(Op::HtmlEscape),
        "htmlunescape" | "htmldecode" | "htmld" => Some(Op::HtmlUnescape),
        "jsonescape" | "jsonencode" => Some(Op::JsonEscape),
        "jsonunescape" | "jsondecode" => Some(Op::JsonUnescape),
        "rot13" => Some(Op::Rot13),
        "md5" => Some(Op::Md5),
        "sha1" => Some(Op::Sha1),
        "sha256" => Some(Op::Sha256),
        "sha3" | "sha3-256" => Some(Op::Sha3_256),
        "sha3-512" => Some(Op::Sha3_512),
        "blake3" => Some(Op::Blake3),
        _ => None,
    }
}

fn apply_op(op: Op, input: &str) -> Option<String> {
    match op {
        Op::Base64Encode => Some(BASE64_STANDARD.encode(input.as_bytes())),
        Op::Base64Decode => {
            let bytes = BASE64_STANDARD.decode(input).ok()?;
            String::from_utf8(bytes).ok()
        }
        Op::UrlEncode => Some(urlencoding::encode(input).into_owned()),
        Op::UrlDecode => urlencoding::decode(input).ok().map(|c| c.into_owned()),
        Op::HtmlEscape => Some(html_escape(input)),
        Op::HtmlUnescape => Some(html_unescape(input)),
        Op::JsonEscape => Some(json_escape(input)),
        Op::JsonUnescape => json_unescape(input),
        Op::Rot13 => Some(caesar_shift(input, 13)),
        Op::Md5 => Some(format!("{:x}", md5::compute(input.as_bytes()))),
        Op::Sha1 => {
            let mut h = sha1::Sha1::new();
            h.update(input.as_bytes());
            Some(hex::encode(h.finalize()))
        }
        Op::Sha256 => {
            let mut h = sha2::Sha256::new();
            h.update(input.as_bytes());
            Some(hex::encode(h.finalize()))
        }
        Op::Sha3_256 => {
            use sha3::Digest;
            let mut h = sha3::Sha3_256::new();
            h.update(input.as_bytes());
            Some(hex::encode(h.finalize()))
        }
        Op::Sha3_512 => {
            use sha3::Digest;
            let mut h = sha3::Sha3_512::new();
            h.update(input.as_bytes());
            Some(hex::encode(h.finalize()))
        }
        Op::Blake3 => Some(blake3::hash(input.as_bytes()).to_hex().to_string()),
    }
}

/// Escape the five characters that are unsafe in HTML body / attribute
/// contexts. Use `&#39;` for the apostrophe (rather than `&apos;`) since
/// `&apos;` isn't a valid HTML 4 named entity
fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Decode the common named entities plus decimal/hex numeric character
/// references. Unknown entities pass through verbatim - the philosophy is
/// "useful for ad-hoc paste-from-the-web", not "fully spec-compliant
/// browser parser."
fn html_unescape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(semi) = input[i + 1..].find(';').map(|p| i + 1 + p) {
                let entity = &input[i + 1..semi];
                if let Some(ch) = decode_entity(entity) {
                    out.push(ch);
                    i = semi + 1;
                    continue;
                }
            }
        }
        // Fallback: copy a single Unicode scalar - not just a byte -
        // so we dont slice through a multibyte char boundary
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    if let Some(rest) = entity.strip_prefix('#') {
        let code = if let Some(hex) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            rest.parse::<u32>().ok()?
        };
        return char::from_u32(code);
    }
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{00A0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '-',
        "ndash" => '–',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "laquo" => '«',
        "raquo" => '»',
        "deg" => '°',
        "times" => '×',
        "divide" => '÷',
        "plusmn" => '±',
        "para" => '¶',
        "sect" => '§',
        "bull" => '•',
        _ => return None,
    })
}

/// JSON-string escape: returns value as it would appear *between* the
/// quotes of a JSON string literal. `serde_json::to_string` produces the
/// quoted form; we strip surrounding quotes
fn json_escape(input: &str) -> String {
    let quoted = serde_json::to_string(input).unwrap_or_else(|_| format!("\"{input}\""));
    quoted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .map(|s| s.to_string())
        .unwrap_or(quoted)
}

/// Inverse of `json_escape`. Accepts both the bare escaped form and the
/// quoted form (with or without surrounding `"`)
fn json_unescape(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let quoted_form = if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        trimmed.to_string()
    } else {
        format!("\"{trimmed}\"")
    };
    serde_json::from_str::<String>(&quoted_form).ok()
}

/// Caesar cipher: shift each ASCII letter by `n` positions, preserving
/// case. Non-letters pass through. `n` is reduced modulo 26 and may be
/// negative
pub fn caesar_shift(input: &str, n: i32) -> String {
    // Reduce shift to [0, 26) so wrap-around math always works
    let n = n.rem_euclid(26);
    let n = n as u8;
    input
        .chars()
        .map(|c| match c {
            'A'..='Z' => (((c as u8 - b'A') + n) % 26 + b'A') as char,
            'a'..='z' => (((c as u8 - b'a') + n) % 26 + b'a') as char,
            other => other,
        })
        .collect()
}

/// Render candidate for `caesar <n> <text>`. Empty/invalid input
/// returns no candidates rather than an error row - keeps dropdown
/// quiet while user is still typing
fn caesar_candidate(rest: &str) -> Vec<Candidate> {
    let (n_str, text) = match rest.split_once(char::is_whitespace) {
        Some(parts) => parts,
        None => return vec![],
    };
    let n: i32 = match n_str.parse() {
        Ok(n) => n,
        Err(_) => return vec![],
    };
    let text = text.trim();
    if text.is_empty() {
        return vec![];
    }
    let result = caesar_shift(text, n);
    vec![Candidate {
        id: format!("encode::{result}"),
        title: truncate(&result, 120),
        subtitle: Some(format!("Caesar shift {n} · {}", truncate(text, 60))),
        icon: Icon::SfSymbol("key.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }]
}

/// Render candidate for `hmac <algo> <key> <text>`. The algo token
/// is `sha1` / `sha256` / `sha512`; key is a single whitespace-delimited
/// token; everything after the key is message
fn hmac_candidates(rest: &str) -> Vec<Candidate> {
    let parts: Vec<&str> = rest.splitn(3, char::is_whitespace).collect();
    if parts.len() < 3 {
        return vec![];
    }
    let algo = parts[0];
    let key = parts[1];
    let text = parts[2].trim();
    if key.is_empty() || text.is_empty() {
        return vec![];
    }
    let result = match algo {
        "sha1" => hmac_sha1(key, text),
        "sha256" => hmac_sha256(key, text),
        "sha512" => hmac_sha512(key, text),
        _ => return vec![],
    };
    let Some(hex) = result else { return vec![] };
    vec![Candidate {
        id: format!("encode::{hex}"),
        title: truncate(&hex, 120),
        subtitle: Some(format!("HMAC-{} · key={}", algo.to_uppercase(), truncate(key, 30))),
        icon: Icon::SfSymbol("key.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }]
}

pub fn hmac_sha1(key: &str, message: &str) -> Option<String> {
    let mut mac = <Hmac<sha1::Sha1>>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(message.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

pub fn hmac_sha256(key: &str, message: &str) -> Option<String> {
    let mut mac = <Hmac<sha2::Sha256>>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(message.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

pub fn hmac_sha512(key: &str, message: &str) -> Option<String> {
    let mut mac = <Hmac<sha2::Sha512>>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(message.as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

/// Source of truth for encoding provider's keywords. Migrated
/// to `Provider::keywords()` as the proof of concept for the
/// registry SSOT refactor - `hints.rs` will eventually walk this
/// list instead of duplicating the data. Registry-tests pin the
/// shape so future entries here automatically surface in the hint
/// generator
const KEYWORDS: &[KeywordSpec] = &[
    KeywordSpec {
        keyword: "b64",
        aliases: &["base64"],
        syntax: "b64 <text>",
        description: "Base64 encode",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "b64d",
        aliases: &["base64d"],
        syntax: "b64d <text>",
        description: "Base64 decode",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "url",
        aliases: &[],
        syntax: "url <text>",
        description: "URL encode",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "urld",
        aliases: &[],
        syntax: "urld <text>",
        description: "URL decode",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "htmlescape",
        aliases: &["htmlencode", "html"],
        syntax: "htmlescape <text>",
        description: "Escape HTML entities (& < > \" ')",
        symbol: "chevron.left.slash.chevron.right",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "htmlunescape",
        aliases: &["htmldecode", "htmld"],
        syntax: "htmlunescape <text>",
        description: "Decode HTML entities back to characters",
        symbol: "chevron.left.slash.chevron.right",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "jsonescape",
        aliases: &["jsonencode"],
        syntax: "jsonescape <text>",
        description: "Escape text for JSON string literal",
        symbol: "curlybraces",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "jsonunescape",
        aliases: &["jsondecode"],
        syntax: "jsonunescape <text>",
        description: "Decode JSON-string escapes",
        symbol: "curlybraces",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "rot13",
        aliases: &[],
        syntax: "rot13 <text>",
        description: "ROT13 cipher (self-inverse)",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "caesar",
        aliases: &[],
        syntax: "caesar <n> <text>",
        description: "Caesar cipher with shift n",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "hmac",
        aliases: &[],
        syntax: "hmac <algo> <key> <text>",
        description: "HMAC-SHA1/256/512 of <text> using <key>",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "md5",
        aliases: &[],
        syntax: "md5 <text>",
        description: "MD5 hex digest",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "sha1",
        aliases: &[],
        syntax: "sha1 <text>",
        description: "SHA-1 hex digest",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "sha256",
        aliases: &[],
        syntax: "sha256 <text>",
        description: "SHA-256 hex digest",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "sha3",
        aliases: &["sha3-256"],
        syntax: "sha3 <text>",
        description: "SHA3-256 hex digest",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "sha3-512",
        aliases: &[],
        syntax: "sha3-512 <text>",
        description: "SHA3-512 hex digest",
        symbol: "key.fill",
        self_contained: false,
    },
    KeywordSpec {
        keyword: "blake3",
        aliases: &[],
        syntax: "blake3 <text>",
        description: "BLAKE3 hex digest",
        symbol: "key.fill",
        self_contained: false,
    },
];

#[async_trait]
impl Provider for EncodingProvider {
    fn id(&self) -> &str {
        "encode"
    }

    fn keywords(&self) -> &'static [KeywordSpec] {
        KEYWORDS
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        // Caesar takes two positional args (`<n> <text>`); shoehorning it
        // into keyword+text Op table would require a 3-token split,
        // so handle it as a separate code path
        if let Some(rest) = pattern.strip_prefix("caesar ") {
            return caesar_candidate(rest.trim());
        }
        // HMAC needs three args: algo, key, text. Same reasoning
        if let Some(rest) = pattern.strip_prefix("hmac ") {
            return hmac_candidates(rest.trim());
        }

        let Some((kw, input)) = pattern.split_once(char::is_whitespace) else {
            return vec![];
        };
        let Some(op) = parse_op(kw) else {
            return vec![];
        };
        let input = input.trim();
        if input.is_empty() {
            return vec![];
        }
        let Some(result) = apply_op(op, input) else {
            return vec![];
        };
        vec![Candidate {
            id: format!("encode::{result}"),
            title: truncate(&result, 120),
            subtitle: Some(format!("{} · {}", op.label(), truncate(input, 60))),
            icon: Icon::SfSymbol("key.fill".into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("Copy")],
            search_text: String::new(),
            bypass_rank: true,
        }]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let content = id
            .strip_prefix("encode::")
            .ok_or_else(|| anyhow::anyhow!("invalid encode candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(content.to_string()))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.into()
    } else {
        let mut it = s.chars();
        let head: String = it.by_ref().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_keyword_no_candidate() {
        let p = EncodingProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn unknown_keyword_no_candidate() {
        let p = EncodingProvider;
        assert!(p.query(&Query::new("xyz hello")).await.is_empty());
    }

    #[tokio::test]
    async fn keyword_with_no_input_no_candidate() {
        let p = EncodingProvider;
        assert!(p.query(&Query::new("b64 ")).await.is_empty());
        assert!(p.query(&Query::new("b64    ")).await.is_empty());
    }

    #[tokio::test]
    async fn base64_roundtrip() {
        let p = EncodingProvider;
        let enc = p.query(&Query::new("b64 hello world")).await;
        assert_eq!(enc.len(), 1);
        assert_eq!(enc[0].title, "aGVsbG8gd29ybGQ=");

        let dec = p.query(&Query::new("b64d aGVsbG8gd29ybGQ=")).await;
        assert_eq!(dec.len(), 1);
        assert_eq!(dec[0].title, "hello world");
    }

    #[tokio::test]
    async fn base64_decode_invalid_yields_no_candidate() {
        let p = EncodingProvider;
        assert!(p.query(&Query::new("b64d !!!not-base64!!!")).await.is_empty());
    }

    #[tokio::test]
    async fn url_roundtrip() {
        let p = EncodingProvider;
        let enc = p.query(&Query::new("url hello world & more")).await;
        assert_eq!(enc.len(), 1);
        assert!(enc[0].title.contains("%20"));
        assert!(enc[0].title.contains("%26"));

        let dec = p.query(&Query::new(format!("urld {}", enc[0].title))).await;
        assert_eq!(dec[0].title, "hello world & more");
    }

    #[tokio::test]
    async fn md5_is_correct() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("md5 hello")).await;
        // Known: md5("hello") = 5d41402abc4b2a76b9719d911017c592
        assert_eq!(out[0].title, "5d41402abc4b2a76b9719d911017c592");
    }

    #[tokio::test]
    async fn sha1_is_correct() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("sha1 hello")).await;
        // Known: sha1("hello") = aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d
        assert_eq!(out[0].title, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
    }

    #[tokio::test]
    async fn sha256_is_correct() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("sha256 hello")).await;
        // Known: sha256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            out[0].title,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[tokio::test]
    async fn activate_copies_result() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("b64 hi")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "aGk="),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn base64_aliases_both_work() {
        let p = EncodingProvider;
        let short = p.query(&Query::new("b64 hi")).await;
        let long = p.query(&Query::new("base64 hi")).await;
        assert_eq!(short[0].title, long[0].title);
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = EncodingProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }


    #[test]
    fn parse_op_matches_known_keywords() {
        assert_eq!(parse_op("b64"), Some(Op::Base64Encode));
        assert_eq!(parse_op("base64"), Some(Op::Base64Encode));
        assert_eq!(parse_op("b64d"), Some(Op::Base64Decode));
        assert_eq!(parse_op("url"), Some(Op::UrlEncode));
        assert_eq!(parse_op("urld"), Some(Op::UrlDecode));
        assert_eq!(parse_op("htmlescape"), Some(Op::HtmlEscape));
        assert_eq!(parse_op("htmlencode"), Some(Op::HtmlEscape));
        assert_eq!(parse_op("html"), Some(Op::HtmlEscape));
        assert_eq!(parse_op("htmlunescape"), Some(Op::HtmlUnescape));
        assert_eq!(parse_op("htmldecode"), Some(Op::HtmlUnescape));
        assert_eq!(parse_op("htmld"), Some(Op::HtmlUnescape));
        assert_eq!(parse_op("jsonescape"), Some(Op::JsonEscape));
        assert_eq!(parse_op("jsonencode"), Some(Op::JsonEscape));
        assert_eq!(parse_op("jsonunescape"), Some(Op::JsonUnescape));
        assert_eq!(parse_op("jsondecode"), Some(Op::JsonUnescape));
        assert_eq!(parse_op("md5"), Some(Op::Md5));
        assert_eq!(parse_op("sha1"), Some(Op::Sha1));
        assert_eq!(parse_op("sha256"), Some(Op::Sha256));
        assert_eq!(parse_op("sha3"), Some(Op::Sha3_256));
        assert_eq!(parse_op("sha3-256"), Some(Op::Sha3_256));
        assert_eq!(parse_op("sha3-512"), Some(Op::Sha3_512));
        assert_eq!(parse_op("blake3"), Some(Op::Blake3));
    }


    #[tokio::test]
    async fn htmlescape_handles_all_five() {
        let p = EncodingProvider;
        let out = p.query(&Query::new(r#"htmlescape <a href="x">&'</a>"#)).await;
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].title,
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[tokio::test]
    async fn htmlescape_alias_html() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("html <b>hi</b>")).await;
        assert_eq!(out[0].title, "&lt;b&gt;hi&lt;/b&gt;");
    }

    #[tokio::test]
    async fn htmlunescape_named_entities() {
        let p = EncodingProvider;
        let out = p
            .query(&Query::new("htmlunescape &lt;b&gt;hi&amp;bye&lt;/b&gt;"))
            .await;
        assert_eq!(out[0].title, "<b>hi&bye</b>");
    }

    #[tokio::test]
    async fn htmlunescape_numeric_decimal() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("htmlunescape &#65;&#66;&#67;")).await;
        assert_eq!(out[0].title, "ABC");
    }

    #[tokio::test]
    async fn htmlunescape_numeric_hex() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("htmlunescape &#x41;&#X42;")).await;
        assert_eq!(out[0].title, "AB");
    }

    #[tokio::test]
    async fn htmlunescape_unknown_entity_passes_through() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("htmlunescape a&unknown;b")).await;
        assert_eq!(out[0].title, "a&unknown;b");
    }

    #[tokio::test]
    async fn htmlescape_roundtrip() {
        let p = EncodingProvider;
        let escaped = p.query(&Query::new(r#"htmlescape <hi & "bye">"#)).await;
        let back = p.query(&Query::new(format!("htmlunescape {}", escaped[0].title))).await;
        assert_eq!(back[0].title, r#"<hi & "bye">"#);
    }

    #[test]
    fn html_escape_pure() {
        assert_eq!(html_escape("plain"), "plain");
        assert_eq!(html_escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#39;");
    }

    #[test]
    fn html_unescape_pure_named() {
        assert_eq!(html_unescape("&amp;&lt;&gt;&quot;&#39;"), "&<>\"'");
        assert_eq!(html_unescape("&copy; &mdash;"), "© -");
    }

    #[test]
    fn html_unescape_handles_multibyte_input() {
        // Dont slice through a UTF-8 boundary while scanning for `&`
        assert_eq!(html_unescape("café &amp; tea"), "café & tea");
    }


    #[tokio::test]
    async fn jsonescape_quotes_and_newlines() {
        let p = EncodingProvider;
        let out = p
            .query(&Query::new("jsonescape say \"hello\""))
            .await;
        assert_eq!(out[0].title, r#"say \"hello\""#);
    }

    #[tokio::test]
    async fn jsonescape_passes_through_safe_chars() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("jsonescape plain text")).await;
        assert_eq!(out[0].title, "plain text");
    }

    #[tokio::test]
    async fn jsonunescape_handles_escapes() {
        let p = EncodingProvider;
        let out = p
            .query(&Query::new(r#"jsonunescape say \"hi\""#))
            .await;
        assert_eq!(out[0].title, "say \"hi\"");
    }

    #[tokio::test]
    async fn jsonunescape_with_quoted_input() {
        let p = EncodingProvider;
        let out = p.query(&Query::new(r#"jsonunescape "quoted text""#)).await;
        assert_eq!(out[0].title, "quoted text");
    }

    #[tokio::test]
    async fn jsonunescape_invalid_no_candidate() {
        // Stray backslash isn't a valid JSON escape
        let p = EncodingProvider;
        assert!(p.query(&Query::new(r#"jsonunescape bad \z escape"#)).await.is_empty());
    }

    #[tokio::test]
    async fn json_escape_roundtrip() {
        let p = EncodingProvider;
        let original = "line1\n\"quoted\"\ttabbed";
        let escaped = p
            .query(&Query::new(format!("jsonescape {original}")))
            .await;
        let back = p
            .query(&Query::new(format!(
                "jsonunescape {}",
                escaped[0].title
            )))
            .await;
        assert_eq!(back[0].title, original);
    }

    #[test]
    fn json_escape_pure() {
        assert_eq!(json_escape("plain"), "plain");
        assert_eq!(json_escape("a\nb"), "a\\nb");
        assert_eq!(json_escape("a\"b"), "a\\\"b");
    }

    #[test]
    fn json_unescape_pure() {
        assert_eq!(json_unescape("a\\nb").as_deref(), Some("a\nb"));
        assert_eq!(json_unescape("\"quoted\"").as_deref(), Some("quoted"));
        assert!(json_unescape("\\z").is_none());
    }


    #[tokio::test]
    async fn rot13_basic() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("rot13 Hello")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Uryyb");
    }

    #[tokio::test]
    async fn rot13_is_self_inverse() {
        let p = EncodingProvider;
        let once = p.query(&Query::new("rot13 Hello, World!")).await;
        let twice = p.query(&Query::new(format!("rot13 {}", once[0].title))).await;
        assert_eq!(twice[0].title, "Hello, World!");
    }

    #[tokio::test]
    async fn rot13_preserves_non_letters() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("rot13 Hi! 123 :)")).await;
        assert_eq!(out[0].title, "Uv! 123 :)");
    }


    #[tokio::test]
    async fn caesar_shift_3() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("caesar 3 abc")).await;
        assert_eq!(out[0].title, "def");
    }

    #[tokio::test]
    async fn caesar_negative_shift() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("caesar -1 bcd")).await;
        assert_eq!(out[0].title, "abc");
    }

    #[tokio::test]
    async fn caesar_wraps_alphabet() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("caesar 1 xyz")).await;
        assert_eq!(out[0].title, "yza");
    }

    #[tokio::test]
    async fn caesar_preserves_case() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("caesar 1 AaBbCc")).await;
        assert_eq!(out[0].title, "BbCcDd");
    }

    #[tokio::test]
    async fn caesar_zero_is_identity() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("caesar 0 hello")).await;
        assert_eq!(out[0].title, "hello");
    }

    #[tokio::test]
    async fn caesar_invalid_shift_no_output() {
        let p = EncodingProvider;
        assert!(p.query(&Query::new("caesar foo bar")).await.is_empty());
    }

    #[tokio::test]
    async fn caesar_no_text_no_output() {
        let p = EncodingProvider;
        assert!(p.query(&Query::new("caesar 3 ")).await.is_empty());
    }

    #[test]
    fn caesar_shift_pure() {
        assert_eq!(caesar_shift("Hello", 13), "Uryyb");
        assert_eq!(caesar_shift("Uryyb", 13), "Hello");
        assert_eq!(caesar_shift("AbC", 26), "AbC"); // shift mod 26
        assert_eq!(caesar_shift("AbC", 52), "AbC"); // larger multiple
        assert_eq!(caesar_shift("a", -1), "z");
        assert_eq!(caesar_shift("Z", 1), "A");
        assert_eq!(caesar_shift("", 5), "");
        assert_eq!(caesar_shift("123!@#", 5), "123!@#");
    }


    #[tokio::test]
    async fn hmac_sha256_known_vector() {
        // Common reference vector - easy to cross-check with `openssl`:
        //   echo -n "The quick brown fox jumps over the lazy dog" \
        //     | openssl dgst -sha256 -hmac "key"
        // -> f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8
        let p = EncodingProvider;
        let out = p
            .query(&Query::new(
                "hmac sha256 key The quick brown fox jumps over the lazy dog",
            ))
            .await;
        assert_eq!(
            out[0].title,
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[tokio::test]
    async fn hmac_sha1_known_vector() {
        // HMAC-SHA1("key", "The quick brown fox jumps over the lazy dog")
        // = de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9
        let p = EncodingProvider;
        let out = p
            .query(&Query::new(
                "hmac sha1 key The quick brown fox jumps over the lazy dog",
            ))
            .await;
        assert_eq!(out[0].title, "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9");
    }

    #[tokio::test]
    async fn hmac_sha512_correct_length() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("hmac sha512 secret hello")).await;
        // SHA-512 = 512 bits = 128 hex chars
        let id_value = out[0].id.strip_prefix("encode::").unwrap();
        assert_eq!(id_value.len(), 128);
    }

    #[tokio::test]
    async fn hmac_unknown_algo_no_output() {
        let p = EncodingProvider;
        assert!(p.query(&Query::new("hmac md5 key text")).await.is_empty());
    }

    #[tokio::test]
    async fn hmac_missing_text_no_output() {
        let p = EncodingProvider;
        assert!(p.query(&Query::new("hmac sha256 key")).await.is_empty());
        assert!(p.query(&Query::new("hmac sha256")).await.is_empty());
    }

    #[tokio::test]
    async fn hmac_deterministic() {
        let p = EncodingProvider;
        let a = p.query(&Query::new("hmac sha256 key hello")).await;
        let b = p.query(&Query::new("hmac sha256 key hello")).await;
        assert_eq!(a[0].title, b[0].title);
    }

    #[tokio::test]
    async fn hmac_different_keys_diverge() {
        let p = EncodingProvider;
        let a = p.query(&Query::new("hmac sha256 key1 hello")).await;
        let b = p.query(&Query::new("hmac sha256 key2 hello")).await;
        assert_ne!(a[0].title, b[0].title);
    }

    #[test]
    fn hmac_sha256_pure() {
        assert_eq!(
            hmac_sha256("key", "The quick brown fox jumps over the lazy dog").as_deref(),
            Some("f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8")
        );
    }

    #[tokio::test]
    async fn sha3_256_is_correct() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("sha3 hello")).await;
        // Known: SHA3-256("hello") = 3338be694f50c5f338814986cdf0686453a888b84f424d792af4b9202398f392
        assert_eq!(
            out[0].title,
            "3338be694f50c5f338814986cdf0686453a888b84f424d792af4b9202398f392"
        );
    }

    #[tokio::test]
    async fn sha3_512_is_correct_length() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("sha3-512 hello")).await;
        // SHA3-512 = 512 bits = 128 hex chars. Full value lives in the id
        // (the title may be truncated for display)
        let hash = out[0].id.strip_prefix("encode::").unwrap();
        assert_eq!(hash.len(), 128);
    }

    #[tokio::test]
    async fn blake3_is_correct_length() {
        let p = EncodingProvider;
        let out = p.query(&Query::new("blake3 hello")).await;
        // BLAKE3 default = 256 bits = 64 hex chars
        assert_eq!(out[0].title.len(), 64);
    }

    #[tokio::test]
    async fn blake3_is_deterministic() {
        let p = EncodingProvider;
        let a = p.query(&Query::new("blake3 hello")).await;
        let b = p.query(&Query::new("blake3 hello")).await;
        assert_eq!(a[0].title, b[0].title);
    }

    #[test]
    fn parse_op_rejects_unknowns() {
        assert!(parse_op("sha384").is_none());
        assert!(parse_op("foobar").is_none());
        assert!(parse_op("").is_none());
    }

    #[test]
    fn truncate_leaves_short_alone() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_appends_ellipsis() {
        let s = truncate("hellohellohellohello", 5);
        assert_eq!(s, "hello…");
    }
}
