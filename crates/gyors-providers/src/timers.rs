//! Timers. Keyword: `timer <duration> [label]`, e.g.
//!   `timer 25m focus`
//!   `timer 1h 30m deep work`
//!   `timer 5m break`
//!
//! The timer itself lives Swift-side (menu-bar countdown + notification
//! on expiry). This provider just parses input and emits an
//! `Effect::StartTimer { secs, label }` that shell handles
//!
//! Also supports:
//!   `timer`       -> list running timers  (via `Effect::ListTimers`)
//!   `timer stop`  -> cancel all timers   (via `Effect::CancelTimers`)

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct TimerProvider;

#[async_trait]
impl Provider for TimerProvider {
    fn id(&self) -> &str {
        "timer"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(rest) = strip_keyword(pattern) else { return vec![]; };
        let rest = rest.trim();

        if rest.is_empty() {
            return vec![list_candidate(), stop_candidate()];
        }
        if rest.eq_ignore_ascii_case("stop")
            || rest.eq_ignore_ascii_case("clear")
            || rest.eq_ignore_ascii_case("cancel")
        {
            return vec![stop_candidate()];
        }

        let Some((secs, label)) = parse_duration_and_label(rest) else {
            return vec![error_candidate(rest)];
        };
        if secs == 0 {
            return vec![error_candidate(rest)];
        }

        vec![start_candidate(secs, &label)]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        if id == "timer::list" {
            return Ok(Effect::ListTimers);
        }
        if id == "timer::stop" {
            return Ok(Effect::CancelTimers);
        }
        if id == "timer::error" {
            return Ok(Effect::None);
        }
        // `timer::start::<secs>::<label>`
        let rest = id
            .strip_prefix("timer::start::")
            .ok_or_else(|| anyhow::anyhow!("invalid timer candidate id: {id}"))?;
        let (secs_str, label) = match rest.split_once("::") {
            Some((s, l)) => (s, l.to_string()),
            None => (rest, String::new()),
        };
        let secs: u64 = secs_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid timer seconds: {secs_str}"))?;
        Ok(Effect::StartTimer { secs, label })
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    if s == "timer" || s == "timers" {
        return Some("");
    }
    s.strip_prefix("timer ").or_else(|| s.strip_prefix("timers "))
}

/// Parse a duration + optional label. Accepts forms like
/// "25m", "1h 30m", "90s", "1h30m focus", "45 min break".
/// Returns (seconds, label). Label is everything after the last
/// consumed duration token
pub fn parse_duration_and_label(s: &str) -> Option<(u64, String)> {
    let lower = s.trim().to_lowercase();
    if lower.is_empty() { return None; }

    let bytes = lower.as_bytes();
    let mut i = 0usize;
    let mut total: u64 = 0;
    let mut saw_duration = false;
    // Byte offset in `s` where the duration region ends; everything
    // after is the label (preserved in original casing)
    let mut label_start = s.len();

    while i < bytes.len() {
        // Skip leading whitespace between tokens
        while i < bytes.len() && (bytes[i] as char).is_whitespace() { i += 1; }
        if i >= bytes.len() { break; }

        // Must start with a digit; otherwise rest is the label
        let num_start = i;
        if !bytes[i].is_ascii_digit() && bytes[i] != b'.' { break; }
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') { i += 1; }
        let num: f64 = match lower[num_start..i].parse() {
            Ok(n) => n,
            Err(_) => break,
        };

        // Optional spaces between number and unit
        let before_unit = i;
        while i < bytes.len() && bytes[i] == b' ' { i += 1; }

        // Letters = unit candidate
        let unit_start = i;
        while i < bytes.len() && (bytes[i] as char).is_ascii_alphabetic() { i += 1; }
        let unit = &lower[unit_start..i];

        let multiplier: u64 = match unit {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "d" | "day" | "days" => 86_400,
            "" => 60, // bare number -> minutes
            _ => {
                // Not a recognised unit - the word belongs to the label.
                // Default the number to minutes and stop parsing
                // durations; everything from `before_unit` (pre-space)
                // onward is the label
                total = total.saturating_add(num as u64 * 60);
                saw_duration = true;
                label_start = before_unit;
                break;
            }
        };

        total = total.saturating_add((num * multiplier as f64) as u64);
        saw_duration = true;
        label_start = i;

        // If the unit was empty ("bare number"), stop - the remainder
        // is the label, regardless of what's there
        if unit.is_empty() { break; }

        // Otherwise keep going in case user chained (e.g. "1h 30m")
    }

    if !saw_duration { return None; }
    let label = s[label_start..].trim().to_string();
    Some((total, label))
}

fn start_candidate(secs: u64, label: &str) -> Candidate {
    let pretty = format_duration(secs);
    let title = if label.is_empty() {
        format!("Start {pretty} timer")
    } else {
        format!("Start {pretty} timer - {label}")
    };
    Candidate {
        id: format!("timer::start::{secs}::{label}"),
        title,
        subtitle: Some("A live countdown will appear in the menu bar".into()),
        icon: Icon::SfSymbol("timer".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Start")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn list_candidate() -> Candidate {
    Candidate {
        id: "timer::list".into(),
        title: "Show running timers".into(),
        subtitle: Some("Open the timers panel".into()),
        icon: Icon::SfSymbol("list.bullet.clipboard".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Show")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn stop_candidate() -> Candidate {
    Candidate {
        id: "timer::stop".into(),
        title: "Cancel all timers".into(),
        subtitle: Some("Stop every running timer right now".into()),
        icon: Icon::SfSymbol("xmark.circle.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Cancel")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn error_candidate(input: &str) -> Candidate {
    Candidate {
        id: "timer::error".into(),
        title: "Couldn't parse timer duration".into(),
        subtitle: Some(format!("Try: 25m · 1h 30m · 90s · 2 hours · 45 min break. Got: {input}")),
        icon: Icon::SfSymbol("exclamationmark.triangle.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

pub fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    match (h, m, s) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, 0, 0) => format!("{h}h"),
        (h, m, 0) => format!("{h}h {m}m"),
        (h, m, s) => format!("{h}h {m}m {s}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: &str) -> u64 {
        parse_duration_and_label(s).unwrap().0
    }

    fn lbl(s: &str) -> String {
        parse_duration_and_label(s).unwrap().1
    }

    #[test]
    fn parse_minutes() {
        assert_eq!(secs("25m"), 25 * 60);
        assert_eq!(secs("25 min"), 25 * 60);
        assert_eq!(secs("25 minutes"), 25 * 60);
    }

    #[test]
    fn parse_hours_and_minutes() {
        assert_eq!(secs("1h 30m"), 3600 + 30 * 60);
        assert_eq!(secs("1h30m"), 3600 + 30 * 60);
        assert_eq!(secs("2 hours"), 2 * 3600);
    }

    #[test]
    fn parse_seconds() {
        assert_eq!(secs("90s"), 90);
    }

    #[test]
    fn parse_days() {
        assert_eq!(secs("1d"), 86_400);
    }

    #[test]
    fn parse_bare_number_defaults_to_minutes() {
        assert_eq!(secs("5"), 300);
    }

    #[test]
    fn parse_label_captured() {
        assert_eq!(lbl("25m focus"), "focus");
        assert_eq!(lbl("1h 30m deep work"), "deep work");
        assert_eq!(lbl("5 break"), "break");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_duration_and_label("hello").is_none());
        assert!(parse_duration_and_label("abc m").is_none());
        assert!(parse_duration_and_label("").is_none());
    }

    #[test]
    fn parse_unknown_word_treated_as_label_not_unit() {
        // Before we were strict and rejected; users expect `timer 5 focus`
        // to mean "5 minutes, label=focus" since `focus` isn't a unit
        let (s, l) = parse_duration_and_label("5 focus").unwrap();
        assert_eq!(s, 300);
        assert_eq!(l, "focus");
    }

    #[test]
    fn format_duration_shapes() {
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(3600), "1h");
        assert_eq!(format_duration(5400), "1h 30m");
        assert_eq!(format_duration(45), "45s");
    }


    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = TimerProvider;
        assert!(p.query(&Query::new("25m")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_controls() {
        let p = TimerProvider;
        let out = p.query(&Query::new("timer")).await;
        // "show running" + "cancel all"
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "timer::list");
        assert_eq!(out[1].id, "timer::stop");
    }

    #[tokio::test]
    async fn stop_keyword_shortcuts_to_cancel() {
        let p = TimerProvider;
        let out = p.query(&Query::new("timer stop")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "timer::stop");
    }

    #[tokio::test]
    async fn valid_duration_emits_start_candidate() {
        let p = TimerProvider;
        let out = p.query(&Query::new("timer 25m focus")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("timer::start::1500::"));
        assert!(out[0].id.ends_with("focus"));
    }

    #[tokio::test]
    async fn invalid_duration_emits_error() {
        let p = TimerProvider;
        let out = p.query(&Query::new("timer garbage input")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "timer::error");
    }

    #[tokio::test]
    async fn activate_start_emits_start_effect() {
        let p = TimerProvider;
        let eff = p
            .activate(&"timer::start::1500::focus".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::StartTimer { secs, label } => {
                assert_eq!(secs, 1500);
                assert_eq!(label, "focus");
            }
            other => panic!("expected StartTimer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_start_without_label() {
        let p = TimerProvider;
        let eff = p
            .activate(&"timer::start::60".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::StartTimer { secs, label } => {
                assert_eq!(secs, 60);
                assert!(label.is_empty());
            }
            other => panic!("expected StartTimer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_cancel_all_emits_cancel() {
        let p = TimerProvider;
        let eff = p.activate(&"timer::stop".to_string(), "default").await.unwrap();
        assert!(matches!(eff, Effect::CancelTimers));
    }

    #[tokio::test]
    async fn activate_list_emits_list_effect() {
        let p = TimerProvider;
        let eff = p.activate(&"timer::list".to_string(), "default").await.unwrap();
        assert!(matches!(eff, Effect::ListTimers));
    }

    #[tokio::test]
    async fn activate_error_is_noop() {
        let p = TimerProvider;
        let eff = p.activate(&"timer::error".to_string(), "default").await.unwrap();
        assert!(matches!(eff, Effect::None));
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = TimerProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }
}
