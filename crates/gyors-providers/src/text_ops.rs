//! Lightweight text transforms - `upper`, `lower`, `rev`/`reverse`, `count`,
//! `sort`, `dedup`, `trim`, `nows`/`stripws`, `normalize`, `repeat`,
//! `lpad`/`rpad`. Each keyword takes rest of query as input and
//! produces a single candidate whose activation copies result (or, for
//! `count`, the info line)
//!
//! `sort` and `dedup` operate on lines (split on `\n`); paste a multi-line
//! block into input field to use them

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct TextOpsProvider;

#[async_trait]
impl Provider for TextOpsProvider {
    fn id(&self) -> &str {
        "text"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some((op, input)) = split_op(query.pattern()) else {
            return vec![];
        };
        let input = input.trim();
        if input.is_empty() {
            return vec![];
        }
        match op {
            "upper" | "uppercase" => one(result_candidate(&input.to_uppercase(), "uppercase")),
            "lower" | "lowercase" => one(result_candidate(&input.to_lowercase(), "lowercase")),
            "rev" | "reverse" => {
                let reversed: String = input.chars().rev().collect();
                one(result_candidate(&reversed, "reversed"))
            }
            "count" => one(count_candidate(input)),
            "sort" => {
                let sorted = sort_lines(input, false);
                one(result_candidate(&sorted, "sorted lines"))
            }
            "sortr" => {
                let sorted = sort_lines(input, true);
                one(result_candidate(&sorted, "sorted lines · reverse"))
            }
            "dedup" | "uniq" => {
                let deduped = dedup_lines(input);
                one(result_candidate(&deduped, "duplicate lines removed"))
            }
            "trim" => one(result_candidate(input.trim(), "trimmed")),
            "nows" | "stripws" => {
                let stripped: String = input.chars().filter(|c| !c.is_whitespace()).collect();
                one(result_candidate(&stripped, "whitespace removed"))
            }
            "normalize" | "normws" => {
                let normalized = normalize_whitespace(input);
                one(result_candidate(&normalized, "whitespace normalized"))
            }
            "repeat" => match split_count(input) {
                Some((n, text)) => {
                    if n == 0 {
                        return one(result_candidate("", "repeated 0 times"));
                    }
                    if n > 10_000 {
                        // Cap to avoid OOM if someone fat-fingers a huge n
                        return vec![];
                    }
                    let repeated: String = text.repeat(n as usize);
                    one(result_candidate(&repeated, &format!("repeated {n} times")))
                }
                None => vec![],
            },
            "lpad" | "padl" | "padleft" => match split_count(input) {
                Some((width, text)) => {
                    let padded = pad(text, width as usize, true);
                    one(result_candidate(&padded, &format!("left-padded to {width}")))
                }
                None => vec![],
            },
            "rpad" | "padr" | "padright" => match split_count(input) {
                Some((width, text)) => {
                    let padded = pad(text, width as usize, false);
                    one(result_candidate(&padded, &format!("right-padded to {width}")))
                }
                None => vec![],
            },
            _ => vec![],
        }
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let content = id
            .strip_prefix("text::")
            .ok_or_else(|| anyhow::anyhow!("invalid text candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(content.to_string()))
    }
}

fn split_op(pattern: &str) -> Option<(&str, &str)> {
    let (op, rest) = pattern.split_once(char::is_whitespace)?;
    Some((op, rest))
}

fn result_candidate(value: &str, kind: &str) -> Candidate {
    Candidate {
        id: format!("text::{value}"),
        title: truncate(value, 120),
        subtitle: Some(kind.into()),
        icon: Icon::SfSymbol("characters.uppercase".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Split on `\n`, sort case-insensitively, rejoin with `\n`. Keeps a
/// trailing newline if input had one (round-tripping user's text)
fn sort_lines(input: &str, reverse: bool) -> String {
    let trailing_nl = input.ends_with('\n');
    let mut lines: Vec<&str> = input.split('\n').collect();
    if trailing_nl {
        // `split('\n')` of "a\nb\n" yields ["a", "b", ""]. Drop the
        // empty trailing field so it doesn't sort to the top, and put
        // the newline back at end
        lines.pop();
    }
    lines.sort_by(|a, b| {
        let ord = a.to_lowercase().cmp(&b.to_lowercase());
        if reverse { ord.reverse() } else { ord }
    });
    let mut out = lines.join("\n");
    if trailing_nl {
        out.push('\n');
    }
    out
}

/// Collapse runs of whitespace to a single space, trim ends.
/// Counts any Unicode whitespace, not just ASCII
fn normalize_whitespace(input: &str) -> String {
    let trimmed = input.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut last_was_space = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out
}

/// Split input as "<count> <rest>". The count is u32; rest is the
/// remainder verbatim (without the leading separator). Returns None if
/// the first whitespace-delimited token doesn't parse as u32
fn split_count(input: &str) -> Option<(u32, &str)> {
    let (head, rest) = input.split_once(char::is_whitespace)?;
    let n = head.parse::<u32>().ok()?;
    Some((n, rest))
}

/// Pad `text` to character-count `width` with spaces. Returns input
/// unchanged when it's already at or above the target width
fn pad(text: &str, width: usize, on_left: bool) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_string();
    }
    let padding = " ".repeat(width - len);
    if on_left {
        format!("{padding}{text}")
    } else {
        format!("{text}{padding}")
    }
}

/// Drop duplicate lines while preserving first-occurrence order.
/// Comparison is case-sensitive; whitespace is significant
fn dedup_lines(input: &str) -> String {
    let trailing_nl = input.ends_with('\n');
    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<&str> = Vec::new();
    let mut iter = input.split('\n').peekable();
    while let Some(line) = iter.next() {
        // The synthetic empty trailing element from a final `\n` isn't a
        // real line - skip it so it doesn't get deduped against blanks
        // earlier in input
        if trailing_nl && iter.peek().is_none() && line.is_empty() {
            break;
        }
        if seen.insert(line) {
            kept.push(line);
        }
    }
    let mut out = kept.join("\n");
    if trailing_nl {
        out.push('\n');
    }
    out
}

fn count_candidate(input: &str) -> Candidate {
    let words = input.split_whitespace().count();
    let chars = input.chars().count();
    let bytes = input.len();
    let lines = input.lines().count().max(1);
    let summary = format!("{words} words · {chars} chars · {bytes} bytes · {lines} lines");
    Candidate {
        id: format!("text::{summary}"),
        title: summary.clone(),
        subtitle: Some(format!("count · {}", truncate(input, 60))),
        icon: Icon::SfSymbol("number".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn one(c: Candidate) -> Vec<Candidate> {
    vec![c]
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.into()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = TextOpsProvider;
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn keyword_with_no_text_no_match() {
        let p = TextOpsProvider;
        assert!(p.query(&Query::new("upper ")).await.is_empty());
    }

    #[tokio::test]
    async fn upper_transforms() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("upper hello world")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "HELLO WORLD");
    }

    #[tokio::test]
    async fn uppercase_alias() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("uppercase hi")).await;
        assert_eq!(out[0].title, "HI");
    }

    #[tokio::test]
    async fn lower_transforms() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("lower HELLO World")).await;
        assert_eq!(out[0].title, "hello world");
    }

    #[tokio::test]
    async fn rev_reverses_chars() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("rev abcdef")).await;
        assert_eq!(out[0].title, "fedcba");
    }

    #[tokio::test]
    async fn reverse_alias() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("reverse abc")).await;
        assert_eq!(out[0].title, "cba");
    }

    #[tokio::test]
    async fn count_reports_metrics() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("count hello world")).await;
        assert_eq!(out.len(), 1);
        let t = &out[0].title;
        assert!(t.contains("2 words"));
        assert!(t.contains("11 chars"));
    }

    #[tokio::test]
    async fn activate_copies_result() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("upper hi")).await;
        let effect = p.activate(&out[0].id, "default").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "HI"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = TextOpsProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn unknown_op_no_match() {
        let p = TextOpsProvider;
        assert!(p.query(&Query::new("bogus hello")).await.is_empty());
    }


    #[tokio::test]
    async fn sort_lines_alphabetical() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("sort banana\napple\ncherry")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "apple\nbanana\ncherry");
    }

    #[tokio::test]
    async fn sort_lines_case_insensitive() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("sort banana\nApple\nCherry")).await;
        // Case-insensitive: Apple < banana < Cherry
        assert_eq!(out[0].title, "Apple\nbanana\nCherry");
    }

    #[tokio::test]
    async fn sort_single_line_is_passthrough() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("sort just one line")).await;
        assert_eq!(out[0].title, "just one line");
    }

    #[tokio::test]
    async fn sortr_reverses_order() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("sortr a\nb\nc")).await;
        assert_eq!(out[0].title, "c\nb\na");
    }

    #[test]
    fn sort_lines_preserves_trailing_newline() {
        assert_eq!(sort_lines("b\na\n", false), "a\nb\n");
        assert_eq!(sort_lines("b\na", false), "a\nb");
    }

    #[test]
    fn sort_lines_handles_empty_lines() {
        // Blank lines are sorted alongside content lines
        let out = sort_lines("z\n\na", false);
        assert_eq!(out, "\na\nz");
    }


    #[tokio::test]
    async fn dedup_drops_repeated_lines() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("dedup a\nb\na\nc\nb")).await;
        assert_eq!(out[0].title, "a\nb\nc");
    }

    #[tokio::test]
    async fn dedup_preserves_first_occurrence_order() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("dedup z\na\nz\nb\na")).await;
        assert_eq!(out[0].title, "z\na\nb");
    }

    #[tokio::test]
    async fn dedup_uniq_alias() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("uniq a\nb\na")).await;
        assert_eq!(out[0].title, "a\nb");
    }

    #[tokio::test]
    async fn dedup_case_sensitive() {
        // Different case = different line; both kept
        let p = TextOpsProvider;
        let out = p.query(&Query::new("dedup Hello\nhello\nHELLO")).await;
        assert_eq!(out[0].title, "Hello\nhello\nHELLO");
    }

    #[test]
    fn dedup_lines_preserves_trailing_newline() {
        assert_eq!(dedup_lines("a\nb\na\n"), "a\nb\n");
    }

    #[test]
    fn dedup_lines_keeps_one_blank() {
        // Multiple blank lines collapse to one (first wins)
        assert_eq!(dedup_lines("a\n\nb\n\nc"), "a\n\nb\nc");
    }


    #[tokio::test]
    async fn trim_strips_ends_only() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("trim    hi  there   ")).await;
        assert_eq!(out[0].title, "hi  there");
    }

    #[tokio::test]
    async fn trim_preserves_internal_whitespace() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("trim a   b\nc\td")).await;
        assert_eq!(out[0].title, "a   b\nc\td");
    }

    #[tokio::test]
    async fn nows_removes_all_whitespace() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("nows hello world\tfoo\n bar")).await;
        assert_eq!(out[0].title, "helloworldfoobar");
    }

    #[tokio::test]
    async fn stripws_alias_for_nows() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("stripws hi\tbye")).await;
        assert_eq!(out[0].title, "hibye");
    }

    #[tokio::test]
    async fn normalize_collapses_runs_to_single_space() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("normalize  a  b\tc\nd  ")).await;
        assert_eq!(out[0].title, "a b c d");
    }

    #[test]
    fn normalize_pure_unicode_whitespace() {
        // U+00A0 NBSP is whitespace-class
        assert_eq!(normalize_whitespace("a\u{00A0}b"), "a b");
        assert_eq!(normalize_whitespace("a\t\tb"), "a b");
        assert_eq!(normalize_whitespace("    "), "");
    }


    #[tokio::test]
    async fn repeat_three_times() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("repeat 3 ab")).await;
        assert_eq!(out[0].title, "ababab");
    }

    #[tokio::test]
    async fn repeat_zero_yields_empty() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("repeat 0 hello")).await;
        assert_eq!(out[0].title, "");
    }

    #[tokio::test]
    async fn repeat_huge_count_rejected() {
        let p = TextOpsProvider;
        // Cap at 10_000 - anything larger is no-output (safe-guard against
        // accidental megabytes of text)
        assert!(p.query(&Query::new("repeat 100000 a")).await.is_empty());
    }

    #[tokio::test]
    async fn repeat_non_numeric_count_no_output() {
        let p = TextOpsProvider;
        assert!(p.query(&Query::new("repeat foo bar")).await.is_empty());
    }


    #[tokio::test]
    async fn lpad_pads_with_spaces() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("lpad 6 hi")).await;
        assert_eq!(out[0].title, "    hi");
    }

    #[tokio::test]
    async fn rpad_pads_with_spaces() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("rpad 6 hi")).await;
        assert_eq!(out[0].title, "hi    ");
    }

    #[tokio::test]
    async fn pad_aliases_match() {
        let p = TextOpsProvider;
        let lpad = p.query(&Query::new("lpad 4 ab")).await;
        let padl = p.query(&Query::new("padl 4 ab")).await;
        let padleft = p.query(&Query::new("padleft 4 ab")).await;
        assert_eq!(lpad[0].title, padl[0].title);
        assert_eq!(lpad[0].title, padleft[0].title);
    }

    #[tokio::test]
    async fn pad_no_op_when_text_at_or_above_width() {
        let p = TextOpsProvider;
        let out = p.query(&Query::new("lpad 2 hello")).await;
        assert_eq!(out[0].title, "hello");
    }

    #[test]
    fn pad_pure_unicode_width() {
        // Width counts characters (codepoints), not bytes
        assert_eq!(pad("é", 4, true), "   é");
        assert_eq!(pad("🚀", 3, false), "🚀  ");
    }

    #[test]
    fn split_count_table() {
        assert_eq!(split_count("3 hello"), Some((3, "hello")));
        assert_eq!(split_count("0 a"), Some((0, "a")));
        assert_eq!(split_count("foo bar"), None);
        assert_eq!(split_count(""), None);
        assert_eq!(split_count("3"), None); // no rest
    }
}
