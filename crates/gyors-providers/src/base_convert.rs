//! Base conversion. Detects patterns like `<number> to <base>` and
//! converts between binary / octal / decimal / hex. Pattern-triggered
//! (no keyword), so `0xff to dec` just works
//!
//! Recognised radixes on input:
//!   `0b...` binary   - `0o...` octal   - `0x...` hex   - plain decimal
//!
//! Target aliases: `bin` / `binary` - `oct` / `octal` - `dec` /
//! `decimal` - `hex` / `hexadecimal`

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct BaseConverterProvider;

#[async_trait]
impl Provider for BaseConverterProvider {
    fn id(&self) -> &str {
        "base"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some((value, target)) = parse_conversion(query.pattern()) else { return vec![]; };
        let converted = format_in_base(value, target);
        vec![Candidate {
            id: format!("base::{converted}"),
            title: converted.clone(),
            subtitle: Some(format!("{} → {}", describe_input(value), target.name())),
            icon: Icon::SfSymbol("number.square.fill".into()),
            kind: CandidateKind::Action,
            actions: vec![Action::primary("Copy")],
            search_text: String::new(),
            bypass_rank: true,
        }]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("base::")
            .ok_or_else(|| anyhow::anyhow!("invalid base candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Bin,
    Oct,
    Dec,
    Hex,
}

impl Base {
    fn name(self) -> &'static str {
        match self {
            Base::Bin => "binary",
            Base::Oct => "octal",
            Base::Dec => "decimal",
            Base::Hex => "hexadecimal",
        }
    }

    pub fn parse_target(s: &str) -> Option<Base> {
        match s {
            "bin" | "binary" | "base2" | "b" => Some(Base::Bin),
            "oct" | "octal" | "base8" | "o" => Some(Base::Oct),
            "dec" | "decimal" | "base10" | "d" | "int" => Some(Base::Dec),
            "hex" | "hexadecimal" | "base16" | "h" => Some(Base::Hex),
            _ => None,
        }
    }
}

/// Parse `<number> to <base>` (also accepts ` in ` / ` as `). Returns
/// parsed `u64` value and requested target base
pub fn parse_conversion(s: &str) -> Option<(u64, Base)> {
    let lower = s.trim().to_lowercase();
    let idx = [" to ", " in ", " as "]
        .iter()
        .filter_map(|sep| lower.find(sep).map(|i| (i, sep.len())))
        .min_by_key(|&(i, _)| i)?;
    let (num_str, rest) = lower.split_at(idx.0);
    let target_str = rest[idx.1..].trim();
    let target = Base::parse_target(target_str)?;
    let value = parse_number(num_str.trim())?;
    Some((value, target))
}

/// Parse a non-negative integer with optional `0b`/`0o`/`0x` prefix.
/// Returns `None` for values that dont fit in a u64 - good enough for
/// developer-palette use (64-bit memory, 0xffffffff-style masks, etc.)
pub fn parse_number(s: &str) -> Option<u64> {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("0b") { return u64::from_str_radix(rest, 2).ok(); }
    if let Some(rest) = t.strip_prefix("0o") { return u64::from_str_radix(rest, 8).ok(); }
    if let Some(rest) = t.strip_prefix("0x") { return u64::from_str_radix(rest, 16).ok(); }
    t.parse::<u64>().ok()
}

/// Describe input form for the subtitle ("0xff" stays "0xff",
/// decimal gets no prefix)
pub fn describe_input(value: u64) -> String {
    value.to_string()
}

pub fn format_in_base(value: u64, base: Base) -> String {
    match base {
        Base::Bin => format!("0b{value:b}"),
        Base::Oct => format!("0o{value:o}"),
        Base::Dec => value.to_string(),
        Base::Hex => format!("0x{value:x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_number_variants() {
        assert_eq!(parse_number("42"), Some(42));
        assert_eq!(parse_number("0xff"), Some(255));
        assert_eq!(parse_number("0b1010"), Some(10));
        assert_eq!(parse_number("0o755"), Some(493));
        assert_eq!(parse_number("0x0"), Some(0));
    }

    #[test]
    fn parse_number_rejects_garbage() {
        assert_eq!(parse_number("0xzz"), None);
        assert_eq!(parse_number("-5"), None); // unsigned only
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn parse_conversion_basic() {
        assert_eq!(parse_conversion("255 to hex"), Some((255, Base::Hex)));
        assert_eq!(parse_conversion("0xff to dec"), Some((255, Base::Dec)));
        assert_eq!(parse_conversion("0b1010 to hex"), Some((10, Base::Hex)));
        assert_eq!(parse_conversion("0o755 to dec"), Some((493, Base::Dec)));
    }

    #[test]
    fn parse_conversion_separators() {
        assert_eq!(parse_conversion("255 in hex"), Some((255, Base::Hex)));
        assert_eq!(parse_conversion("255 as hex"), Some((255, Base::Hex)));
    }

    #[test]
    fn parse_conversion_case_insensitive() {
        assert_eq!(parse_conversion("0XFF TO DEC"), Some((255, Base::Dec)));
    }

    #[test]
    fn parse_conversion_rejects_junk() {
        assert!(parse_conversion("hello").is_none());
        assert!(parse_conversion("5 to bananas").is_none());
        assert!(parse_conversion("nope to hex").is_none());
    }

    #[test]
    fn format_in_base_cases() {
        assert_eq!(format_in_base(255, Base::Hex), "0xff");
        assert_eq!(format_in_base(10, Base::Bin), "0b1010");
        assert_eq!(format_in_base(493, Base::Oct), "0o755");
        assert_eq!(format_in_base(42, Base::Dec), "42");
    }


    #[tokio::test]
    async fn query_converts() {
        let p = BaseConverterProvider;
        let out = p.query(&Query::new("0xff to dec")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "255");
    }

    #[tokio::test]
    async fn query_empty_without_pattern() {
        let p = BaseConverterProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn query_empty_for_unknown_base() {
        let p = BaseConverterProvider;
        assert!(p.query(&Query::new("5 to base999")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_copies_converted_value() {
        let p = BaseConverterProvider;
        let out = p.query(&Query::new("255 to hex")).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "0xff"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = BaseConverterProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }
}
