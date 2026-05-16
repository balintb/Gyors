//! Number formatter - `numformat 1234567.89` -> "1,234,567.89"
//!
//! Optional precision: `numformat 1234.5678 2` -> "1,234.57".
//! Output styles: comma-thousands (US/UK), dot-thousands (European).
//! Activation copies the comma-thousands form by default; the European
//! variant is the second row

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct NumFormatProvider;

#[async_trait]
impl Provider for NumFormatProvider {
    fn id(&self) -> &str {
        "numformat"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let rest = match pattern.strip_prefix("numformat ").or_else(|| pattern.strip_prefix("nf "))
        {
            Some(r) => r.trim(),
            None => return vec![],
        };
        if rest.is_empty() {
            return vec![];
        }

        // Two-arg form: "<number> <precision>"
        let (num_str, precision) = match rest.rsplit_once(char::is_whitespace) {
            Some((head, tail)) if tail.parse::<u32>().is_ok() => {
                (head.trim(), Some(tail.parse::<u32>().unwrap()))
            }
            _ => (rest, None),
        };

        let num: f64 = match num_str.parse() {
            Ok(n) => n,
            Err(_) => return vec![],
        };

        let comma = format_with(num, precision, ',', '.');
        let dot = format_with(num, precision, '.', ',');
        let plain = format_plain(num, precision);
        vec![
            make_candidate(&comma, "1,234,567.89 (US/UK)"),
            make_candidate(&dot, "1.234.567,89 (European)"),
            make_candidate(&plain, "no separators"),
        ]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("numformat::")
            .ok_or_else(|| anyhow::anyhow!("invalid numformat candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

fn make_candidate(value: &str, label: &str) -> Candidate {
    Candidate {
        id: format!("numformat::{value}"),
        title: value.to_string(),
        subtitle: Some(label.to_string()),
        icon: Icon::SfSymbol("number.circle".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Format `num` with a thousands separator and a decimal point. Splitting
/// integer/fractional up front avoids the `thousands == '.'` collision -
/// otherwise grouping a value like 1234567.89 with thousands='.' produces
/// "1.234.567.89" and any later "find the decimal dot" matches the wrong
/// dot
pub fn format_with(num: f64, precision: Option<u32>, thousands: char, decimal: char) -> String {
    let plain = format_plain(num.abs(), precision);
    let sign = if num.is_sign_negative() { "-" } else { "" };
    let (int_str, frac_opt) = match plain.find('.') {
        Some(idx) => (&plain[..idx], Some(&plain[idx + 1..])),
        None => (plain.as_str(), None),
    };
    let mut grouped = String::with_capacity(int_str.len() + int_str.len() / 3);
    for (i, ch) in int_str.chars().rev().enumerate() {
        if i != 0 && i % 3 == 0 {
            grouped.push(thousands);
        }
        grouped.push(ch);
    }
    let int_grouped: String = grouped.chars().rev().collect();
    match frac_opt {
        Some(frac) => format!("{sign}{int_grouped}{decimal}{frac}"),
        None => format!("{sign}{int_grouped}"),
    }
}

/// Plain `{num}` rendering with optional fixed precision; no thousands
/// separator. Strips a trailing `.0` only when no precision was requested
/// (so `numformat 5 2` still gives "5.00")
fn format_plain(num: f64, precision: Option<u32>) -> String {
    if let Some(p) = precision {
        return format!("{:.*}", p as usize, num);
    }
    let raw = format!("{num}");
    // `format!("{}", 5_f64)` already gives "5", but `format!("{}", 5.0)` may
    // give "5" too because Rust prints `f64` without trailing zero. Belt &
    // braces - handle "5.0" defensively
    if let Some(stripped) = raw.strip_suffix(".0") {
        stripped.to_string()
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_keyword_no_match() {
        let p = NumFormatProvider;
        assert!(p.query(&Query::new("1234")).await.is_empty());
    }

    #[tokio::test]
    async fn keyword_with_no_input_empty() {
        let p = NumFormatProvider;
        assert!(p.query(&Query::new("numformat ")).await.is_empty());
    }

    #[tokio::test]
    async fn invalid_number_no_match() {
        let p = NumFormatProvider;
        assert!(p.query(&Query::new("numformat foo")).await.is_empty());
    }

    #[tokio::test]
    async fn integer_thousands_separator() {
        let p = NumFormatProvider;
        let out = p.query(&Query::new("numformat 1234567")).await;
        assert!(out.iter().any(|c| c.title == "1,234,567"));
        assert!(out.iter().any(|c| c.title == "1.234.567"));
        assert!(out.iter().any(|c| c.title == "1234567"));
    }

    #[tokio::test]
    async fn decimal_thousands_separator() {
        let p = NumFormatProvider;
        let out = p.query(&Query::new("numformat 1234567.89")).await;
        assert!(out.iter().any(|c| c.title == "1,234,567.89"));
        assert!(out.iter().any(|c| c.title == "1.234.567,89"));
    }

    #[tokio::test]
    async fn negative_keeps_sign_outside_separators() {
        let p = NumFormatProvider;
        let out = p.query(&Query::new("numformat -1234567.89")).await;
        assert!(out.iter().any(|c| c.title == "-1,234,567.89"));
        assert!(out.iter().any(|c| c.title == "-1.234.567,89"));
    }

    #[tokio::test]
    async fn precision_argument_rounds() {
        let p = NumFormatProvider;
        let out = p.query(&Query::new("numformat 1234.5678 2")).await;
        assert!(out.iter().any(|c| c.title == "1,234.57"));
    }

    #[tokio::test]
    async fn precision_argument_pads_zero() {
        let p = NumFormatProvider;
        let out = p.query(&Query::new("numformat 5 2")).await;
        assert!(out.iter().any(|c| c.title == "5.00"));
    }

    #[tokio::test]
    async fn small_numbers_unchanged() {
        let p = NumFormatProvider;
        let out = p.query(&Query::new("numformat 42")).await;
        assert!(out.iter().any(|c| c.title == "42"));
    }

    #[tokio::test]
    async fn nf_alias() {
        let p = NumFormatProvider;
        let out = p.query(&Query::new("nf 1000")).await;
        assert!(out.iter().any(|c| c.title == "1,000"));
    }

    #[tokio::test]
    async fn activate_copies_chosen_form() {
        let p = NumFormatProvider;
        let out = p.query(&Query::new("numformat 1234567")).await;
        let eu = out.iter().find(|c| c.title == "1.234.567").unwrap();
        let eff = p.activate(&eu.id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "1.234.567"),
            other => panic!("got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = NumFormatProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }


    #[test]
    fn format_with_basic() {
        assert_eq!(format_with(1234.0, None, ',', '.'), "1,234");
        assert_eq!(format_with(1234567.89, None, ',', '.'), "1,234,567.89");
        assert_eq!(format_with(1234567.89, None, '.', ','), "1.234.567,89");
        assert_eq!(format_with(0.0, None, ',', '.'), "0");
        assert_eq!(format_with(123.0, None, ',', '.'), "123");
    }

    #[test]
    fn format_with_negative() {
        assert_eq!(format_with(-1234.0, None, ',', '.'), "-1,234");
        assert_eq!(format_with(-1.0, None, ',', '.'), "-1");
    }

    #[test]
    fn format_with_precision() {
        assert_eq!(format_with(1234.0, Some(2), ',', '.'), "1,234.00");
        assert_eq!(format_with(1234.5678, Some(3), ',', '.'), "1,234.568");
        assert_eq!(format_with(0.5, Some(0), ',', '.'), "0");
        assert_eq!(format_with(0.6, Some(0), ',', '.'), "1");
    }

    #[test]
    fn format_with_three_digits_no_separator() {
        // No grouping for numbers < 1000
        assert_eq!(format_with(999.0, None, ',', '.'), "999");
        assert_eq!(format_with(100.0, None, ',', '.'), "100");
    }
}
