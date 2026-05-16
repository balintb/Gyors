//! Unit conversion. Detects patterns of the form
//! `<number> <from_unit> to <to_unit>` (case-insensitive, whitespace-
//! tolerant) and emits a single candidate with the converted value.
//! Keyword-less: pattern-triggered, so it chimes in alongside `calc`
//!
//! Covers the categories a developer actually needs from a launcher:
//! Length, mass, temperature, time, data (binary), volume, angle.
//! Everything is local Rust arithmetic - no network, no I/O

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct UnitConverterProvider;

#[async_trait]
impl Provider for UnitConverterProvider {
    fn id(&self) -> &str {
        "unit"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some((value, from, to)) = parse_conversion(pattern) else { return vec![]; };
        match convert(value, &from, &to) {
            Some(result) => vec![candidate(value, &from, &to, result)],
            None => vec![],
        }
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("unit::")
            .ok_or_else(|| anyhow::anyhow!("invalid unit candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

/// Parse `<number> <from_unit> to <to_unit>`. Returns Some iff input
/// matches that exact shape and the unit names are non-empty
pub fn parse_conversion(s: &str) -> Option<(f64, String, String)> {
    let lower = s.trim().to_lowercase();
    let mut parts = lower.splitn(2, char::is_whitespace);
    let num_str = parts.next()?;
    let value: f64 = num_str.parse().ok()?;
    let rest = parts.next()?.trim();
    // Split the remainder at " to " / " in " / " as " separators
    let idx = [" to ", " in ", " as "]
        .iter()
        .filter_map(|sep| rest.find(sep).map(|i| (i, sep.len())))
        .min_by_key(|&(i, _)| i)?;
    let (from, after) = rest.split_at(idx.0);
    let to = &after[idx.1..];
    let from = from.trim();
    let to = to.trim();
    if from.is_empty() || to.is_empty() {
        return None;
    }
    Some((value, from.into(), to.into()))
}

/// Run the conversion. Returns `None` if the units come from different
/// categories or either name isn't recognised
pub fn convert(value: f64, from: &str, to: &str) -> Option<f64> {
    // Temperature is special - it's not a simple ratio
    if let (Some(fc), Some(tc)) = (temp_code(from), temp_code(to)) {
        return Some(convert_temperature(value, fc, tc));
    }
    // All other categories use SI-reference scalar multiplication
    let (f_ref, f_cat) = unit_to_si(from)?;
    let (t_ref, t_cat) = unit_to_si(to)?;
    if f_cat != t_cat {
        return None;
    }
    Some(value * f_ref / t_ref)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cat {
    Length,
    Mass,
    Time,
    Data,
    Volume,
    Angle,
}

fn temp_code(u: &str) -> Option<char> {
    match u {
        "c" | "celsius" | "°c" => Some('C'),
        "f" | "fahrenheit" | "°f" => Some('F'),
        "k" | "kelvin" => Some('K'),
        _ => None,
    }
}

fn convert_temperature(value: f64, from: char, to: char) -> f64 {
    // Normalise via Celsius, then convert out
    let c = match from {
        'C' => value,
        'F' => (value - 32.0) * 5.0 / 9.0,
        'K' => value - 273.15,
        _ => value,
    };
    match to {
        'C' => c,
        'F' => c * 9.0 / 5.0 + 32.0,
        'K' => c + 273.15,
        _ => c,
    }
}

/// Return (scalar against SI base unit of its category, category)
fn unit_to_si(unit: &str) -> Option<(f64, Cat)> {
    // Length -> metres
    if let Some(r) = match unit {
        "mm" | "millimeter" | "millimetre" | "millimeters" | "millimetres" => Some(0.001),
        "cm" | "centimeter" | "centimetre" | "centimeters" | "centimetres" => Some(0.01),
        "m" | "meter" | "metre" | "meters" | "metres" => Some(1.0),
        "km" | "kilometer" | "kilometre" | "kilometers" | "kilometres" => Some(1000.0),
        "in" | "inch" | "inches" => Some(0.0254),
        "ft" | "foot" | "feet" => Some(0.3048),
        "yd" | "yard" | "yards" => Some(0.9144),
        "mi" | "mile" | "miles" => Some(1609.344),
        "nmi" | "nautical mile" | "nautical miles" => Some(1852.0),
        _ => None,
    } {
        return Some((r, Cat::Length));
    }
    // Mass -> grams
    if let Some(r) = match unit {
        "mg" | "milligram" | "milligrams" => Some(0.001),
        "g" | "gram" | "grams" => Some(1.0),
        "kg" | "kilogram" | "kilograms" => Some(1000.0),
        "t" | "ton" | "tonne" | "tons" | "tonnes" => Some(1_000_000.0),
        "oz" | "ounce" | "ounces" => Some(28.349523125),
        "lb" | "lbs" | "pound" | "pounds" => Some(453.59237),
        "st" | "stone" | "stones" => Some(6350.29318),
        _ => None,
    } {
        return Some((r, Cat::Mass));
    }
    // Time -> seconds
    if let Some(r) = match unit {
        "ms" | "millisecond" | "milliseconds" => Some(0.001),
        "s" | "sec" | "second" | "seconds" => Some(1.0),
        "min" | "minute" | "minutes" => Some(60.0),
        "h" | "hr" | "hour" | "hours" => Some(3600.0),
        "d" | "day" | "days" => Some(86_400.0),
        "w" | "wk" | "week" | "weeks" => Some(604_800.0),
        "mo" | "month" | "months" => Some(2_629_746.0), // avg Gregorian month
        "y" | "yr" | "year" | "years" => Some(31_556_952.0),
        _ => None,
    } {
        return Some((r, Cat::Time));
    }
    // Data (binary: 1 KB = 1024 B) -> bytes
    if let Some(r) = match unit {
        "b" | "byte" | "bytes" => Some(1.0),
        "kb" | "kib" | "kilobyte" | "kilobytes" => Some(1024.0),
        "mb" | "mib" | "megabyte" | "megabytes" => Some(1024.0 * 1024.0),
        "gb" | "gib" | "gigabyte" | "gigabytes" => Some(1024.0 * 1024.0 * 1024.0),
        "tb" | "tib" | "terabyte" | "terabytes" => Some(1024f64.powi(4)),
        "pb" | "pib" | "petabyte" | "petabytes" => Some(1024f64.powi(5)),
        _ => None,
    } {
        return Some((r, Cat::Data));
    }
    // Volume -> litres
    if let Some(r) = match unit {
        "ml" | "milliliter" | "millilitre" | "milliliters" | "millilitres" => Some(0.001),
        "l" | "liter" | "litre" | "liters" | "litres" => Some(1.0),
        "cup" | "cups" => Some(0.2365882365),
        "pt" | "pint" | "pints" => Some(0.473176473),
        "qt" | "quart" | "quarts" => Some(0.946352946),
        "gal" | "gallon" | "gallons" => Some(3.785411784),
        "floz" | "fl oz" | "fluid ounce" | "fluid ounces" => Some(0.0295735295625),
        "tsp" | "teaspoon" | "teaspoons" => Some(0.00492892159),
        "tbsp" | "tablespoon" | "tablespoons" => Some(0.01478676478),
        _ => None,
    } {
        return Some((r, Cat::Volume));
    }
    // Angle -> radians
    if let Some(r) = match unit {
        "deg" | "degree" | "degrees" | "°" => Some(std::f64::consts::PI / 180.0),
        "rad" | "radian" | "radians" => Some(1.0),
        "grad" | "gon" | "gradian" | "gradians" => Some(std::f64::consts::PI / 200.0),
        "turn" | "turns" => Some(std::f64::consts::TAU),
        _ => None,
    } {
        return Some((r, Cat::Angle));
    }
    None
}

fn candidate(value: f64, from: &str, to: &str, result: f64) -> Candidate {
    let display = format_number(result);
    Candidate {
        id: format!("unit::{display}"),
        title: format!("{display} {to}"),
        subtitle: Some(format!("{} {from} → {to}", format_number(value))),
        icon: Icon::SfSymbol("ruler".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Avoid `6.999999999e-1`-style trailing noise; cap at 6 significant
/// digits, drop trailing zeros after the decimal point
pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let formatted = format!("{v:.6}");
    // Trim trailing zeros, then a trailing dot if we land on one
    let trimmed = formatted.trim_end_matches('0');
    let trimmed = trimmed.trim_end_matches('.');
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6 || ((a - b) / a.max(b).max(1.0)).abs() < 1e-6
    }


    #[test]
    fn parse_basic() {
        let (v, f, t) = parse_conversion("5 km to mi").unwrap();
        assert_eq!(v, 5.0);
        assert_eq!(f, "km");
        assert_eq!(t, "mi");
    }

    #[test]
    fn parse_in_separator() {
        assert_eq!(parse_conversion("5 km in mi").unwrap().2, "mi");
    }

    #[test]
    fn parse_as_separator() {
        assert_eq!(parse_conversion("5 km as mi").unwrap().2, "mi");
    }

    #[test]
    fn parse_case_insensitive_and_whitespace() {
        let (v, f, t) = parse_conversion("  5  KM  to  MI  ").unwrap();
        assert_eq!(v, 5.0);
        assert_eq!(f, "km");
        assert_eq!(t, "mi");
    }

    #[test]
    fn parse_multi_word_unit() {
        let (_, f, _) = parse_conversion("5 nautical mile to km").unwrap();
        assert_eq!(f, "nautical mile");
    }

    #[test]
    fn parse_rejects_non_numeric() {
        assert!(parse_conversion("foo km to mi").is_none());
    }

    #[test]
    fn parse_rejects_missing_to() {
        assert!(parse_conversion("5 km").is_none());
        assert!(parse_conversion("5 km mi").is_none());
    }

    #[test]
    fn parse_negative_values() {
        assert_eq!(parse_conversion("-5 c to f").unwrap().0, -5.0);
    }

    #[test]
    fn parse_decimal_values() {
        assert_eq!(parse_conversion("1.5 km to m").unwrap().0, 1.5);
    }


    #[test]
    fn km_to_mi() {
        assert!(close(convert(10.0, "km", "mi").unwrap(), 6.2137119));
    }

    #[test]
    fn inches_to_cm() {
        assert!(close(convert(12.0, "in", "cm").unwrap(), 30.48));
    }

    #[test]
    fn feet_to_meters() {
        assert!(close(convert(100.0, "ft", "m").unwrap(), 30.48));
    }


    #[test]
    fn kg_to_lb() {
        assert!(close(convert(1.0, "kg", "lb").unwrap(), 2.2046226));
    }

    #[test]
    fn oz_to_g() {
        assert!(close(convert(16.0, "oz", "g").unwrap(), 453.5923));
    }


    #[test]
    fn celsius_to_fahrenheit() {
        assert!(close(convert(100.0, "c", "f").unwrap(), 212.0));
        assert!(close(convert(0.0, "celsius", "fahrenheit").unwrap(), 32.0));
    }

    #[test]
    fn fahrenheit_to_celsius() {
        assert!(close(convert(32.0, "f", "c").unwrap(), 0.0));
        assert!(close(convert(212.0, "f", "c").unwrap(), 100.0));
    }

    #[test]
    fn kelvin_roundtrip() {
        assert!(close(convert(0.0, "c", "k").unwrap(), 273.15));
        assert!(close(convert(273.15, "k", "c").unwrap(), 0.0));
    }


    #[test]
    fn hours_to_seconds() {
        assert!(close(convert(2.0, "h", "s").unwrap(), 7200.0));
    }

    #[test]
    fn gib_to_mib_binary() {
        assert!(close(convert(1.0, "gb", "mb").unwrap(), 1024.0));
    }

    #[test]
    fn gallons_to_liters() {
        assert!(close(convert(1.0, "gal", "l").unwrap(), 3.785411784));
    }

    #[test]
    fn degrees_to_radians() {
        assert!(close(convert(180.0, "deg", "rad").unwrap(), std::f64::consts::PI));
    }


    #[test]
    fn cross_category_returns_none() {
        assert!(convert(5.0, "km", "kg").is_none());
    }

    #[test]
    fn unknown_unit_returns_none() {
        assert!(convert(5.0, "km", "bananas").is_none());
    }


    #[tokio::test]
    async fn query_emits_candidate_for_valid_conversion() {
        let p = UnitConverterProvider;
        let out = p.query(&Query::new("10 km to mi")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("mi"));
    }

    #[tokio::test]
    async fn query_empty_for_unrelated_input() {
        let p = UnitConverterProvider;
        assert!(p.query(&Query::new("hello world")).await.is_empty());
    }

    #[tokio::test]
    async fn query_empty_for_unknown_units() {
        let p = UnitConverterProvider;
        assert!(p.query(&Query::new("5 foobars to bazquux")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_copies_formatted_value() {
        let p = UnitConverterProvider;
        let out = p.query(&Query::new("100 c to f")).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "212"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = UnitConverterProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn format_number_trims_noise() {
        assert_eq!(format_number(1.0), "1");
        assert_eq!(format_number(1.5), "1.5");
        assert_eq!(format_number(1.123456789), "1.123457");
        assert_eq!(format_number(0.0), "0");
    }
}
