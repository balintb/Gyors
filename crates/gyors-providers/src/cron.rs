//! Natural-language -> cron-expression converter
//!
//! Keyword: `cron <phrase>`. Examples that round-trip cleanly:
//!
//! ```text
//! cron every minute            -> * * * * *
//! cron every 5 minutes         -> */5 * * * *
//! cron every hour              -> 0 * * * *
//! cron every 3 hours           -> 0 */3 * * *
//! cron every day at 6pm        -> 0 18 * * *
//! cron daily at 9:30am         -> 30 9 * * *
//! cron weekdays at 8           -> 0 8 * * 1-5
//! cron weekends at noon        -> 0 12 * * 0,6
//! cron monday at 7pm           -> 0 19 * * 1
//! cron mon, wed, fri at 6:30   -> 30 6 * * 1,3,5
//! cron at midnight             -> 0 0 * * *
//! cron at 23:45                -> 45 23 * * *
//! cron the 1st at 9am          -> 0 9 1 * *
//! cron 15th of every month at noon -> 0 12 15 * *
//! ```
//!
//! Why deterministic over AI: the launcher fires this thousands of
//! times across a user's lifetime; an LLM "usually" producing
//! `0 18 * * *` and "occasionally" producing `0 6 * * *` for "6pm"
//! is a dealbreaker. This parser is offline, fast, and predictable.
//! For phrasings outside its grammar user still gets a clear
//! "couldn't parse" row instead of a silent wrong answer

use anyhow::Result;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct CronProvider;

#[async_trait]
impl Provider for CronProvider {
    fn id(&self) -> &str {
        "cron"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(rest) = strip_keyword(query.pattern()) else {
            return Vec::new();
        };
        if rest.is_empty() {
            return vec![help_candidate()];
        }
        match parse_cron(rest) {
            Some(expr) => vec![success_candidate(rest, &expr)],
            None => vec![error_candidate(rest)],
        }
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        if let Some(expr) = id.strip_prefix("cron::ok::") {
            return match action {
                "default" | "copy" => Ok(Effect::CopyToClipboard(expr.to_string())),
                "install" => Ok(Effect::RunShell(install_command(expr))),
                "explain" => Ok(Effect::ShowText {
                    text: explain(expr),
                    label: format!("Explained · {expr}"),
                    language: None,
                    editable_path: None,
                }),
                other => anyhow::bail!("unknown cron action: {other}"),
            };
        }
        if id == "cron::error" || id == "cron::help" {
            return Ok(Effect::None);
        }
        anyhow::bail!("invalid cron candidate id: {id}")
    }
}


fn strip_keyword(pattern: &str) -> Option<&str> {
    let trimmed = pattern.trim_start();
    if trimmed.eq_ignore_ascii_case("cron") {
        return Some("");
    }
    if trimmed.len() > 4 {
        let (head, tail) = trimmed.split_at(4);
        if head.eq_ignore_ascii_case("cron") && tail.starts_with(char::is_whitespace) {
            return Some(tail.trim());
        }
    }
    None
}

fn success_candidate(input: &str, expr: &str) -> Candidate {
    Candidate {
        id: format!("cron::ok::{expr}"),
        title: expr.to_string(),
        subtitle: Some(format!("from {input:?}  ·  ↵ Copy  ·  → for actions")),
        icon: Icon::SfSymbol("clock.arrow.circlepath".into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Copy"),
            Action::new("install", "Install (append to crontab)"),
            Action::new("explain", "Explain"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn error_candidate(input: &str) -> Candidate {
    Candidate {
        id: "cron::error".into(),
        title: format!("couldn't parse {input:?}"),
        subtitle: Some(
            "Try: every day at 6pm · weekdays at 9 · every 5 minutes · monday at 7pm".into(),
        ),
        icon: Icon::SfSymbol("exclamationmark.triangle".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn help_candidate() -> Candidate {
    Candidate {
        id: "cron::help".into(),
        title: "Cron expression generator".into(),
        subtitle: Some(
            "Type a schedule: 'every day at 6pm', 'weekdays at 9', 'every 15 minutes'".into(),
        ),
        icon: Icon::SfSymbol("clock.arrow.circlepath".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Build a shell command that appends the cron line to user's
/// existing crontab without clobbering it. We use the `crontab -l`
/// fallback `|| true` so users with no current crontab dont trip
/// on `crontab: no crontab for ...`
fn install_command(expr: &str) -> String {
    let line = format!("{expr} # added by gyors");
    format!("(crontab -l 2>/dev/null || true; echo '{line}') | crontab -")
}


/// Parse a natural-language schedule into a 5-field cron expression.
/// Returns None when the phrase doesn't match any known shape - we'd
/// rather show "couldn't parse" than emit a wrong schedule
pub fn parse_cron(input: &str) -> Option<String> {
    let s = normalize(input);

    if s == "every minute" {
        return Some("* * * * *".into());
    }
    if let Some(n) = parse_every_n(&s, "minute") {
        return Some(format!("*/{n} * * * *"));
    }
    if s == "every hour" || s == "hourly" {
        return Some("0 * * * *".into());
    }
    if let Some(n) = parse_every_n(&s, "hour") {
        return Some(format!("0 */{n} * * *"));
    }

    // "every other day", "every second day", "every 3 days at 6pm".
    // Cron has no real way to say "every N days" across month
    // boundaries - `*/N` in DOM resets to 1 each month, so it's
    // approximate. We emit it anyway because it's the closest cron
    // can come; the explanation row is honest about the gap
    if let Some(expr) = parse_every_n_days(&s) {
        return Some(expr);
    }

    // -- Day-of-month forms (run before generic time parsing so phrases
    // like "the 15th at noon" dont match a daily template). ---------
    if let Some(expr) = parse_day_of_month(&s) {
        return Some(expr);
    }

    // Pattern: "<dayspec> at <time>" or "at <time>" or just "<time>"
    let (dayspec, time_part) = split_at_time(&s);
    let (hour, minute) = parse_time(time_part)?;
    let dow_field = match parse_dow(dayspec) {
        // Daily / no-spec -> wildcard
        DowSpec::EveryDay => "*".to_string(),
        DowSpec::Specific(s) => s,
        // Non-empty unparseable spec ("every second day", "on doomsday").
        // Reject the whole parse so user sees the "couldn't parse"
        // helper row instead of a silently-wrong every-day schedule
        DowSpec::Invalid => return None,
    };
    Some(format!("{minute} {hour} * * {dow_field}"))
}

/// "every other day at 6pm" -> `0 18 */2 * *`.
/// "every 3 days at 9" -> `0 9 */3 * *`.
/// "every fifth day" without a time -> None (we still need a time)
fn parse_every_n_days(s: &str) -> Option<String> {
    let rest = s.strip_prefix("every ")?;
    let (head, time_part) = match rest.split_once(" at ") {
        Some((h, t)) => (h.trim(), t.trim()),
        None => (rest.trim(), ""),
    };
    // Head must be one of: "<word> day", "<word> days", "N day(s)"
    let tokens: Vec<&str> = head.split_whitespace().collect();
    let n = match tokens.as_slice() {
        [count, "day"] | [count, "days"] => word_or_ordinal_to_number(count)?,
        _ => return None,
    };
    if n < 2 {
        return None;
    }
    // No time -> default to midnight, mirroring how Unix `cron` users
    // think of "daily" jobs. Users who want a specific hour just say so
    let (hour, minute) = if time_part.is_empty() {
        (0, 0)
    } else {
        parse_time(time_part)?
    };
    Some(format!("{minute} {hour} */{n} * *"))
}

/// "other" / "second" -> 2, "third" -> 3, "5th" / "fifth" -> 5, ...
/// Returns None for "every", "day", and other non-count words
fn word_or_ordinal_to_number(s: &str) -> Option<u32> {
    if let Some(n) = parse_ordinal(s) {
        return Some(n);
    }
    Some(match s {
        "other" | "second" => 2,
        "third" => 3,
        "fourth" => 4,
        "fifth" => 5,
        "sixth" => 6,
        "seventh" => 7,
        "eighth" => 8,
        "ninth" => 9,
        "tenth" => 10,
        _ => return None,
    })
}

/// Lowercase + collapse whitespace + strip filler words ("on", "the",
/// "of"). Lets users type colloquially without us writing a regex
/// for every variant
fn normalize(input: &str) -> String {
    let lower: String = input.to_lowercase();
    let cleaned = lower
        .split_whitespace()
        .filter(|w| !matches!(*w, "on"))
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.replace(" o'clock", "")
}

/// Parses `every <n> <unit>s` where `<unit>` is "minute" or "hour".
/// Returns the count if the shape matches and n >= 1
fn parse_every_n(s: &str, unit: &str) -> Option<u32> {
    let plural = format!("{unit}s");
    let prefix = "every ";
    let rest = s.strip_prefix(prefix)?;
    let parts: Vec<&str> = rest.splitn(2, ' ').collect();
    if parts.len() != 2 {
        return None;
    }
    let n: u32 = parts[0].parse().ok()?;
    if n < 1 {
        return None;
    }
    let tail = parts[1].trim();
    if tail == unit || tail == plural {
        Some(n)
    } else {
        None
    }
}

/// Day-of-month forms: "the 1st at 9", "15th of every month at noon",
/// "1st and 15th at 6:30am"
fn parse_day_of_month(s: &str) -> Option<String> {
    let trimmed = s.trim_start_matches("the ");
    // Must contain " at " for a time spec, and the leading token(s)
    // must look like ordinals
    let (head, time_part) = trimmed.rsplit_once(" at ")?;
    let head = head.replace(" of every month", "").replace(" of the month", "");
    let day_field = parse_ordinals(&head)?;
    let (hour, minute) = parse_time(time_part)?;
    Some(format!("{minute} {hour} {day_field} * *"))
}

/// "1st and 15th" -> "1,15".  "5th" -> "5".  Returns None if anything
/// in the comma/and-separated list doesn't look like an ordinal
fn parse_ordinals(s: &str) -> Option<String> {
    let parts: Vec<&str> = s
        .split([',', ' '])
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "and")
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        out.push(parse_ordinal(p)?);
    }
    Some(out.iter().map(u32::to_string).collect::<Vec<_>>().join(","))
}

fn parse_ordinal(s: &str) -> Option<u32> {
    let stripped = s
        .trim_end_matches("st")
        .trim_end_matches("nd")
        .trim_end_matches("rd")
        .trim_end_matches("th");
    let n: u32 = stripped.parse().ok()?;
    if (1..=31).contains(&n) {
        Some(n)
    } else {
        None
    }
}

/// Splits a phrase into (dayspec, time). For an input that has " at ",
/// the dayspec is prefix and `time` is the rest. With no " at ",
/// the dayspec is the whole input and `time` is empty
fn split_at_time(s: &str) -> (&str, &str) {
    if let Some(idx) = s.find(" at ") {
        let day = &s[..idx];
        let time = &s[idx + 4..];
        (day.trim(), time.trim())
    } else if let Some(rest) = s.strip_prefix("at ") {
        ("", rest.trim())
    } else {
        // Whole input might be a bare time ("6pm", "noon"). Treat
        // dayspec as "every day" implicitly
        if looks_like_time(s) {
            ("", s)
        } else {
            (s, "")
        }
    }
}

/// Parse a time string into (hour, minute). Supports:
/// `6pm`, `6:30pm`, `9am`, `09:00`, `18:00`, `noon`, `midnight`
fn parse_time(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s == "noon" {
        return Some((12, 0));
    }
    if s == "midnight" {
        return Some((0, 0));
    }

    // Strip trailing am/pm to drive the 12-hour conversion
    let (digits, suffix) = split_ampm(s);
    let (h_str, m_str) = match digits.split_once(':') {
        Some((h, m)) => (h, m),
        None => (digits, "0"),
    };
    let mut hour: u32 = h_str.parse().ok()?;
    let minute: u32 = m_str.parse().ok()?;
    if minute > 59 {
        return None;
    }
    match suffix {
        Some("am") => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            if hour == 12 {
                hour = 0;
            }
        }
        Some("pm") => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            if hour < 12 {
                hour += 12;
            }
        }
        None => {
            if hour > 23 {
                return None;
            }
        }
        _ => return None,
    }
    Some((hour, minute))
}

fn split_ampm(s: &str) -> (&str, Option<&str>) {
    if let Some(stripped) = s.strip_suffix("am") {
        (stripped.trim(), Some("am"))
    } else if let Some(stripped) = s.strip_suffix("pm") {
        (stripped.trim(), Some("pm"))
    } else {
        (s, None)
    }
}

/// Cheap "does this look like a bare time?" check - used to route
/// inputs like `6pm` or `noon` through the time-only path
fn looks_like_time(s: &str) -> bool {
    s == "noon"
        || s == "midnight"
        || s.ends_with("am")
        || s.ends_with("pm")
        || (s.contains(':') && s.chars().all(|c| c.is_ascii_digit() || c == ':'))
}

/// Result of trying to parse a day-of-week spec
///
/// The three-way distinction matters: an empty / "daily" spec means
/// "no constraint" and caller should emit `*`; an unparseable
/// spec means input names *something specific* we dont understand,
/// and caller should fail parse rather than silently produce a
/// daily schedule
enum DowSpec {
    /// No constraint - wildcard. (Empty input, or "daily" / "every day".)
    EveryDay,
    /// A parsed cron DOW field - `"1-5"`, `"1,3,5"`, `"0,6"`, etc
    Specific(String),
    /// Input was something we couldn't parse. Caller should reject
    /// the whole expression - silently emitting a daily schedule
    /// would be a wrong answer, and that's worse than a clear "I
    /// dont understand."
    Invalid,
}

/// Parse a day-of-week spec into a cron DOW field.
/// Recognises:
/// - empty / `every day` / `daily` -> `EveryDay`
/// - `weekdays` -> `1-5`
/// - `weekends` -> `0,6`
/// - single weekday names (`monday`, `mon`, ...) -> numeric
/// - lists: `monday and friday`, `mon, wed, fri`
/// - everything else -> `Invalid`
fn parse_dow(s: &str) -> DowSpec {
    let s = s.trim();
    if s.is_empty() || s == "every day" || s == "daily" {
        return DowSpec::EveryDay;
    }
    if s == "weekdays" || s == "every weekday" {
        return DowSpec::Specific("1-5".into());
    }
    if s == "weekends" || s == "every weekend" {
        return DowSpec::Specific("0,6".into());
    }
    let parts: Vec<&str> = s
        .split([',', ' '])
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "and" && *p != "every")
        .collect();
    if parts.is_empty() {
        return DowSpec::EveryDay;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in &parts {
        match weekday_to_num(p) {
            Some(n) => nums.push(n),
            None => return DowSpec::Invalid,
        }
    }
    if nums.len() == 1 {
        return DowSpec::Specific(nums[0].to_string());
    }
    DowSpec::Specific(nums.iter().map(u32::to_string).collect::<Vec<_>>().join(","))
}

fn weekday_to_num(s: &str) -> Option<u32> {
    Some(match s {
        "sunday" | "sun" => 0,
        "monday" | "mon" => 1,
        "tuesday" | "tue" | "tues" => 2,
        "wednesday" | "wed" | "weds" => 3,
        "thursday" | "thu" | "thur" | "thurs" => 4,
        "friday" | "fri" => 5,
        "saturday" | "sat" => 6,
        _ => return None,
    })
}


/// Plain-English description of a cron expression. Skips the deeper
/// edge cases (step ranges in DOW, lists of hours, etc.) - for the
/// shapes our parser produces, this covers them all
pub fn explain(expr: &str) -> String {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return format!("{expr}\n\n(not a 5-field cron expression)");
    }
    let (m, h, dom, mon, dow) = (parts[0], parts[1], parts[2], parts[3], parts[4]);
    let mut lines = vec![format!("{expr}"), String::new()];
    let when = describe_minute_hour(m, h);
    lines.push(format!("• {when}"));
    if dom != "*" {
        lines.push(format!("• {}", describe_dom(dom)));
    }
    if mon != "*" {
        lines.push(format!("• in month {mon}"));
    }
    if dow != "*" {
        lines.push(format!("• on {}", describe_dow(dow)));
    }
    lines.join("\n")
}

/// Render the day-of-month field as English. The `*/N` step form
/// gets a footnote about the month-boundary reset - cron picks days
/// 1, 1+N, 1+2N, ... each month, so a user asking for "every other
/// day" sees a 1-day gap on month transitions where N=2 would
/// otherwise put a 2-day gap. Saying that out loud beats letting
/// users discover it the hard way
fn describe_dom(dom: &str) -> String {
    if let Some(n) = dom.strip_prefix("*/") {
        return format!(
            "every {n} days of the month (note: cron resets at the start of each month, so the gap shortens at month boundaries)",
        );
    }
    if dom.contains(',') {
        return format!("on days {dom} of the month");
    }
    format!("on day {dom} of the month")
}

fn describe_minute_hour(m: &str, h: &str) -> String {
    if m == "*" && h == "*" {
        return "every minute".into();
    }
    if let Some(n) = m.strip_prefix("*/") {
        if h == "*" {
            return format!("every {n} minutes");
        }
    }
    if let Some(n) = h.strip_prefix("*/") {
        if m == "0" {
            return format!("every {n} hours");
        }
    }
    if h == "*" {
        return format!("at minute {m} of every hour");
    }
    let minute_part = if m == "0" {
        ":00".to_string()
    } else if let Ok(n) = m.parse::<u32>() {
        format!(":{n:02}")
    } else {
        format!(":{m}")
    };
    format!("at {h}{minute_part}")
}

fn describe_dow(dow: &str) -> String {
    if dow == "1-5" {
        return "weekdays".into();
    }
    if dow == "0,6" {
        return "weekends".into();
    }
    let names = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    let parts: Vec<String> = dow
        .split(',')
        .filter_map(|p| p.parse::<usize>().ok().and_then(|n| names.get(n).map(|s| (*s).to_string())))
        .collect();
    if parts.is_empty() {
        return dow.into();
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(input: &str) -> Option<String> {
        parse_cron(input)
    }


    #[test]
    fn every_minute_uses_star_minute_field() {
        assert_eq!(p("every minute"), Some("* * * * *".into()));
    }

    #[test]
    fn every_n_minutes_emits_step() {
        assert_eq!(p("every 5 minutes"), Some("*/5 * * * *".into()));
        assert_eq!(p("every 15 minutes"), Some("*/15 * * * *".into()));
    }

    #[test]
    fn every_hour_pins_minute_to_zero() {
        assert_eq!(p("every hour"), Some("0 * * * *".into()));
        assert_eq!(p("hourly"), Some("0 * * * *".into()));
    }

    #[test]
    fn every_n_hours_steps_correctly() {
        assert_eq!(p("every 3 hours"), Some("0 */3 * * *".into()));
    }


    #[test]
    fn pm_means_after_noon() {
        assert_eq!(p("every day at 6pm"), Some("0 18 * * *".into()));
        assert_eq!(p("daily at 6pm"), Some("0 18 * * *".into()));
        assert_eq!(p("at 6pm"), Some("0 18 * * *".into()));
    }

    #[test]
    fn am_keeps_hour_or_zeroes_noon() {
        assert_eq!(p("every day at 9am"), Some("0 9 * * *".into()));
        // 12am === midnight -> 0 in cron
        assert_eq!(p("at 12am"), Some("0 0 * * *".into()));
    }

    #[test]
    fn twentyfour_hour_time_passes_through() {
        assert_eq!(p("at 18:00"), Some("0 18 * * *".into()));
        assert_eq!(p("daily at 09:30"), Some("30 9 * * *".into()));
        assert_eq!(p("at 23:45"), Some("45 23 * * *".into()));
    }

    #[test]
    fn noon_and_midnight_are_words_we_understand() {
        assert_eq!(p("at noon"), Some("0 12 * * *".into()));
        assert_eq!(p("at midnight"), Some("0 0 * * *".into()));
        assert_eq!(p("daily at noon"), Some("0 12 * * *".into()));
    }

    #[test]
    fn minutes_within_the_hour_carry_through() {
        assert_eq!(p("every day at 6:30pm"), Some("30 18 * * *".into()));
        assert_eq!(p("at 9:30am"), Some("30 9 * * *".into()));
    }


    #[test]
    fn weekdays_collapses_to_one_to_five() {
        assert_eq!(p("weekdays at 8"), Some("0 8 * * 1-5".into()));
        assert_eq!(p("every weekday at 9am"), Some("0 9 * * 1-5".into()));
    }

    #[test]
    fn weekends_collapses_to_sun_and_sat() {
        assert_eq!(p("weekends at noon"), Some("0 12 * * 0,6".into()));
    }

    #[test]
    fn single_weekday_full_or_short() {
        assert_eq!(p("monday at 7pm"), Some("0 19 * * 1".into()));
        assert_eq!(p("mon at 7pm"), Some("0 19 * * 1".into()));
        assert_eq!(p("friday at 5pm"), Some("0 17 * * 5".into()));
    }

    #[test]
    fn weekday_lists_handle_commas_and_and() {
        assert_eq!(
            p("monday and friday at 6:30pm"),
            Some("30 18 * * 1,5".into()),
        );
        assert_eq!(
            p("mon, wed, fri at 6:30am"),
            Some("30 6 * * 1,3,5".into()),
        );
    }


    #[test]
    fn first_of_month_pins_dom() {
        assert_eq!(p("the 1st at 9am"), Some("0 9 1 * *".into()));
        assert_eq!(p("1st of every month at 9am"), Some("0 9 1 * *".into()));
    }

    #[test]
    fn fifteenth_with_explicit_time() {
        assert_eq!(
            p("15th of every month at noon"),
            Some("0 12 15 * *".into()),
        );
    }

    #[test]
    fn ordinal_lists_collapse_to_comma_form() {
        assert_eq!(
            p("1st and 15th at 6:30am"),
            Some("30 6 1,15 * *".into()),
        );
    }


    #[test]
    fn nonsense_input_rejects_cleanly() {
        assert_eq!(p("once in a blue moon"), None);
        assert_eq!(p(""), None);
        assert_eq!(p("at 25:00"), None); // invalid hour
        assert_eq!(p("at 6:99am"), None); // invalid minute
        assert_eq!(p("every 0 minutes"), None);
    }

    //
    // Regression for a wrong-answer bug: "every second day at 3pm"
    // used to silently resolve to "0 15 * * *" (every day at 15:00)
    // because parser saw "every second day" as an unrecognised
    // dayspec and fell through to the every-day default. Now we
    // explicitly recognise the every-N-days shape AND we reject
    // unparseable dayspecs instead of treating them as "*"

    #[test]
    fn every_second_day_uses_dom_step_not_daily_wildcard() {
        // "every second day" -> every other day. Cron's closest
        // expression is `*/2` in DOM (which resets at month
        // boundaries, but is still closer than "every day")
        assert_eq!(p("every second day at 3pm"), Some("0 15 */2 * *".into()));
        assert_ne!(p("every second day at 3pm"), Some("0 15 * * *".into()));
    }

    #[test]
    fn every_other_day_is_synonym_for_second() {
        assert_eq!(p("every other day at 6am"), Some("0 6 */2 * *".into()));
    }

    #[test]
    fn every_n_days_with_word_or_digit() {
        assert_eq!(p("every third day at 9am"), Some("0 9 */3 * *".into()));
        assert_eq!(p("every 3 days at 9am"), Some("0 9 */3 * *".into()));
        assert_eq!(p("every 4th day at noon"), Some("0 12 */4 * *".into()));
    }

    #[test]
    fn every_n_days_without_time_defaults_to_midnight() {
        // Pragmatic: a bare "every other day" without a time is
        // equivalent to a daily cron entry - Unix users default
        // those to midnight
        assert_eq!(p("every other day"), Some("0 0 */2 * *".into()));
    }

    #[test]
    fn every_one_day_is_just_daily() {
        // n < 2 isn't really "every Nth" - fall back to whatever
        // rest of parser would produce. Nonsense rejected
        assert!(p("every 1 day at 6pm").is_none());
    }

    #[test]
    fn unrecognised_dayspec_rejects_instead_of_silently_defaulting() {
        // The sneaky regression: user typed something specific
        // we dont understand, so we MUST NOT silently produce a
        // daily schedule. Better to show "couldn't parse" and let
        // user reword
        assert!(p("on doomsday at 3pm").is_none());
        assert!(p("the schwartz at noon").is_none());
        assert!(p("every blue moon at 6pm").is_none());
    }


    #[test]
    fn explain_calls_out_month_boundary_reset_for_step_dom() {
        let s = explain("0 15 */2 * *");
        assert!(s.contains("every 2 days"));
        assert!(s.contains("month"), "must mention the month-reset caveat");
    }

    #[test]
    fn case_insensitive_input() {
        assert_eq!(p("EVERY DAY AT 6PM"), Some("0 18 * * *".into()));
        assert_eq!(p("Weekdays At 9AM"), Some("0 9 * * 1-5".into()));
    }


    #[tokio::test]
    async fn keyword_alone_offers_help() {
        let p = CronProvider;
        let out = p.query(&Query::new("cron")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "cron::help");
    }

    #[tokio::test]
    async fn parsable_phrase_returns_expression_row() {
        let p = CronProvider;
        let out = p.query(&Query::new("cron every day at 6pm")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "0 18 * * *");
        assert_eq!(out[0].id, "cron::ok::0 18 * * *");
    }

    #[tokio::test]
    async fn unparsable_phrase_returns_help_row_not_silence() {
        // Returning `vec![]` would let the launcher fall through to
        // unrelated providers and confuse user. A clear error
        // candidate keeps the cron context visible
        let p = CronProvider;
        let out = p.query(&Query::new("cron schmron schmoo")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "cron::error");
    }

    #[tokio::test]
    async fn copy_action_returns_cron_expression_on_clipboard() {
        let p = CronProvider;
        let effect = p
            .activate(&"cron::ok::0 18 * * *".into(), "default")
            .await
            .unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "0 18 * * *"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn install_action_emits_safe_append_shell_pipeline() {
        // Append-not-overwrite - using `crontab -l | { cat; echo ...; }`
        // ensures user's existing crontab survives. The `|| true`
        // covers users who have no crontab yet
        let p = CronProvider;
        let effect = p
            .activate(&"cron::ok::0 18 * * *".into(), "install")
            .await
            .unwrap();
        match effect {
            Effect::RunShell(s) => {
                assert!(s.contains("crontab -l"), "must read existing crontab");
                assert!(s.contains("crontab -"), "must pipe back into crontab");
                assert!(s.contains("0 18 * * *"));
            }
            other => panic!("expected RunShell, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn explain_action_emits_human_readable_text() {
        let p = CronProvider;
        let effect = p
            .activate(&"cron::ok::0 18 * * 1-5".into(), "explain")
            .await
            .unwrap();
        match effect {
            Effect::ShowText { text, .. } => {
                assert!(text.contains("18:00") || text.contains("at 18"));
                assert!(text.contains("weekdays"));
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_keyword_no_match() {
        let p = CronProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
        assert!(p.query(&Query::new("at 6pm")).await.is_empty());
    }

    #[tokio::test]
    async fn foreign_id_errors() {
        let p = CronProvider;
        assert!(p.activate(&"apps::x".into(), "default").await.is_err());
    }

    #[tokio::test]
    async fn unknown_action_errors() {
        let p = CronProvider;
        assert!(p
            .activate(&"cron::ok::0 18 * * *".into(), "nonsense")
            .await
            .is_err());
    }


    #[test]
    fn explain_handles_weekdays_and_time() {
        let s = explain("0 9 * * 1-5");
        assert!(s.contains("9:00") || s.contains("at 9"));
        assert!(s.contains("weekdays"));
    }

    #[test]
    fn explain_handles_every_n_minutes() {
        assert!(explain("*/5 * * * *").contains("every 5 minutes"));
    }

    #[test]
    fn explain_handles_dom() {
        assert!(explain("0 12 15 * *").contains("day 15"));
    }

    #[test]
    fn explain_rejects_malformed() {
        assert!(explain("0 18 * *").contains("not a 5-field"));
    }
}
