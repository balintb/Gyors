//! Search & switch to open browser tabs. Keyword: `tab <query>`
//!
//! Queries Safari + Chrome via AppleScript on demand (NOT cached - the
//! tab list is live). Needs Automation permission for each browser the
//! first time it's used; menu-bar "Request Finder Automation Access"
//! path can re-trigger those prompts if they're accidentally denied
//!
//! Activation emits an AppleScript that focuses the target window + tab
//! and brings the browser to the front

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::process::Command;

pub struct BrowserTabsProvider;

/// Opaque row emitted per open tab. The id is `tab::<browser>::<window_id>::<tab_index>`
/// so activator can re-target the exact tab without URL
const RESULT_LIMIT: usize = 25;

#[async_trait]
impl Provider for BrowserTabsProvider {
    fn id(&self) -> &str {
        "tab"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(rest) = strip_keyword(pattern) else { return vec![]; };
        let filter = rest.trim().to_lowercase();
        // Run the two AppleScripts in parallel - neither is hot and
        // neither needs to block the other
        let (safari, chrome) = tokio::join!(
            tokio::task::spawn_blocking(fetch_safari_tabs),
            tokio::task::spawn_blocking(fetch_chrome_tabs),
        );
        let mut all: Vec<Tab> = Vec::new();
        all.extend(safari.unwrap_or_default());
        all.extend(chrome.unwrap_or_default());

        all.into_iter()
            .filter(|t| filter.is_empty() || tab_matches(t, &filter))
            .take(RESULT_LIMIT)
            .map(to_candidate)
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let rest = id
            .strip_prefix("tab::")
            .ok_or_else(|| anyhow::anyhow!("invalid tab candidate id: {id}"))?;
        // Format: `<browser>::<window_id>::<tab_index>`
        let parts: Vec<&str> = rest.splitn(3, "::").collect();
        if parts.len() != 3 {
            anyhow::bail!("malformed tab id: {id}");
        }
        let (browser, window_id, tab_index) = (parts[0], parts[1], parts[2]);
        Ok(Effect::RunAppleScript(switch_script(browser, window_id, tab_index)))
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    if s == "tab" || s == "tabs" { return Some(""); }
    s.strip_prefix("tab ").or_else(|| s.strip_prefix("tabs "))
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub browser: &'static str,
    pub window_id: String,
    pub tab_index: String,
    pub title: String,
    pub url: String,
}

fn tab_matches(t: &Tab, filter_lower: &str) -> bool {
    t.title.to_lowercase().contains(filter_lower)
        || t.url.to_lowercase().contains(filter_lower)
}

fn to_candidate(t: Tab) -> Candidate {
    let browser_name = match t.browser {
        "safari" => "Safari",
        "chrome" => "Chrome",
        other => other,
    };
    Candidate {
        id: format!("tab::{}::{}::{}", t.browser, t.window_id, t.tab_index),
        title: t.title,
        subtitle: Some(format!("{}  ·  {}", browser_name, truncate(&t.url, 80))),
        icon: Icon::SfSymbol("safari.fill".into()),
        kind: CandidateKind::Web,
        actions: vec![Action::primary("Switch to Tab")],
        search_text: format!("{} {} {}", browser_name, t.url, t.url),
        bypass_rank: false,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

fn fetch_safari_tabs() -> Vec<Tab> {
    let script = r#"
        tell application "System Events"
            if not (exists application process "Safari") then return ""
        end tell
        tell application "Safari"
            set out to ""
            repeat with w in windows
                set wid to id of w
                set i to 1
                repeat with t in tabs of w
                    set tname to name of t
                    set turl to URL of t
                    if turl is missing value then set turl to ""
                    set out to out & wid & "\t" & i & "\t" & tname & "\t" & turl & linefeed
                    set i to i + 1
                end repeat
            end repeat
            return out
        end tell
    "#;
    parse_tab_list(&run_osascript(script), "safari")
}

fn fetch_chrome_tabs() -> Vec<Tab> {
    let script = r#"
        tell application "System Events"
            if not (exists application process "Google Chrome") then return ""
        end tell
        tell application "Google Chrome"
            set out to ""
            repeat with w in windows
                set wid to id of w
                set i to 1
                repeat with t in tabs of w
                    set tname to title of t
                    set turl to URL of t
                    if turl is missing value then set turl to ""
                    set out to out & wid & "\t" & i & "\t" & tname & "\t" & turl & linefeed
                    set i to i + 1
                end repeat
            end repeat
            return out
        end tell
    "#;
    parse_tab_list(&run_osascript(script), "chrome")
}

fn run_osascript(src: &str) -> String {
    match Command::new("/usr/bin/osascript").args(["-e", src]).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

/// Parse `\t`-separated `<window_id>\t<tab_index>\t<title>\t<url>` lines
/// emitted by our AppleScripts. Tolerates empty output (no windows) and
/// lines with missing fields (treated as skipped)
pub fn parse_tab_list(text: &str, browser: &'static str) -> Vec<Tab> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let (wid, idx, title) = (parts[0], parts[1], parts[2]);
        let url = parts.get(3).copied().unwrap_or("").to_string();
        if wid.is_empty() || idx.is_empty() || title.trim().is_empty() {
            continue;
        }
        out.push(Tab {
            browser,
            window_id: wid.to_string(),
            tab_index: idx.to_string(),
            title: title.to_string(),
            url,
        });
    }
    out
}

fn switch_script(browser: &str, window_id: &str, tab_index: &str) -> String {
    let app = match browser {
        "safari" => "Safari",
        "chrome" => "Google Chrome",
        _ => return String::new(),
    };
    let tab_prop = if browser == "safari" { "current tab" } else { "active tab index" };
    if browser == "safari" {
        format!(
            r#"tell application "{app}"
                activate
                set targetWin to window id {window_id}
                set current tab of targetWin to tab {tab_index} of targetWin
                set index of targetWin to 1
            end tell"#
        )
    } else {
        format!(
            r#"tell application "{app}"
                activate
                set targetWin to window id {window_id}
                set {tab_prop} of targetWin to {tab_index}
                set index of targetWin to 1
            end tell"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(browser: &'static str, wid: &str, idx: &str, title: &str, url: &str) -> Tab {
        Tab {
            browser,
            window_id: wid.into(),
            tab_index: idx.into(),
            title: title.into(),
            url: url.into(),
        }
    }

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = BrowserTabsProvider;
        assert!(p.query(&Query::new("something")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_parses_compound_id() {
        let p = BrowserTabsProvider;
        let eff = p.activate(&"tab::safari::123::2".to_string(), "default").await.unwrap();
        match eff {
            Effect::RunAppleScript(s) => {
                assert!(s.contains(r#"application "Safari""#));
                assert!(s.contains("window id 123"));
                assert!(s.contains("tab 2"));
            }
            other => panic!("expected RunAppleScript, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_chrome_emits_chrome_script() {
        let p = BrowserTabsProvider;
        let eff = p.activate(&"tab::chrome::42::3".to_string(), "default").await.unwrap();
        match eff {
            Effect::RunAppleScript(s) => {
                assert!(s.contains("Google Chrome"));
                assert!(s.contains("window id 42"));
                assert!(s.contains("active tab index"));
            }
            other => panic!("expected RunAppleScript, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = BrowserTabsProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_malformed_id_errors() {
        let p = BrowserTabsProvider;
        assert!(p.activate(&"tab::oops".to_string(), "default").await.is_err());
    }


    #[test]
    fn parse_empty() {
        assert!(parse_tab_list("", "safari").is_empty());
    }

    #[test]
    fn parse_single_tab() {
        let text = "123\t1\tExample\thttps://example.com\n";
        let tabs = parse_tab_list(text, "safari");
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].window_id, "123");
        assert_eq!(tabs[0].tab_index, "1");
        assert_eq!(tabs[0].title, "Example");
        assert_eq!(tabs[0].url, "https://example.com");
    }

    #[test]
    fn parse_multiple_tabs() {
        let text = "\
1\t1\tA\thttps://a.com
1\t2\tB\thttps://b.com
2\t1\tC\thttps://c.com
";
        let tabs = parse_tab_list(text, "chrome");
        assert_eq!(tabs.len(), 3);
    }

    #[test]
    fn parse_skips_missing_url() {
        // URL column may be missing but the line is still usable
        let text = "1\t1\tExample\t\n";
        let tabs = parse_tab_list(text, "safari");
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].url, "");
    }

    #[test]
    fn parse_skips_blank_title() {
        let text = "1\t1\t\thttps://example.com\n";
        assert!(parse_tab_list(text, "safari").is_empty());
    }

    #[test]
    fn parse_skips_malformed_lines() {
        let text = "short\nno\ttabs\n";
        assert!(parse_tab_list(text, "safari").is_empty());
    }


    #[test]
    fn tab_matches_by_title() {
        let t = tab("safari", "1", "1", "Hacker News", "https://news.ycombinator.com");
        assert!(tab_matches(&t, "hacker"));
        assert!(!tab_matches(&t, "twitter"));
    }

    #[test]
    fn tab_matches_by_url() {
        let t = tab("chrome", "1", "1", "News", "https://news.ycombinator.com");
        assert!(tab_matches(&t, "ycombinator"));
    }
}
