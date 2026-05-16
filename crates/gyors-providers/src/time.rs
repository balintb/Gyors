//! Time / date utility provider
//!
//!   now                       -> current unix timestamp + ISO + local
//!   ts <unix>                 -> human-readable date from a unix timestamp
//!   date +3d / -1w / ...        -> date arithmetic relative to now
//!   datediff <a> :: <b>       -> days between two YYYY-MM-DD dates
//!   datediff <a>              -> days between <a> and today
//!
//! Accepted units for `date`: `s`, `m`, `h`, `d`, `w`. Leading `+`/`-` is
//! optional (default is `+`)

use async_trait::async_trait;
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct TimeProvider;

#[async_trait]
impl Provider for TimeProvider {
    fn id(&self) -> &str {
        "time"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        if pattern == "now" {
            return render(Utc::now(), "now");
        }
        if let Some(rest) = pattern.strip_prefix("ts ") {
            if let Ok(ts) = rest.trim().parse::<i64>() {
                if let Some(dt) = Utc.timestamp_opt(ts, 0).single() {
                    return render(dt, "from unix timestamp");
                }
            }
        }
        if let Some(rest) = pattern.strip_prefix("date ") {
            if let Some(d) = parse_duration(rest.trim()) {
                return render(Utc::now() + d, "date offset");
            }
        }
        if let Some(rest) = pattern.strip_prefix("datediff ") {
            return datediff_candidates(rest.trim());
        }
        vec![]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let value = id
            .strip_prefix("time::")
            .ok_or_else(|| anyhow::anyhow!("invalid time candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

fn render(dt: DateTime<Utc>, context: &str) -> Vec<Candidate> {
    let local = dt.with_timezone(&Local);
    vec![
        make_candidate(&dt.timestamp().to_string(), &format!("unix timestamp · {context}")),
        make_candidate(
            &local.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
            &format!("local date-time · {context}"),
        ),
        make_candidate(
            &dt.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            &format!("ISO 8601 UTC · {context}"),
        ),
        make_candidate(
            &local.format("%A, %B %e, %Y").to_string(),
            &format!("long date · {context}"),
        ),
    ]
}

fn make_candidate(value: &str, context: &str) -> Candidate {
    Candidate {
        id: format!("time::{value}"),
        title: value.to_string(),
        subtitle: Some(context.to_string()),
        icon: Icon::SfSymbol("clock.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Parse a YYYY-MM-DD date, or the literal "today" / "now"
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if matches!(s, "today" | "now") {
        return Some(Local::now().date_naive());
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// Format the diff between `a` and `b` as a single Candidate. Result
/// reports days, weeks, and approximate months/years for context. Sign
/// follows `b - a` (a future `b` produces a positive number)
pub fn datediff_candidates(rest: &str) -> Vec<Candidate> {
    if rest.is_empty() {
        return vec![];
    }
    let (a_str, b_str) = match rest.split_once("::") {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (rest, "today"),
    };
    let Some(a) = parse_date(a_str) else {
        return vec![];
    };
    let Some(b) = parse_date(b_str) else {
        return vec![];
    };
    let days = (b - a).num_days();
    let weeks = days / 7;
    let approx_months = (days as f64) / 30.4375;
    let approx_years = (days as f64) / 365.25;
    let summary = format!("{days} days · {weeks} weeks · ~{approx_months:.1} months · ~{approx_years:.2} years");
    let context = format!("{a} → {b}");
    vec![
        make_candidate(&days.to_string(), &format!("days · {context}")),
        make_candidate(&summary, &context),
    ]
}

fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (sign, rest) = if let Some(r) = s.strip_prefix('+') {
        (1i64, r)
    } else if let Some(r) = s.strip_prefix('-') {
        (-1, r)
    } else {
        (1, s)
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    let unit_char = rest.chars().last()?;
    let num_str = &rest[..rest.len() - unit_char.len_utf8()];
    let num: i64 = num_str.trim().parse().ok()?;
    let n = sign.checked_mul(num)?;
    match unit_char {
        's' => Some(Duration::seconds(n)),
        'm' => Some(Duration::minutes(n)),
        'h' => Some(Duration::hours(n)),
        'd' => Some(Duration::days(n)),
        'w' => Some(Duration::weeks(n)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = TimeProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
        assert!(p.query(&Query::new("")).await.is_empty());
    }

    #[tokio::test]
    async fn now_yields_four_formats() {
        let p = TimeProvider;
        let out = p.query(&Query::new("now")).await;
        assert_eq!(out.len(), 4);
        assert!(out.iter().any(|c| c.subtitle.as_deref().unwrap_or("").contains("unix timestamp")));
        assert!(out.iter().any(|c| c.subtitle.as_deref().unwrap_or("").contains("ISO 8601")));
    }

    #[tokio::test]
    async fn ts_converts_unix_timestamp() {
        let p = TimeProvider;
        // 2024-01-01 00:00:00 UTC = 1704067200
        let out = p.query(&Query::new("ts 1704067200")).await;
        assert_eq!(out.len(), 4);
        let iso = out
            .iter()
            .find(|c| c.subtitle.as_deref().unwrap_or("").contains("ISO 8601"))
            .unwrap();
        assert!(iso.title.starts_with("2024-01-01"), "got {}", iso.title);
    }

    #[tokio::test]
    async fn ts_invalid_yields_nothing() {
        let p = TimeProvider;
        assert!(p.query(&Query::new("ts hello")).await.is_empty());
    }

    #[tokio::test]
    async fn date_offset_future() {
        let p = TimeProvider;
        let out = p.query(&Query::new("date +3d")).await;
        assert_eq!(out.len(), 4);
    }

    #[tokio::test]
    async fn date_offset_past() {
        let p = TimeProvider;
        let out = p.query(&Query::new("date -1w")).await;
        assert_eq!(out.len(), 4);
    }

    #[tokio::test]
    async fn date_offset_invalid_yields_nothing() {
        let p = TimeProvider;
        assert!(p.query(&Query::new("date foo")).await.is_empty());
        assert!(p.query(&Query::new("date 3x")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_copies_value() {
        let p = TimeProvider;
        let out = p.query(&Query::new("now")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert!(!s.is_empty()),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[test]
    fn parse_duration_cases() {
        assert_eq!(parse_duration("+3d"), Some(Duration::days(3)));
        assert_eq!(parse_duration("-1w"), Some(Duration::weeks(-1)));
        assert_eq!(parse_duration("3d"), Some(Duration::days(3)));
        assert_eq!(parse_duration("30m"), Some(Duration::minutes(30)));
        assert_eq!(parse_duration("2h"), Some(Duration::hours(2)));
        assert_eq!(parse_duration("10s"), Some(Duration::seconds(10)));
    }

    #[test]
    fn parse_duration_rejects_bad_input() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("3x"), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("+"), None);
        assert_eq!(parse_duration("d"), None);
    }


    #[tokio::test]
    async fn datediff_two_dates_positive() {
        let p = TimeProvider;
        let out = p
            .query(&Query::new("datediff 2024-01-01 :: 2024-12-31"))
            .await;
        assert_eq!(out.len(), 2);
        // 2024 is a leap year - Jan 1 -> Dec 31 is 365 days
        assert_eq!(out[0].title, "365");
        assert!(out[1].title.contains("365 days"));
        assert!(out[1].title.contains("52 weeks"));
    }

    #[tokio::test]
    async fn datediff_two_dates_negative() {
        let p = TimeProvider;
        let out = p
            .query(&Query::new("datediff 2024-12-31 :: 2024-01-01"))
            .await;
        assert_eq!(out[0].title, "-365");
    }

    #[tokio::test]
    async fn datediff_same_day_is_zero() {
        let p = TimeProvider;
        let out = p
            .query(&Query::new("datediff 2024-06-15 :: 2024-06-15"))
            .await;
        assert_eq!(out[0].title, "0");
    }

    #[tokio::test]
    async fn datediff_single_arg_uses_today() {
        let p = TimeProvider;
        let today = Local::now().date_naive();
        let out = p
            .query(&Query::new(format!("datediff {today}")))
            .await;
        assert_eq!(out[0].title, "0");
    }

    #[tokio::test]
    async fn datediff_invalid_no_output() {
        let p = TimeProvider;
        assert!(p.query(&Query::new("datediff foo")).await.is_empty());
        assert!(p.query(&Query::new("datediff 2024-01-01 :: bar")).await.is_empty());
        assert!(p.query(&Query::new("datediff ")).await.is_empty());
    }

    #[tokio::test]
    async fn datediff_today_keyword() {
        let p = TimeProvider;
        let out = p.query(&Query::new("datediff today :: today")).await;
        assert_eq!(out[0].title, "0");
    }

    #[test]
    fn parse_date_table() {
        assert!(parse_date("2024-01-01").is_some());
        assert!(parse_date("2024-12-31").is_some());
        assert!(parse_date("today").is_some());
        assert!(parse_date("now").is_some());
        assert_eq!(parse_date("not-a-date"), None);
        assert_eq!(parse_date("2024-13-01"), None); // bad month
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("01/01/2024"), None); // wrong format
    }

    #[test]
    fn datediff_leap_year_february() {
        // 2024 is leap -> Feb has 29 days
        let a = NaiveDate::parse_from_str("2024-02-01", "%Y-%m-%d").unwrap();
        let b = NaiveDate::parse_from_str("2024-03-01", "%Y-%m-%d").unwrap();
        assert_eq!((b - a).num_days(), 29);

        // 2023 is not leap -> Feb has 28 days
        let a = NaiveDate::parse_from_str("2023-02-01", "%Y-%m-%d").unwrap();
        let b = NaiveDate::parse_from_str("2023-03-01", "%Y-%m-%d").unwrap();
        assert_eq!((b - a).num_days(), 28);
    }
}
