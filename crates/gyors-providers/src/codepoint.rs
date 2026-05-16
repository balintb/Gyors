//! Unicode codepoint inspector. `cp A` -> "U+0041 - 65 dec - UTF-8: 41"
//!
//! Accepts:
//!   - A single character: `cp A` or any single Unicode char
//!   - A `U+XXXX` literal:  `cp U+1F680`
//!   - A hex prefix:        `cp 0x41`
//!   - A bare decimal:      `cp 65`
//!
//! Output rows: hex codepoint, decimal codepoint, UTF-8 byte sequence,
//! UTF-16 unit sequence, and the literal character itself

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct CodepointProvider;

#[async_trait]
impl Provider for CodepointProvider {
    fn id(&self) -> &str {
        "cp"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let rest = match pattern
            .strip_prefix("cp ")
            .or_else(|| pattern.strip_prefix("codepoint "))
        {
            Some(r) => r.trim(),
            None => return vec![],
        };
        if rest.is_empty() {
            return vec![];
        }
        let Some(ch) = parse_input(rest) else {
            return vec![error(&format!(
                "couldn't parse '{rest}' as a character or codepoint"
            ))];
        };
        let cp = ch as u32;
        let utf8 = utf8_bytes(ch);
        let utf16 = utf16_units(ch);
        vec![
            row(&format!("U+{cp:04X}"), &format!("{ch} · hex codepoint")),
            row(&format!("{cp}"), &format!("{ch} · decimal codepoint")),
            row(&utf8, &format!("{ch} · UTF-8 bytes")),
            row(&utf16, &format!("{ch} · UTF-16 units")),
            row(&ch.to_string(), &format!("U+{cp:04X} · literal character")),
        ]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        if id == "cp::error" {
            return Ok(Effect::None);
        }
        let value = id
            .strip_prefix("cp::")
            .ok_or_else(|| anyhow::anyhow!("invalid cp candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

/// Resolve user input to a single `char`. Three input shapes are tried in
/// order: a literal single-char input, a `U+...` / `0x...` hex literal,
/// and a bare decimal
pub fn parse_input(s: &str) -> Option<char> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Shape 1 - literal character. Most common case (e.g. `cp A`)
    if s.chars().count() == 1 {
        return s.chars().next();
    }
    // Shape 2 - explicit codepoint literal
    let upper = s.to_uppercase();
    let hex_str = upper
        .strip_prefix("U+")
        .or_else(|| upper.strip_prefix("0X"));
    if let Some(hex) = hex_str {
        let code = u32::from_str_radix(hex, 16).ok()?;
        return char::from_u32(code);
    }
    // Shape 3 - bare decimal. Only fires when the whole input parses
    // as a u32; this avoids stealing inputs like "abc" that look mostly
    // alphabetic
    if let Ok(code) = s.parse::<u32>() {
        return char::from_u32(code);
    }
    None
}

fn utf8_bytes(ch: char) -> String {
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    s.bytes()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn utf16_units(ch: char) -> String {
    let mut buf = [0u16; 2];
    let s = ch.encode_utf16(&mut buf);
    s.iter()
        .map(|u| format!("{u:04X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn row(value: &str, sub: &str) -> Candidate {
    Candidate {
        id: format!("cp::{value}"),
        title: value.to_string(),
        subtitle: Some(sub.to_string()),
        icon: Icon::SfSymbol("character.cursor.ibeam".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn error(msg: &str) -> Candidate {
    Candidate {
        id: "cp::error".into(),
        title: "Codepoint error".into(),
        subtitle: Some(msg.to_string()),
        icon: Icon::SfSymbol("exclamationmark.triangle.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = CodepointProvider;
        assert!(p.query(&Query::new("A")).await.is_empty());
    }

    #[tokio::test]
    async fn ascii_character() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp A")).await;
        assert!(out.iter().any(|c| c.title == "U+0041"));
        assert!(out.iter().any(|c| c.title == "65"));
        assert!(out.iter().any(|c| c.title == "41"));
        assert!(out.iter().any(|c| c.title == "0041"));
        assert!(out.iter().any(|c| c.title == "A"));
    }

    #[tokio::test]
    async fn rocket_emoji() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp 🚀")).await;
        assert!(out.iter().any(|c| c.title == "U+1F680"));
        assert!(out.iter().any(|c| c.title == "128640"));
        // Surrogate pair for U+1F680 is D83D DE80
        assert!(out.iter().any(|c| c.title == "D83D DE80"));
        assert!(out.iter().any(|c| c.title == "F0 9F 9A 80"));
    }

    #[tokio::test]
    async fn u_plus_literal() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp U+1F680")).await;
        assert!(out.iter().any(|c| c.title == "🚀"));
    }

    #[tokio::test]
    async fn lowercase_u_plus() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp u+0041")).await;
        assert!(out.iter().any(|c| c.title == "A"));
    }

    #[tokio::test]
    async fn hex_0x_literal() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp 0x41")).await;
        assert!(out.iter().any(|c| c.title == "A"));
    }

    #[tokio::test]
    async fn bare_decimal() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp 65")).await;
        assert!(out.iter().any(|c| c.title == "A"));
    }

    #[tokio::test]
    async fn invalid_input_yields_error() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp abc")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "cp::error");
    }

    #[tokio::test]
    async fn out_of_range_codepoint_is_error() {
        // U+11FFFF is past the Unicode max U+10FFFF
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp U+11FFFF")).await;
        assert_eq!(out[0].id, "cp::error");
    }

    #[tokio::test]
    async fn surrogate_codepoint_is_error() {
        // Surrogates aren't valid `char`s
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp U+D800")).await;
        assert_eq!(out[0].id, "cp::error");
    }

    #[tokio::test]
    async fn codepoint_alias() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("codepoint A")).await;
        assert!(out.iter().any(|c| c.title == "U+0041"));
    }

    #[tokio::test]
    async fn activate_copies_value() {
        let p = CodepointProvider;
        let out = p.query(&Query::new("cp A")).await;
        let hex_row = out.iter().find(|c| c.title == "U+0041").unwrap();
        let eff = p.activate(&hex_row.id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "U+0041"),
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_error_is_noop() {
        let p = CodepointProvider;
        let eff = p
            .activate(&"cp::error".to_string(), "default")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::None));
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = CodepointProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }


    #[test]
    fn parse_input_table() {
        assert_eq!(parse_input("A"), Some('A'));
        assert_eq!(parse_input("🚀"), Some('🚀'));
        assert_eq!(parse_input("U+0041"), Some('A'));
        assert_eq!(parse_input("u+0041"), Some('A'));
        assert_eq!(parse_input("0x41"), Some('A'));
        assert_eq!(parse_input("65"), Some('A'));
        assert_eq!(parse_input(""), None);
        assert_eq!(parse_input("abc"), None);
        assert_eq!(parse_input("U+11FFFF"), None);
    }

    #[test]
    fn utf8_bytes_table() {
        assert_eq!(utf8_bytes('A'), "41");
        assert_eq!(utf8_bytes('é'), "C3 A9");
        assert_eq!(utf8_bytes('字'), "E5 AD 97");
        assert_eq!(utf8_bytes('🚀'), "F0 9F 9A 80");
    }

    #[test]
    fn utf16_units_table() {
        assert_eq!(utf16_units('A'), "0041");
        // U+1F680 surrogate pair
        assert_eq!(utf16_units('🚀'), "D83D DE80");
    }
}
