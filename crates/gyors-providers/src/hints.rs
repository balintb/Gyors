//! Command hints - discoverability for keyword-triggered providers
//!
//! While user types a single token, matching keyword hints surface
//! alongside other results. Activating a hint fires `Effect::SetInput` to
//! prefill keyword + space and keep panel open
//!
//! Two modes:
//! - Prefix match - `md` -> `md5 <text>`. The primary behaviour.
//! - Typo correction - if nothing starts with input but something
//!   is within edit distance 2, show as "Did you mean ...?"
//!
//! A hint is suppressed when input exactly matches a self-contained
//! (no-required-params) keyword, because its provider is already showing
//! the real output - the hint would be redundant

use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct CommandHintsProvider;

#[derive(Debug, Clone, Copy)]
struct Hint {
    keyword: &'static str,
    syntax: &'static str,
    description: &'static str,
    symbol: &'static str,
}

const HINTS: &[Hint] = &[
    // Web search
    Hint { keyword: "g",       syntax: "g <query>",       description: "Search Google",           symbol: "magnifyingglass" },
    Hint { keyword: "ddg",     syntax: "ddg <query>",     description: "Search DuckDuckGo",       symbol: "magnifyingglass.circle" },
    Hint { keyword: "gh",      syntax: "gh <query>",      description: "Search GitHub",           symbol: "chevron.left.forwardslash.chevron.right" },
    Hint { keyword: "so",      syntax: "so <query>",      description: "Search Stack Overflow",   symbol: "questionmark.circle" },
    Hint { keyword: "yt",      syntax: "yt <query>",      description: "Search YouTube",          symbol: "play.rectangle.fill" },
    Hint { keyword: "npm",     syntax: "npm <query>",     description: "Search npm",              symbol: "shippingbox.fill" },
    Hint { keyword: "w",       syntax: "w <query>",       description: "Search Wikipedia",        symbol: "book.fill" },
    Hint { keyword: "docs",    syntax: "docs <query>",    description: "Search docs.rs",          symbol: "shield.fill" },
    // Encoding
    Hint { keyword: "b64",     syntax: "b64 <text>",      description: "Base64 encode",           symbol: "key.fill" },
    Hint { keyword: "b64d",    syntax: "b64d <text>",     description: "Base64 decode",           symbol: "key.fill" },
    Hint { keyword: "url",     syntax: "url <text>",      description: "URL encode",              symbol: "key.fill" },
    Hint { keyword: "urld",    syntax: "urld <text>",     description: "URL decode",              symbol: "key.fill" },
    Hint { keyword: "htmlescape",   syntax: "htmlescape <text>",   description: "Escape HTML entities (& < > \" ')", symbol: "chevron.left.slash.chevron.right" },
    Hint { keyword: "htmlunescape", syntax: "htmlunescape <text>", description: "Decode HTML entities back to characters", symbol: "chevron.left.slash.chevron.right" },
    Hint { keyword: "jsonescape",   syntax: "jsonescape <text>",   description: "Escape text for JSON string literal", symbol: "curlybraces" },
    Hint { keyword: "jsonunescape", syntax: "jsonunescape <text>", description: "Decode JSON-string escapes",       symbol: "curlybraces" },
    Hint { keyword: "rot13",        syntax: "rot13 <text>",        description: "ROT13 cipher (self-inverse)",     symbol: "key.fill" },
    Hint { keyword: "caesar",       syntax: "caesar <n> <text>",   description: "Caesar cipher with shift n",       symbol: "key.fill" },
    Hint { keyword: "hmac",         syntax: "hmac <algo> <key> <text>", description: "HMAC-SHA1/256/512 of <text> using <key>", symbol: "key.fill" },
    Hint { keyword: "md5",     syntax: "md5 <text>",      description: "MD5 hex digest",          symbol: "key.fill" },
    Hint { keyword: "sha1",    syntax: "sha1 <text>",     description: "SHA-1 hex digest",        symbol: "key.fill" },
    Hint { keyword: "sha256",  syntax: "sha256 <text>",   description: "SHA-256 hex digest",      symbol: "key.fill" },
    Hint { keyword: "sha3",    syntax: "sha3 <text>",     description: "SHA3-256 hex digest",     symbol: "key.fill" },
    Hint { keyword: "sha3-512", syntax: "sha3-512 <text>", description: "SHA3-512 hex digest",    symbol: "key.fill" },
    Hint { keyword: "blake3",  syntax: "blake3 <text>",   description: "BLAKE3 hex digest",       symbol: "key.fill" },
    Hint { keyword: "jwt",     syntax: "jwt <token>",    description: "Decode JWT header & payload", symbol: "key.fill" },
    Hint { keyword: "newjwt",  syntax: "newjwt sub=alice exp=1h iss=foo", description: "Sign a new HS256 JWT from claims", symbol: "key.fill" },
    // Kill
    Hint { keyword: "kill",    syntax: "kill <name>",     description: "Find & terminate process", symbol: "xmark.octagon.fill" },
    // Clipboard
    Hint { keyword: "clip",    syntax: "clip [filter]",   description: "Clipboard history",       symbol: "doc.on.clipboard" },
    Hint { keyword: "paste",   syntax: "paste [filter]",  description: "Clipboard history",       symbol: "doc.on.clipboard" },
    Hint { keyword: "cb",      syntax: "cb [filter]",     description: "Clipboard history",       symbol: "doc.on.clipboard" },
    // Emoji
    Hint { keyword: "emoji",   syntax: "emoji <search>",  description: "Find & copy an emoji",    symbol: "face.smiling" },
    // Color
    Hint { keyword: "color",   syntax: "color <value>",   description: "Hex / RGB / HSL converter", symbol: "paintpalette.fill" },
    Hint { keyword: "rgb",     syntax: "rgb <r> <g> <b>", description: "RGB → other formats",     symbol: "paintpalette.fill" },
    Hint { keyword: "hsl",     syntax: "hsl <h> <s> <l>", description: "HSL → other formats",     symbol: "paintpalette.fill" },
    // Time
    Hint { keyword: "now",     syntax: "now",             description: "Current date/time + unix timestamp", symbol: "clock.fill" },
    Hint { keyword: "ts",      syntax: "ts <unix>",       description: "Unix timestamp → human date", symbol: "clock.fill" },
    Hint { keyword: "date",    syntax: "date +3d",        description: "Date arithmetic (s/m/h/d/w)", symbol: "calendar" },
    // Generators
    Hint { keyword: "uuid",    syntax: "uuid",            description: "New random UUIDv4",       symbol: "barcode.viewfinder" },
    Hint { keyword: "uuid7",   syntax: "uuid7",           description: "New time-ordered UUIDv7", symbol: "barcode.viewfinder" },
    Hint { keyword: "passw",   syntax: "passw [len]",     description: "Random password (20 default)", symbol: "key.horizontal.fill" },
    // Case converter
    Hint { keyword: "case",    syntax: "case <kind>? <text>", description: "Convert text case", symbol: "textformat" },
    // Git repos
    Hint { keyword: "repo",    syntax: "repo [name]",  description: "Git repo - open, search on GitHub, inspect", symbol: "chevron.left.forwardslash.chevron.right" },
    Hint { keyword: "repos",   syntax: "repos [name]", description: "Git repo - open, search on GitHub, inspect", symbol: "chevron.left.forwardslash.chevron.right" },
    // Dictionary
    Hint { keyword: "def",     syntax: "def <word>",   description: "Look up in macOS Dictionary", symbol: "book.fill" },
    // QR code
    Hint { keyword: "qr",      syntax: "qr <text>",    description: "QR code → clipboard image", symbol: "qrcode" },
    // AI
    Hint { keyword: "ai",      syntax: "ai <question>", description: "Ask your configured AI provider", symbol: "sparkles" },
    Hint { keyword: "ask",     syntax: "ask <question>", description: "Ask your configured AI provider", symbol: "sparkles" },
    Hint { keyword: "summarize", syntax: "summarize [text]", description: "Summarize clipboard or inline text",  symbol: "text.redaction" },
    Hint { keyword: "tldr",    syntax: "tldr [text]",    description: "One-sentence TL;DR of clipboard/text",     symbol: "text.redaction" },
    Hint { keyword: "explain", syntax: "explain [text]", description: "Explain clipboard/text in plain English",  symbol: "questionmark.bubble" },
    Hint { keyword: "rewrite", syntax: "rewrite [text]", description: "Clearer, more concise rewrite",            symbol: "pencil.and.outline" },
    Hint { keyword: "fix",     syntax: "fix [text]",     description: "Fix grammar/spelling via AI",              symbol: "checkmark.seal" },
    Hint { keyword: "shorten", syntax: "shorten [text]", description: "Shorten text to roughly half",             symbol: "arrow.down.right.and.arrow.up.left" },
    Hint { keyword: "expand",  syntax: "expand [text]",  description: "Expand text with detail / examples",       symbol: "arrow.up.left.and.arrow.down.right" },
    Hint { keyword: "translate", syntax: "translate <lang> [text]", description: "Translate clipboard/text",       symbol: "character.bubble" },
    // Notes
    Hint { keyword: "note",    syntax: "note [filter]",  description: "Find & open a markdown note (top 20)", symbol: "doc.text.fill" },
    Hint { keyword: "notes",   syntax: "notes [filter]", description: "Find & open a markdown note (top 20)", symbol: "doc.text.fill" },
    Hint { keyword: "n",       syntax: "n [filter]",     description: "Find & open a markdown note (alias)", symbol: "doc.text.fill" },
    Hint { keyword: "findnote", syntax: "findnote <query>", description: "Search inside note bodies (content-first)", symbol: "text.magnifyingglass" },
    Hint { keyword: "searchnotes", syntax: "searchnotes <query>", description: "Same as findnote", symbol: "text.magnifyingglass" },
    Hint { keyword: "notes all", syntax: "notes all [filter]", description: "List ALL notes (no cap)", symbol: "doc.text.fill" },
    Hint { keyword: "newnote", syntax: "newnote <title>", description: "Create a new markdown note", symbol: "plus.square.fill" },
    Hint { keyword: "nn",      syntax: "nn <title>",     description: "Create a new markdown note (alias)", symbol: "plus.square.fill" },
    // New dev utilities
    Hint { keyword: "json",    syntax: "json <input>",   description: "Validate & pretty-print JSON",    symbol: "curlybraces" },
    Hint { keyword: "jq",      syntax: "jq <input>",     description: "Validate & pretty-print JSON (alias)", symbol: "curlybraces" },
    Hint { keyword: "re",      syntax: "re <pattern> [:: <text>]", description: "Test a regex",          symbol: "text.magnifyingglass" },
    Hint { keyword: "regex",   syntax: "regex <pattern>", description: "Test a regex",                   symbol: "text.magnifyingglass" },
    Hint { keyword: "ssh",     syntax: "ssh [filter]",   description: "Connect to an SSH host",          symbol: "terminal.fill" },
    Hint { keyword: "tz",      syntax: "tz <city>",      description: "Show the local time in a city",   symbol: "globe.europe.africa.fill" },
    Hint { keyword: "tab",     syntax: "tab <query>",    description: "Search open browser tabs",        symbol: "safari.fill" },
    Hint { keyword: "recent",  syntax: "recent [filter]", description: "Recently used files",            symbol: "clock.arrow.circlepath" },
    Hint { keyword: "rec",     syntax: "rec [filter]",   description: "Recently used files (alias)",     symbol: "clock.arrow.circlepath" },
    // Format converters
    Hint { keyword: "json2yaml", syntax: "json2yaml <input>", description: "Convert JSON → YAML",        symbol: "arrow.left.arrow.right.square" },
    Hint { keyword: "yaml2json", syntax: "yaml2json <input>", description: "Convert YAML → JSON",        symbol: "arrow.left.arrow.right.square" },
    Hint { keyword: "json2toml", syntax: "json2toml <input>", description: "Convert JSON → TOML",        symbol: "arrow.left.arrow.right.square" },
    Hint { keyword: "toml2json", syntax: "toml2json <input>", description: "Convert TOML → JSON",        symbol: "arrow.left.arrow.right.square" },
    Hint { keyword: "yaml2toml", syntax: "yaml2toml <input>", description: "Convert YAML → TOML",        symbol: "arrow.left.arrow.right.square" },
    Hint { keyword: "toml2yaml", syntax: "toml2yaml <input>", description: "Convert TOML → YAML",        symbol: "arrow.left.arrow.right.square" },
    // Timers
    Hint { keyword: "timer",   syntax: "timer <duration> [label]", description: "Start a countdown timer", symbol: "timer" },
    Hint { keyword: "timers",  syntax: "timers [filter]",          description: "List or stop timers",    symbol: "timer" },
    // Config
    Hint { keyword: "config",  syntax: "config [filter]",          description: "Browse & edit Gyors settings",  symbol: "gearshape.2.fill" },
    // Snippets
    Hint { keyword: "snip",    syntax: "snip [filter]",  description: "Find & copy a snippet",       symbol: "text.quote" },
    Hint { keyword: "snippet", syntax: "snippet [filter]", description: "Find & copy a snippet",     symbol: "text.quote" },
    // Scratchpad
    Hint { keyword: "scratch", syntax: "scratch [text]", description: "Open persistent scratchpad",  symbol: "scribble.variable" },
    Hint { keyword: "scratchpad", syntax: "scratchpad [text]", description: "Open persistent scratchpad", symbol: "scribble.variable" },
    // Cron
    Hint { keyword: "cron",    syntax: "cron <schedule>", description: "Cron expression from English (e.g. \"every day at 6pm\")", symbol: "clock.arrow.circlepath" },
    // Shortcuts
    Hint { keyword: "shortcut", syntax: "shortcut [filter]", description: "Run a macOS Shortcut",    symbol: "bolt.horizontal.fill" },
    Hint { keyword: "sc",      syntax: "sc [filter]",    description: "Run a macOS Shortcut",        symbol: "bolt.horizontal.fill" },
    // Text transforms
    Hint { keyword: "upper",   syntax: "upper <text>",   description: "Uppercase text",              symbol: "characters.uppercase" },
    Hint { keyword: "lower",   syntax: "lower <text>",   description: "Lowercase text",              symbol: "characters.lowercase" },
    Hint { keyword: "rev",     syntax: "rev <text>",     description: "Reverse text",                symbol: "arrow.left.arrow.right" },
    Hint { keyword: "count",   syntax: "count <text>",   description: "Word / char / line count",    symbol: "number" },
    Hint { keyword: "sort",    syntax: "sort <lines>",   description: "Sort lines alphabetically",   symbol: "arrow.up.arrow.down" },
    Hint { keyword: "sortr",   syntax: "sortr <lines>",  description: "Sort lines in reverse",       symbol: "arrow.up.arrow.down" },
    Hint { keyword: "dedup",   syntax: "dedup <lines>",  description: "Remove duplicate lines",      symbol: "rectangle.on.rectangle.slash" },
    Hint { keyword: "uniq",    syntax: "uniq <lines>",   description: "Remove duplicate lines",      symbol: "rectangle.on.rectangle.slash" },
    Hint { keyword: "trim",    syntax: "trim <text>",    description: "Trim leading/trailing whitespace",     symbol: "scissors" },
    Hint { keyword: "nows",    syntax: "nows <text>",    description: "Remove all whitespace",                symbol: "scissors" },
    Hint { keyword: "stripws", syntax: "stripws <text>", description: "Remove all whitespace (alias)",       symbol: "scissors" },
    Hint { keyword: "normalize", syntax: "normalize <text>", description: "Collapse runs of whitespace to single space", symbol: "scissors" },
    Hint { keyword: "repeat",  syntax: "repeat <n> <text>", description: "Repeat text n times",              symbol: "repeat" },
    Hint { keyword: "lpad",    syntax: "lpad <n> <text>",   description: "Left-pad to width n",              symbol: "arrow.right.to.line" },
    Hint { keyword: "rpad",    syntax: "rpad <n> <text>",   description: "Right-pad to width n",             symbol: "arrow.left.to.line" },
    // Reference lookups
    Hint { keyword: "http",    syntax: "http [code|name]", description: "HTTP status code reference (e.g. 404, teapot)", symbol: "exclamationmark.triangle.fill" },
    Hint { keyword: "port",    syntax: "port [num|name]",  description: "Well-known TCP/UDP ports (e.g. 22, ssh)",       symbol: "network" },
    Hint { keyword: "mime",    syntax: "mime [ext|type]",  description: "MIME type reference (e.g. pdf, image/)",        symbol: "doc.fill" },
    Hint { keyword: "mimetype",syntax: "mimetype <ext>",   description: "MIME type reference (alias)",                   symbol: "doc.fill" },
    Hint { keyword: "dns",     syntax: "dns [type]",       description: "DNS record type reference (A, MX, CNAME, …)",   symbol: "network" },
    Hint { keyword: "roman",   syntax: "roman <num|MMXXIV>", description: "Roman ↔ decimal numeral converter",            symbol: "character.book.closed" },
    Hint { keyword: "numformat", syntax: "numformat <num> [precision]", description: "Format number with thousands separators", symbol: "number.circle" },
    Hint { keyword: "nf",      syntax: "nf <num>",       description: "Format number with thousands separators (alias)", symbol: "number.circle" },
    Hint { keyword: "morse",   syntax: "morse <text>",   description: "Encode text as Morse code",            symbol: "dot.radiowaves.left.and.right" },
    Hint { keyword: "morsedec",syntax: "morsedec <morse>", description: "Decode Morse code back to text",     symbol: "dot.radiowaves.left.and.right" },
    Hint { keyword: "cp",      syntax: "cp <char|U+...>",  description: "Inspect Unicode codepoint info",      symbol: "character.cursor.ibeam" },
    Hint { keyword: "codepoint", syntax: "codepoint <char>", description: "Inspect Unicode codepoint info (alias)", symbol: "character.cursor.ibeam" },
    Hint { keyword: "datediff", syntax: "datediff <a> :: <b>", description: "Days between two YYYY-MM-DD dates",  symbol: "calendar" },
    Hint { keyword: "csv2json", syntax: "csv2json <csv>",      description: "Convert CSV (with header) → JSON",   symbol: "arrow.left.arrow.right.square" },
    Hint { keyword: "json2csv", syntax: "json2csv <json>",     description: "Convert JSON array of objects → CSV", symbol: "arrow.left.arrow.right.square" },
    // Lorem
    Hint { keyword: "lorem",   syntax: "lorem [n]",      description: "Lorem ipsum (n paragraphs)",  symbol: "text.alignleft" },
];

/// Keywords whose provider produces output without any user-supplied
/// arguments. When user types one of these exactly, matching hint
/// is redundant with provider's output row and should be hidden
const SELF_CONTAINED: &[&str] = &[
    "now", "ts", "date", "uuid", "uuid4", "uuid7", "uuidv7", "passw", "password", "lorem",
    "scratch", "scratchpad", "http", "port", "mime", "mimetype", "dns",
];

/// Minimum query length before the typo-correct fallback kicks in
const TYPO_MIN_LEN: usize = 3;
/// Maximum Levenshtein distance for a typo-correct suggestion
const TYPO_MAX_DISTANCE: usize = 2;
const TYPO_MAX_RESULTS: usize = 3;

#[async_trait]
impl Provider for CommandHintsProvider {
    fn id(&self) -> &str {
        "hint"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let input = query.pattern();
        if input.is_empty() || input.contains(char::is_whitespace) {
            return vec![];
        }
        // The `#<title>` note shorthand is itself a command - dont dilute
        // results with unrelated "did you mean `###`?" suggestions
        if input.starts_with('#') {
            return vec![];
        }
        let lower = input.to_lowercase();

        // Primary: prefix matches
        let prefix: Vec<&Hint> = HINTS
            .iter()
            .filter(|h| h.keyword.starts_with(&lower))
            .filter(|h| !is_redundant_exact_match(h, &lower))
            .collect();
        if !prefix.is_empty() {
            return prefix.into_iter().take(10).map(regular_candidate).collect();
        }

        // Fallback: typo-correct suggestions. Never applied when the
        // user typed a real, self-contained keyword - the relevant
        // provider is already going to surface its own output, and
        // "did you mean `n`?" above actual `now` timestamps just
        // pushes answer off the top of the list
        if lower.chars().count() < TYPO_MIN_LEN {
            return vec![];
        }
        if is_note_command(&lower) || is_self_contained(&lower) || is_known_keyword(&lower) {
            return vec![];
        }
        let mut scored: Vec<(&Hint, usize)> = HINTS
            .iter()
            .map(|h| (h, strsim::levenshtein(&lower, h.keyword)))
            .filter(|(_, d)| *d > 0 && *d <= TYPO_MAX_DISTANCE)
            .collect();
        scored.sort_by_key(|(h, d)| (*d, h.keyword));
        scored
            .into_iter()
            .take(TYPO_MAX_RESULTS)
            .map(|(h, _)| typo_candidate(h))
            .collect()
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let keyword = id
            .strip_prefix("hint::")
            .ok_or_else(|| anyhow::anyhow!("invalid hint candidate id: {id}"))?;
        Ok(Effect::SetInput(format!("{keyword} ")))
    }
}

fn is_redundant_exact_match(h: &Hint, input_lower: &str) -> bool {
    SELF_CONTAINED.contains(&h.keyword) && h.keyword == input_lower
}

/// Short note-related tokens user types deliberately (`n`, `nn`,
/// `note`, `notes`, `newnote`). `n` is within edit-distance 2 of a dozen
/// other hints, so without this guard user gets "Did you mean md5?"
/// every time they start typing a note command
fn is_note_command(input_lower: &str) -> bool {
    matches!(input_lower, "n" | "nn" | "note" | "notes" | "newnote")
}

/// True when query is one of the self-contained keywords whose
/// provider produces intrinsic output (calc, `now`, `uuid`, ...). Used to
/// suppress "Did you mean?" suggestions that would otherwise rank
/// above provider's actual result
fn is_self_contained(input_lower: &str) -> bool {
    SELF_CONTAINED.contains(&input_lower)
}

/// True when query exactly matches any known hint keyword. If the
/// user typed a real keyword, suggestions of similar keywords are
/// distracting - they clearly know the term
fn is_known_keyword(input_lower: &str) -> bool {
    HINTS.iter().any(|h| h.keyword == input_lower)
}

fn regular_candidate(h: &Hint) -> Candidate {
    Candidate {
        id: format!("hint::{}", h.keyword),
        title: h.syntax.to_string(),
        subtitle: Some(h.description.to_string()),
        icon: Icon::SfSymbol(h.symbol.into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Continue")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn typo_candidate(h: &Hint) -> Candidate {
    Candidate {
        id: format!("hint::{}", h.keyword),
        title: h.syntax.to_string(),
        subtitle: Some(format!("Did you mean? · {}", h.description)),
        icon: Icon::SfSymbol("questionmark.circle".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Continue")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_and_whitespace_query_no_hints() {
        let p = CommandHintsProvider;
        assert!(p.query(&Query::new("")).await.is_empty());
        assert!(p.query(&Query::new("   ")).await.is_empty());
        assert!(p.query(&Query::new("md5 hi")).await.is_empty());
    }

    #[tokio::test]
    async fn prefix_match() {
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("md")).await;
        assert!(out.iter().any(|c| c.id == "hint::md5"));
    }

    #[tokio::test]
    async fn sha_family() {
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("sha")).await;
        let ids: Vec<_> = out.iter().map(|c| c.id.clone()).collect();
        assert!(ids.contains(&"hint::sha1".to_string()));
        assert!(ids.contains(&"hint::sha256".to_string()));
        assert!(ids.contains(&"hint::sha3".to_string()));
    }

    #[tokio::test]
    async fn no_param_exact_match_suppressed() {
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("uuid")).await;
        // Uuid (self-contained) is suppressed, but uuid7 / uuidv7 still start
        // with "uuid" and aren't suppressed by exact-match
        assert!(!out.iter().any(|c| c.id == "hint::uuid"));
        assert!(out.iter().any(|c| c.id == "hint::uuid7"));
    }

    #[tokio::test]
    async fn no_param_prefix_still_shown() {
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("uui")).await;
        // Not an exact match -> all uuid* hints visible
        assert!(out.iter().any(|c| c.id == "hint::uuid"));
    }

    #[tokio::test]
    async fn required_param_exact_match_not_suppressed() {
        // Md5 has a required <text> arg, so its hint is useful even after
        // user has typed the full keyword
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("md5")).await;
        assert!(out.iter().any(|c| c.id == "hint::md5"));
    }

    #[tokio::test]
    async fn typo_suggestion_emoji() {
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("emojii")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "hint::emoji");
        assert!(out[0]
            .subtitle
            .as_deref()
            .unwrap_or("")
            .contains("Did you mean"));
    }

    #[tokio::test]
    async fn typo_suggestion_sha256() {
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("sha26")).await;
        assert!(out.iter().any(|c| c.id == "hint::sha256"));
    }

    #[tokio::test]
    async fn typo_suggestion_icon_is_question_mark() {
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("emojii")).await;
        assert!(matches!(
            out[0].icon,
            Icon::SfSymbol(ref s) if s == "questionmark.circle"
        ));
    }

    #[tokio::test]
    async fn typo_ignored_for_short_input() {
        let p = CommandHintsProvider;
        // Too short - dont trigger Levenshtein noise
        assert!(p.query(&Query::new("xy")).await.is_empty());
    }

    #[tokio::test]
    async fn typo_ignored_when_far_from_any_keyword() {
        let p = CommandHintsProvider;
        assert!(p.query(&Query::new("zzzzzzzzz")).await.is_empty());
    }

    #[tokio::test]
    async fn prefix_beats_typo() {
        // "ca" is a prefix of "cb"/"case", so typo fallback shouldn't fire
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("ca")).await;
        assert!(!out
            .iter()
            .any(|c| c.subtitle.as_deref().unwrap_or("").contains("Did you mean")));
    }

    #[tokio::test]
    async fn hash_prefix_suppresses_hints_entirely() {
        // `#<title>` is the note shorthand - never try to correct it
        let p = CommandHintsProvider;
        assert!(p.query(&Query::new("#")).await.is_empty());
        assert!(p.query(&Query::new("#hello")).await.is_empty());
        assert!(p.query(&Query::new("#abcxyz")).await.is_empty());
    }

    #[tokio::test]
    async fn self_contained_keyword_skips_typo_suggestions() {
        // Bug: typing `now` showed "Did you mean n/nn/note?" above the
        // actual timestamp output. Self-contained keywords are real
        // provider triggers - typo suggestions here are noise
        let p = CommandHintsProvider;
        for token in ["now", "ts", "date", "uuid", "lorem", "passw"] {
            let out = p.query(&Query::new(token)).await;
            assert!(
                out.iter().all(|c| !c.subtitle
                    .as_deref()
                    .unwrap_or("")
                    .contains("Did you mean")),
                "typo correction leaked for self-contained keyword {token}"
            );
        }
    }

    #[tokio::test]
    async fn exact_keyword_match_skips_typo_suggestions() {
        // When input is already a known hint keyword, dont also
        // suggest edit-distance-2 neighbours - user knows what
        // they want
        let p = CommandHintsProvider;
        for token in ["md5", "sha256", "b64", "json", "tz", "timer"] {
            let out = p.query(&Query::new(token)).await;
            assert!(
                out.iter().all(|c| !c.subtitle
                    .as_deref()
                    .unwrap_or("")
                    .contains("Did you mean")),
                "typo correction leaked for exact keyword {token}"
            );
        }
    }

    #[tokio::test]
    async fn note_commands_skip_typo_suggestions() {
        // Short note tokens are intentional - no "did you mean md5?" noise
        let p = CommandHintsProvider;
        // `n` / `nn` / `note` would all prefix-match themselves anyway, so
        // look at a case where typo correction could fire.
        // Confirm typo suggestions aren't fired for these short tokens
        for token in ["n", "nn", "note", "notes", "newnote"] {
            let out = p.query(&Query::new(token)).await;
            assert!(
                out.iter().all(|c| !c.subtitle
                    .as_deref()
                    .unwrap_or("")
                    .contains("Did you mean")),
                "typo correction leaked for {token}"
            );
        }
    }

    #[tokio::test]
    async fn n_as_prefix_hint_lists_note_aliases() {
        // Tab-completion target: typing `n` should surface the `n`/`nn`
        // aliases as prefix hints
        let p = CommandHintsProvider;
        let out = p.query(&Query::new("n")).await;
        assert!(
            out.iter().any(|c| c.id == "hint::n" || c.id == "hint::note"),
            "expected note-family hint for 'n', got {:?}",
            out.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn activate_yields_setinput_with_space() {
        let p = CommandHintsProvider;
        let effect = p.activate(&"hint::sha256".to_string(), "default").await.unwrap();
        match effect {
            Effect::SetInput(s) => assert_eq!(s, "sha256 "),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = CommandHintsProvider;
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn hints_have_unique_keywords() {
        let mut seen = std::collections::HashSet::new();
        for h in HINTS {
            assert!(seen.insert(h.keyword), "duplicate hint keyword: {}", h.keyword);
        }
    }
}
