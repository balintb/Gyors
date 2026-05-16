use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub raw: String,
}

impl Query {
    pub fn new(raw: impl Into<String>) -> Self {
        Self { raw: raw.into() }
    }

    pub fn is_empty(&self) -> bool {
        self.raw.trim().is_empty()
    }

    pub fn pattern(&self) -> &str {
        self.raw.trim()
    }
}

/// What set of providers a query should activate
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryMode {
    /// Fast path: apps + calculator + system commands. Default
    Default,
    /// Slow path: everything, including the mdfind-backed file search
    IncludeFiles,
    /// Clipboard history only. Triggered by the `clip` / `paste` / `cb` keyword
    Clipboard,
    /// Arbitrary shell command. Triggered by the `>` prefix
    Shell,
    /// Notes only - `note` / `notes` / `n` / `newnote` / `nn` /
    /// `findnote` / `searchnotes` / `searchnote` / `#<title>`. Routes
    /// exclusively to the notes provider so unrelated fuzzy matches
    /// (e.g. System Prefs on "notes all") can't fill dropdown
    Notes,
}

/// Parse the raw user input into an effective pattern plus a mode hint
///
/// Prefix tokens:
/// - `'`                     -> `IncludeFiles`
/// - `>`                     -> `Shell` (everything after is the command)
/// - `clip`, `paste`, `cb`   -> `Clipboard` (as keyword OR keyword-space-pattern)
///
/// A bare prefix with nothing after returns an empty pattern (no filter -
/// providers in that mode should show all items they have)
pub fn parse_mode(raw: &str) -> (&str, QueryMode) {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('\'') {
        return (rest.trim_start(), QueryMode::IncludeFiles);
    }
    if let Some(rest) = trimmed.strip_prefix('>') {
        return (rest.trim_start(), QueryMode::Shell);
    }
    // `#<title>` goes to notes (quick-create shorthand)
    if trimmed.starts_with('#') {
        return (trimmed, QueryMode::Notes);
    }
    for kw in CLIPBOARD_KEYWORDS {
        if let Some(rest) = strip_keyword(trimmed, kw) {
            return (rest, QueryMode::Clipboard);
        }
    }
    // Note keywords - pass FULL pattern through (not just the
    // tail) because the notes provider does its own sub-parsing on
    // keyword + args
    for kw in NOTE_KEYWORDS {
        if trimmed == *kw || starts_with_keyword(trimmed, kw) {
            return (trimmed, QueryMode::Notes);
        }
    }
    (trimmed, QueryMode::Default)
}

const CLIPBOARD_KEYWORDS: &[&str] = &["clip", "paste", "cb", "c"];
/// Keywords that the notes provider owns end-to-end. Kept in sync
/// with `parse_note_input` - when these trigger, we want a strict
/// notes-only dropdown (no System Prefs / apps / hints noise)
const NOTE_KEYWORDS: &[&str] = &[
    "note", "notes", "n",
    "newnote", "nn",
    "findnote", "searchnotes", "searchnote",
];

fn starts_with_keyword(s: &str, kw: &str) -> bool {
    let Some(rest) = s.strip_prefix(kw) else { return false };
    rest.starts_with(char::is_whitespace)
}

/// Returns the remainder after a leading keyword. Accepts `kw` exactly OR
/// `kw` followed by whitespace + filter. Returns None if `kw` is a prefix of
/// a longer word like `clipboard`
fn strip_keyword<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    if s == kw {
        return Some("");
    }
    let rest = s.strip_prefix(kw)?;
    let next = rest.chars().next()?;
    if next.is_whitespace() {
        Some(rest.trim_start())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_holds_raw() {
        assert_eq!(Query::new("hello").raw, "hello");
    }

    #[test]
    fn empty_is_empty() {
        assert!(Query::new("").is_empty());
        assert!(Query::new("   ").is_empty());
        assert!(Query::new("\t\n").is_empty());
    }

    #[test]
    fn non_empty_is_not_empty() {
        assert!(!Query::new("x").is_empty());
        assert!(!Query::new("  x  ").is_empty());
    }

    #[test]
    fn pattern_trims() {
        assert_eq!(Query::new("  hello  ").pattern(), "hello");
        assert_eq!(Query::new("\thello world\n").pattern(), "hello world");
    }

    #[test]
    fn default_is_empty() {
        assert!(Query::default().is_empty());
    }

    #[test]
    fn parse_mode_default_for_plain_input() {
        assert_eq!(parse_mode("safari"), ("safari", QueryMode::Default));
    }

    #[test]
    fn parse_mode_detects_leading_apostrophe() {
        assert_eq!(parse_mode("'safari"), ("safari", QueryMode::IncludeFiles));
    }

    #[test]
    fn parse_mode_trims_whitespace_after_prefix() {
        assert_eq!(parse_mode("'  safari  "), ("safari", QueryMode::IncludeFiles));
    }

    #[test]
    fn parse_mode_trims_input_before_prefix_check() {
        assert_eq!(parse_mode("  'safari"), ("safari", QueryMode::IncludeFiles));
    }

    #[test]
    fn parse_mode_mid_query_apostrophe_is_plain() {
        // Apostrophes elsewhere are part of query, not a mode signal
        assert_eq!(parse_mode("safari's"), ("safari's", QueryMode::Default));
    }

    #[test]
    fn parse_mode_bare_prefix_yields_empty() {
        assert_eq!(parse_mode("'"), ("", QueryMode::IncludeFiles));
    }

    #[test]
    fn parse_mode_empty_input() {
        assert_eq!(parse_mode(""), ("", QueryMode::Default));
        assert_eq!(parse_mode("   "), ("", QueryMode::Default));
    }

    #[test]
    fn parse_mode_bare_clip_keyword() {
        assert_eq!(parse_mode("clip"), ("", QueryMode::Clipboard));
        assert_eq!(parse_mode("paste"), ("", QueryMode::Clipboard));
        assert_eq!(parse_mode("cb"), ("", QueryMode::Clipboard));
    }

    #[test]
    fn parse_mode_clip_with_filter() {
        assert_eq!(parse_mode("clip hello"), ("hello", QueryMode::Clipboard));
        assert_eq!(parse_mode("paste   world"), ("world", QueryMode::Clipboard));
        assert_eq!(parse_mode("cb foo bar"), ("foo bar", QueryMode::Clipboard));
    }

    #[test]
    fn parse_mode_clip_prefix_of_longer_word_is_plain() {
        // "clipboard" should NOT be treated as the `clip` keyword
        assert_eq!(parse_mode("clipboard"), ("clipboard", QueryMode::Default));
        assert_eq!(parse_mode("clippers"), ("clippers", QueryMode::Default));
        // "cbiz" not a keyword (no whitespace after cb)
        assert_eq!(parse_mode("cbiz"), ("cbiz", QueryMode::Default));
        // `c` is a clipboard alias only when followed by whitespace.
        // Bare letter `c` IS a valid keyword (mode = Clipboard,
        // empty filter, returns all). Anything else starting with
        // c (`config`, `cron`, `case`, ...) must stay default
        assert_eq!(parse_mode("config"), ("config", QueryMode::Default));
        assert_eq!(parse_mode("cron"), ("cron", QueryMode::Default));
        assert_eq!(parse_mode("calc"), ("calc", QueryMode::Default));
    }

    #[test]
    fn parse_mode_c_alias_triggers_clipboard() {
        // `c` (with optional space + filter) is a short-form
        // clipboard alias for users who dont want to type the
        // full `clip` every time. Added because clipboard history
        // is no longer fuzzy-matched in default-mode queries -
        // users who actually WANT clipboard search need a quick
        // way to invoke it
        assert_eq!(parse_mode("c"), ("", QueryMode::Clipboard));
        assert_eq!(parse_mode("c hello"), ("hello", QueryMode::Clipboard));
        assert_eq!(parse_mode("c   foo bar"), ("foo bar", QueryMode::Clipboard));
    }

    #[test]
    fn parse_mode_clip_trims_leading_whitespace_in_raw() {
        assert_eq!(parse_mode("  clip foo"), ("foo", QueryMode::Clipboard));
    }


    #[test]
    fn parse_mode_shell_prefix() {
        assert_eq!(parse_mode(">ls -la"), ("ls -la", QueryMode::Shell));
    }

    #[test]
    fn parse_mode_shell_with_leading_space() {
        assert_eq!(parse_mode("> ls -la"), ("ls -la", QueryMode::Shell));
    }

    #[test]
    fn parse_mode_bare_shell_prefix() {
        assert_eq!(parse_mode(">"), ("", QueryMode::Shell));
    }

    #[test]
    fn parse_mode_shell_trims_outer_whitespace() {
        assert_eq!(parse_mode("  >ls"), ("ls", QueryMode::Shell));
    }


    #[test]
    fn parse_mode_all_note_keywords_trigger_notes_mode() {
        // Pins the contract: every keyword the notes provider owns
        // routes exclusively to it, so unrelated fuzzy matches
        // (System Prefs on "notes all", apps on "n meet") can't
        // surface above a zero-notes folder or a gated-off provider
        for kw in ["note", "notes", "n", "newnote", "nn",
                   "findnote", "searchnotes", "searchnote"]
        {
            let (_, mode) = parse_mode(kw);
            assert_eq!(mode, QueryMode::Notes, "bare `{kw}` → Notes mode");
            let (_, mode) = parse_mode(&format!("{kw} foo"));
            assert_eq!(mode, QueryMode::Notes, "`{kw} foo` → Notes mode");
        }
    }

    #[test]
    fn parse_mode_hash_prefix_is_notes() {
        // `#<title>` is the quick-create shorthand - must route to
        // notes, not fall through to default fuzzy
        assert_eq!(parse_mode("#hello"), ("#hello", QueryMode::Notes));
        assert_eq!(parse_mode("#  with space"), ("#  with space", QueryMode::Notes));
        assert_eq!(parse_mode("#"), ("#", QueryMode::Notes));
    }

    #[test]
    fn parse_mode_note_keyword_prefix_of_longer_word_is_plain() {
        // `nothing` must NOT become notes-mode - `n` needs whitespace
        // after (same pattern as `clip`/`clipboard`)
        assert_eq!(parse_mode("nothing"), ("nothing", QueryMode::Default));
        assert_eq!(parse_mode("noteworthy"), ("noteworthy", QueryMode::Default));
        assert_eq!(parse_mode("newnoting"), ("newnoting", QueryMode::Default));
        assert_eq!(parse_mode("findnotebook"), ("findnotebook", QueryMode::Default));
    }

    #[test]
    fn parse_mode_note_mode_passes_full_pattern_through() {
        // Notes provider does its own sub-parsing on the full keyword
        // + args, so parse_mode must NOT strip keyword
        let (pat, mode) = parse_mode("notes all meeting");
        assert_eq!(pat, "notes all meeting");
        assert_eq!(mode, QueryMode::Notes);
        let (pat, mode) = parse_mode("findnote schema");
        assert_eq!(pat, "findnote schema");
        assert_eq!(mode, QueryMode::Notes);
    }

    #[test]
    fn parse_mode_mid_query_angle_bracket_is_default() {
        // `>` must be the first non-whitespace char to be a prefix
        assert_eq!(parse_mode("foo>bar"), ("foo>bar", QueryMode::Default));
    }
}
