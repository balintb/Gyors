//! Roman numeral converter. `roman 2024` -> MMXXIV; `roman MMXXIV` -> 2024
//!
//! Supports the classical 1..3999 range - beyond that the standard
//! notation runs out of single letters (overlines for x1000 are not in
//! Unicode equivalents most fonts render). Lowercase roman input is
//! accepted; output is always uppercase

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct RomanProvider;

const TABLE: &[(u32, &str)] = &[
    (1000, "M"),
    (900, "CM"),
    (500, "D"),
    (400, "CD"),
    (100, "C"),
    (90, "XC"),
    (50, "L"),
    (40, "XL"),
    (10, "X"),
    (9, "IX"),
    (5, "V"),
    (4, "IV"),
    (1, "I"),
];

#[async_trait]
impl Provider for RomanProvider {
    fn id(&self) -> &str {
        "roman"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(rest) = pattern.strip_prefix("roman ") else {
            return vec![];
        };
        let rest = rest.trim();
        if rest.is_empty() {
            return vec![];
        }
        // Numeric -> encode; otherwise try to decode the roman literal
        if let Ok(n) = rest.parse::<u32>() {
            if let Some(roman) = to_roman(n) {
                return vec![candidate(&roman, &format!("{n} → {roman}"))];
            }
            return vec![error("0 and ≥4000 are outside the classical range (1..3999)")];
        }
        match from_roman(rest) {
            Some(n) => vec![candidate(&n.to_string(), &format!("{} → {n}", rest.to_uppercase()))],
            None => vec![error(&format!("'{rest}' isn't a valid Roman numeral"))],
        }
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        if id == "roman::error" {
            return Ok(Effect::None);
        }
        let value = id
            .strip_prefix("roman::")
            .ok_or_else(|| anyhow::anyhow!("invalid roman candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

/// Encode a positive integer in classical Roman numerals. Returns None
/// for 0 or values that need overline notation (>=4000)
pub fn to_roman(mut n: u32) -> Option<String> {
    if n == 0 || n >= 4000 {
        return None;
    }
    let mut out = String::new();
    for &(value, sym) in TABLE {
        while n >= value {
            out.push_str(sym);
            n -= value;
        }
    }
    Some(out)
}

/// Decode a Roman numeral string. Accepts upper/lower-case and the
/// canonical subtractive form (CM, CD, XC, XL, IX, IV). Rejects
/// non-canonical forms like "IIII" or "VV" - strict by design so the
/// user gets a clear error rather than silent acceptance
pub fn from_roman(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let upper = s.to_uppercase();
    if !upper.chars().all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M')) {
        return None;
    }
    let mut total: u32 = 0;
    let bytes = upper.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let two = if i + 1 < bytes.len() {
            std::str::from_utf8(&bytes[i..i + 2]).ok()
        } else {
            None
        };
        let one = std::str::from_utf8(&bytes[i..i + 1]).ok()?;
        if let Some(t) = two {
            if let Some(&(v, _)) = TABLE.iter().find(|(_, sym)| *sym == t) {
                total += v;
                i += 2;
                continue;
            }
        }
        let &(v, _) = TABLE.iter().find(|(_, sym)| *sym == one)?;
        total += v;
        i += 1;
    }
    // Round-trip: only accept canonical forms
    if to_roman(total).as_deref() == Some(upper.as_str()) {
        Some(total)
    } else {
        None
    }
}

fn candidate(value: &str, sub: &str) -> Candidate {
    Candidate {
        id: format!("roman::{value}"),
        title: value.to_string(),
        subtitle: Some(sub.to_string()),
        icon: Icon::SfSymbol("character.book.closed".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn error(msg: &str) -> Candidate {
    Candidate {
        id: "roman::error".into(),
        title: "Roman numeral error".into(),
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
        let p = RomanProvider;
        assert!(p.query(&Query::new("2024")).await.is_empty());
        assert!(p.query(&Query::new("MMXXIV")).await.is_empty());
    }

    #[tokio::test]
    async fn keyword_with_no_input_empty() {
        let p = RomanProvider;
        assert!(p.query(&Query::new("roman ")).await.is_empty());
    }

    #[tokio::test]
    async fn encode_classic_year() {
        let p = RomanProvider;
        let out = p.query(&Query::new("roman 2024")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "MMXXIV");
    }

    #[tokio::test]
    async fn decode_classic_year() {
        let p = RomanProvider;
        let out = p.query(&Query::new("roman MMXXIV")).await;
        assert_eq!(out[0].title, "2024");
    }

    #[tokio::test]
    async fn decode_lowercase_accepted() {
        let p = RomanProvider;
        let out = p.query(&Query::new("roman mmxxiv")).await;
        assert_eq!(out[0].title, "2024");
    }

    #[tokio::test]
    async fn encode_zero_is_error() {
        let p = RomanProvider;
        let out = p.query(&Query::new("roman 0")).await;
        assert_eq!(out[0].id, "roman::error");
    }

    #[tokio::test]
    async fn encode_4000_is_error() {
        let p = RomanProvider;
        let out = p.query(&Query::new("roman 4000")).await;
        assert_eq!(out[0].id, "roman::error");
    }

    #[tokio::test]
    async fn decode_garbage_is_error() {
        let p = RomanProvider;
        let out = p.query(&Query::new("roman ZZZ")).await;
        assert_eq!(out[0].id, "roman::error");
    }

    #[tokio::test]
    async fn decode_noncanonical_is_error() {
        // "IIII" should be rejected - canonical is "IV"
        let p = RomanProvider;
        let out = p.query(&Query::new("roman IIII")).await;
        assert_eq!(out[0].id, "roman::error");
    }

    #[tokio::test]
    async fn activate_copies_value() {
        let p = RomanProvider;
        let out = p.query(&Query::new("roman 2024")).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "MMXXIV"),
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_error_is_noop() {
        let p = RomanProvider;
        let eff = p
            .activate(&"roman::error".to_string(), "default")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::None));
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = RomanProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }


    #[test]
    fn to_roman_known_values() {
        assert_eq!(to_roman(1).as_deref(), Some("I"));
        assert_eq!(to_roman(2).as_deref(), Some("II"));
        assert_eq!(to_roman(3).as_deref(), Some("III"));
        assert_eq!(to_roman(4).as_deref(), Some("IV"));
        assert_eq!(to_roman(5).as_deref(), Some("V"));
        assert_eq!(to_roman(9).as_deref(), Some("IX"));
        assert_eq!(to_roman(10).as_deref(), Some("X"));
        assert_eq!(to_roman(40).as_deref(), Some("XL"));
        assert_eq!(to_roman(50).as_deref(), Some("L"));
        assert_eq!(to_roman(90).as_deref(), Some("XC"));
        assert_eq!(to_roman(100).as_deref(), Some("C"));
        assert_eq!(to_roman(400).as_deref(), Some("CD"));
        assert_eq!(to_roman(500).as_deref(), Some("D"));
        assert_eq!(to_roman(900).as_deref(), Some("CM"));
        assert_eq!(to_roman(1000).as_deref(), Some("M"));
        assert_eq!(to_roman(1994).as_deref(), Some("MCMXCIV"));
        assert_eq!(to_roman(3999).as_deref(), Some("MMMCMXCIX"));
    }

    #[test]
    fn to_roman_out_of_range() {
        assert_eq!(to_roman(0), None);
        assert_eq!(to_roman(4000), None);
        assert_eq!(to_roman(10_000), None);
    }

    #[test]
    fn from_roman_known_values() {
        assert_eq!(from_roman("I"), Some(1));
        assert_eq!(from_roman("IV"), Some(4));
        assert_eq!(from_roman("IX"), Some(9));
        assert_eq!(from_roman("XL"), Some(40));
        assert_eq!(from_roman("XC"), Some(90));
        assert_eq!(from_roman("CD"), Some(400));
        assert_eq!(from_roman("CM"), Some(900));
        assert_eq!(from_roman("MCMXCIV"), Some(1994));
        assert_eq!(from_roman("MMMCMXCIX"), Some(3999));
    }

    #[test]
    fn from_roman_rejects_invalid() {
        assert_eq!(from_roman(""), None);
        assert_eq!(from_roman("Z"), None);
        assert_eq!(from_roman("123"), None);
        assert_eq!(from_roman("IIII"), None);     // non-canonical
        assert_eq!(from_roman("VV"), None);       // non-canonical
        assert_eq!(from_roman("LL"), None);       // non-canonical
        assert_eq!(from_roman("IC"), None);       // not a legal subtractive
        assert_eq!(from_roman("XM"), None);       // not a legal subtractive
    }

    #[test]
    fn roundtrip_full_range() {
        for n in 1..=3999 {
            let r = to_roman(n).unwrap_or_else(|| panic!("encode failed for {n}"));
            assert_eq!(
                from_roman(&r),
                Some(n),
                "roundtrip failed: {n} → {r} → {:?}",
                from_roman(&r)
            );
        }
    }
}
