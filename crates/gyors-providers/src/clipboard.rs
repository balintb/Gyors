//! Clipboard history provider
//!
//! Looks up recently-copied text from the Index's `clipboard_items` table.
//! Activated by the `clip` / `paste` / `cb` keyword - orchestrator routes
//! queries in `QueryMode::Clipboard` here, passing the filter string as the
//! effective pattern. Empty filter returns N most recent items

use async_trait::async_trait;
use base64::prelude::*;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use gyors_index::{ClipboardItem, Index};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Sentinel prefix for image-typed clipboard items. Swift's
/// `PasteboardWatcher` writes the image to disk and records the
/// content as `gyors-image:<absolute-path>`. Provider strips the
/// prefix and renders an image row with thumbnail + copy-image
/// actions. Kept in sync with `PasteboardWatcher.imageSentinelPrefix`
const IMAGE_SENTINEL_PREFIX: &str = "gyors-image:";

/// How many clipboard items provider returns per query. Bumped
/// well above the launcher's typical "top-N" because users browsing
/// clipboard history want to scroll back through many entries, and
/// SwiftUI's LazyVStack keeps the off-screen rows virtually free.
/// The UI layer adds scroll indicators when this overflows the
/// visible viewport
const RESULT_LIMIT: usize = 200;
const PREVIEW_CHARS: usize = 80;

pub struct ClipboardProvider {
    index: Arc<Index>,
}

impl ClipboardProvider {
    pub fn new(index: Arc<Index>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl Provider for ClipboardProvider {
    fn id(&self) -> &str {
        "clip"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let filter = query.pattern();
        let items = if filter.is_empty() {
            self.index.clipboard_recent(RESULT_LIMIT).unwrap_or_default()
        } else {
            self.index
                .clipboard_search(filter, RESULT_LIMIT)
                .unwrap_or_default()
        };
        let now = now_secs();
        items.into_iter().map(|item| to_candidate(item, now)).collect()
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> anyhow::Result<Effect> {
        let id_str = id
            .strip_prefix("clip::")
            .ok_or_else(|| anyhow::anyhow!("invalid clip candidate id: {id}"))?;
        let numeric: i64 = id_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid clip id: {id_str}"))?;
        let item = self
            .index
            .clipboard_get(numeric)?
            .ok_or_else(|| anyhow::anyhow!("clipboard item {numeric} not found"))?;
        // Image-typed item: read the PNG off disk and emit a copy /
        // preview effect that carries bytes (base64) - row
        // lives in same `clipboard_items` table as text rows but
        // takes a different activation path
        if let Some(path) = image_path(&item.content) {
            return activate_image(&path, action);
        }
        let trimmed = item.content.trim();
        match action {
            "default" => Ok(Effect::CopyToClipboard(item.content)),
            "preview" => {
                // Language hint drives the renderer: JSON gets
                // syntax highlighting, URLs stay plain (they're one
                // line anyway), and hex colours show the literal
                // value in the preview so users can copy variants
                let language = match detect(&item.content) {
                    ContentKind::Json => Some("json".to_string()),
                    _ => None,
                };
                let label = match detect(&item.content) {
                    ContentKind::Url => "Clipboard · URL",
                    ContentKind::Json => "Clipboard · JSON",
                    ContentKind::HexColor => "Clipboard · Color",
                    ContentKind::Email => "Clipboard · Email",
                    ContentKind::Path => "Clipboard · Path",
                    ContentKind::Number => "Clipboard · Number",
                    ContentKind::UnixTs => "Clipboard · Timestamp",
                    ContentKind::Text => "Clipboard",
                };
                Ok(Effect::ShowText {
                    text: item.content,
                    label: label.to_string(),
                    language,
                    editable_path: None,
                })
            }
            "open-url" => Ok(Effect::OpenUrl(trimmed.to_string())),
            "format-json" => {
                let val: serde_json::Value = serde_json::from_str(trimmed)?;
                let pretty = serde_json::to_string_pretty(&val)?;
                Ok(Effect::CopyToClipboard(pretty))
            }
            "open-mail" => Ok(Effect::OpenUrl(format!("mailto:{trimmed}"))),
            "reveal-path" => Ok(Effect::RevealInFinder(
                std::path::PathBuf::from(trimmed),
            )),
            "open-path" => Ok(Effect::OpenPath(std::path::PathBuf::from(trimmed))),
            "copy-hex" => {
                let n = parse_number(trimmed)?;
                Ok(Effect::CopyToClipboard(format!("0x{n:x}")))
            }
            "copy-bin" => {
                let n = parse_number(trimmed)?;
                Ok(Effect::CopyToClipboard(format!("0b{n:b}")))
            }
            "copy-oct" => {
                let n = parse_number(trimmed)?;
                Ok(Effect::CopyToClipboard(format!("0o{n:o}")))
            }
            "format-datetime" => {
                let ts = parse_unix_ts(trimmed)
                    .ok_or_else(|| anyhow::anyhow!("not a Unix timestamp: {trimmed}"))?;
                Ok(Effect::CopyToClipboard(format_unix_utc(ts)))
            }
            other => anyhow::bail!("unknown action for clip: {other}"),
        }
    }
}

/// Strip the image sentinel prefix and return the on-disk path when
/// the content marks an image entry. None for plain-text rows
fn image_path(content: &str) -> Option<PathBuf> {
    content
        .strip_prefix(IMAGE_SENTINEL_PREFIX)
        .map(PathBuf::from)
}

/// Activate an image clipboard row. Reads the PNG bytes from disk
/// once, then dispatches requested action against them
fn activate_image(path: &std::path::Path, action: &str) -> anyhow::Result<Effect> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("read clipboard image {}: {e}", path.display()))?;
    let b64 = BASE64_STANDARD.encode(&bytes);
    match action {
        // Enter on an image row puts the image back on pasteboard
        // (the Swift side handles it via existing
        // `Effect::CopyImagePng` branch)
        "default" | "copy-image" => Ok(Effect::CopyImagePng(b64)),
        "preview" => Ok(Effect::ShowImagePng(b64)),
        // Fall-back: reveal source file in Finder so power users
        // can grab the on-disk artefact directly
        "reveal-image" => Ok(Effect::RevealInFinder(path.to_path_buf())),
        other => anyhow::bail!("unknown action for clip image: {other}"),
    }
}

fn to_candidate(item: ClipboardItem, now: i64) -> Candidate {
    if let Some(path) = image_path(&item.content) {
        return image_candidate(item.id, &path, item.ts, now);
    }
    let kind = detect(&item.content);
    // Every clipboard row gets the inline preview action so -> shows
    // the full content in a scrollable view. Kinds that have a more
    // specialised action (open URL, pretty-print JSON) still expose
    // those alongside, but `preview` is always first in actions
    // list after default "Copy" so -> always does what user
    // expects
    let preview_action = Action::new("preview", "Preview");
    let (icon, actions) = match kind {
        ContentKind::Url => (
            Icon::SfSymbol("link".into()),
            vec![
                Action::primary("Copy"),
                preview_action,
                Action::new("open-url", "Open URL"),
            ],
        ),
        ContentKind::Json => (
            Icon::SfSymbol("curlybraces".into()),
            vec![
                Action::primary("Copy"),
                preview_action,
                Action::new("format-json", "Copy Pretty-Printed"),
            ],
        ),
        ContentKind::HexColor => {
            // Normalise to #rrggbb for the swatch renderer
            let hex = normalize_hex_color(item.content.trim()).unwrap_or_else(|| "#000000".into());
            (Icon::ColorSwatch(hex), vec![Action::primary("Copy"), preview_action])
        }
        ContentKind::Email => (
            Icon::SfSymbol("envelope".into()),
            vec![
                Action::primary("Copy"),
                preview_action,
                Action::new("open-mail", "Compose Email"),
            ],
        ),
        ContentKind::Path => (
            Icon::SfSymbol("folder".into()),
            vec![
                Action::primary("Copy"),
                preview_action,
                Action::new("open-path", "Open"),
                Action::new("reveal-path", "Reveal in Finder"),
            ],
        ),
        ContentKind::Number => (
            Icon::SfSymbol("number.square".into()),
            vec![
                Action::primary("Copy"),
                preview_action,
                Action::new("copy-hex", "Copy as Hex"),
                Action::new("copy-bin", "Copy as Binary"),
                Action::new("copy-oct", "Copy as Octal"),
            ],
        ),
        ContentKind::UnixTs => (
            Icon::SfSymbol("clock".into()),
            vec![
                Action::primary("Copy"),
                preview_action,
                Action::new("format-datetime", "Copy as Datetime"),
            ],
        ),
        ContentKind::Text => (
            Icon::SfSymbol("doc.on.clipboard".into()),
            vec![Action::primary("Copy"), preview_action],
        ),
    };
    Candidate {
        id: format!("clip::{}", item.id),
        title: preview(&item.content),
        // Origin label + age. Without the "clipboard" prefix, a
        // clipboard row matched by a bare query (e.g. typing "ni"
        // that fuzzy-hits a stored "Backend:... " copy) looks
        // indistinguishable from a note or search hit - users
        // couldn't tell where row came from. Prefix fixes that
        subtitle: Some(format!("clipboard · {}", format_relative(now - item.ts))),
        icon,
        kind: CandidateKind::Clipboard,
        actions,
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Render a clipboard image row. Title carries pixel dimensions when
/// we can read them quickly (instant feedback so user knows
/// which copy is which); otherwise falls back to filename. The
/// thumbnail comes from `Icon::ImagePath` - Swift loads file
/// and shows a clipped preview
fn image_candidate(id: i64, path: &std::path::Path, ts: i64, now: i64) -> Candidate {
    // Best-effort dimension probe via file's metadata. We do NOT
    // decode the full image - `image::ImageReader` reads only the
    // header. Cheap on every keystroke; falls through silently when
    // file is missing or corrupt
    let dimensions = image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map(|(w, h)| format!("Image · {w}×{h}"))
        .unwrap_or_else(|| "Image".to_string());
    Candidate {
        id: format!("clip::{id}"),
        title: dimensions,
        subtitle: Some(format!("clipboard · {}", format_relative(now - ts))),
        icon: Icon::ImagePath(path.to_path_buf()),
        kind: CandidateKind::Clipboard,
        actions: vec![
            Action::primary("Copy image"),
            Action::new("preview", "Preview"),
            Action::new("reveal-image", "Reveal in Finder"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Content-type classifier for clipboard items. Cheap substring / first-
/// char checks; nothing is expensive enough to cache
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Url,
    Json,
    HexColor,
    Email,
    Path,
    Number,
    UnixTs,
}

pub fn detect(s: &str) -> ContentKind {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return ContentKind::Text;
    }
    // Ordering matters: check specific shapes before the generic Text
    // fallback. UnixTs is checked before Number because a 13-digit
    // integer is both valid - the timestamp interpretation is more
    // useful. Hex before Number because `0xff` parses as a number too
    if is_url(trimmed) {
        return ContentKind::Url;
    }
    if is_email(trimmed) {
        return ContentKind::Email;
    }
    if is_hex_color(trimmed) {
        return ContentKind::HexColor;
    }
    if is_unix_ts(trimmed) {
        return ContentKind::UnixTs;
    }
    if is_number(trimmed) {
        return ContentKind::Number;
    }
    if is_path(trimmed) {
        return ContentKind::Path;
    }
    if is_json(trimmed) {
        return ContentKind::Json;
    }
    ContentKind::Text
}

fn is_url(s: &str) -> bool {
    if s.contains(char::is_whitespace) {
        return false;
    }
    (s.starts_with("http://") || s.starts_with("https://")) && s.len() > 8
}

fn is_hex_color(s: &str) -> bool {
    let Some(body) = s.strip_prefix('#') else { return false; };
    matches!(body.len(), 3 | 4 | 6 | 8) && body.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_json(s: &str) -> bool {
    // Cheap structural check - only attempt parse for inputs that *look*
    // like JSON. Avoids paying serde on every text clip
    let first = s.chars().next();
    let last = s.chars().last();
    match (first, last) {
        (Some('{'), Some('}')) | (Some('['), Some(']')) => {
            serde_json::from_str::<serde_json::Value>(s).is_ok()
        }
        _ => false,
    }
}

/// Minimal, conservative email match. A full RFC 5322 regex would
/// waste cycles on every clipboard write; the launcher only needs
/// "looks like mailto-able" with no false positives on plain prose
fn is_email(s: &str) -> bool {
    if s.contains(char::is_whitespace) {
        return false;
    }
    let Some(at) = s.find('@') else { return false; };
    // Must have chars on both sides, and the local part can't start or
    // end with a dot. Domain must contain at least one dot with labels
    // on either side
    let (local, domain_with_at) = s.split_at(at);
    let domain = &domain_with_at[1..];
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    if local.starts_with('.') || local.ends_with('.') {
        return false;
    }
    if !domain.contains('.') {
        return false;
    }
    // Reject doubled @ or garbage chars - keep the filter tight enough
    // to stay free of false positives on URLs like `user@host/path`
    // which URL branch already handled above
    if s.matches('@').count() != 1 {
        return false;
    }
    let ok = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    local.chars().all(ok) && domain.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// Absolute filesystem paths that actually exist. Relative paths get
/// classified as plain text - too many false positives otherwise
/// (snippets, git branches, URLs-without-schemes would all trip it)
fn is_path(s: &str) -> bool {
    if !s.starts_with('/') && !s.starts_with('~') {
        return false;
    }
    if s.contains('\n') {
        return false;
    }
    let expanded = expand_tilde(s);
    std::path::Path::new(&expanded).exists()
}

fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    s.to_string()
}

/// Integer / float detector. Accepts leading sign, shows as
/// thousands separators, `0x`/`0b`/`0o` prefixes. Intentionally
/// rejects anything that also parses as a Unix timestamp (10- / 13-
/// digit bare integer) - that path is more useful as `UnixTs`
fn is_number(s: &str) -> bool {
    let cleaned = s.replace('_', "");
    if cleaned.is_empty() {
        return false;
    }
    if parse_number(&cleaned).is_ok() {
        return true;
    }
    // Float path - last resort, only if int parse failed
    cleaned.parse::<f64>().is_ok()
}

/// Parse a user-provided numeric string into a `u64` for radix
/// conversions. Handles negatives via two's-complement wrap so
/// `-1` -> `0xffffffffffffffff` rather than an error - matches the
/// mental model of bit patterns
pub fn parse_number(s: &str) -> anyhow::Result<u64> {
    let cleaned = s.trim().replace('_', "");
    let (sign, body) = if let Some(rest) = cleaned.strip_prefix('-') {
        (-1i128, rest.to_string())
    } else if let Some(rest) = cleaned.strip_prefix('+') {
        (1i128, rest.to_string())
    } else {
        (1i128, cleaned)
    };
    let magnitude = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)?
    } else if let Some(bin) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        u64::from_str_radix(bin, 2)?
    } else if let Some(oct) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
        u64::from_str_radix(oct, 8)?
    } else {
        body.parse::<u64>()?
    };
    Ok(if sign < 0 {
        (magnitude as i64).wrapping_neg() as u64
    } else {
        magnitude
    })
}

/// 10-digit seconds or 13-digit milliseconds Unix timestamps. Narrow
/// to the plausible 2001-2286 range on the seconds side so a random
/// 10-digit product code doesn't start suggesting "as Datetime"
fn is_unix_ts(s: &str) -> bool {
    parse_unix_ts(s).is_some()
}

pub fn parse_unix_ts(s: &str) -> Option<i64> {
    let trimmed = s.trim();
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = trimmed.parse().ok()?;
    match trimmed.len() {
        // Plausible seconds - 2001 -> 2286
        10 if (1_000_000_000..9_999_999_999_i64).contains(&n) => Some(n),
        // Milliseconds - 2001 -> 2286. Convert to seconds for the
        // datetime formatter
        13 if (1_000_000_000_000..9_999_999_999_999_i64).contains(&n) => Some(n / 1000),
        _ => None,
    }
}

/// `YYYY-MM-DD HH:MM:SS UTC`. Good enough for clipboard context -
/// users who want a timezone-aware format can reach for a dedicated
/// formatter; most clipboard timestamps are log lines where UTC is
/// what you want anyway
pub fn format_unix_utc(ts: i64) -> String {
    use chrono::{DateTime, Utc};
    DateTime::<Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}

/// Normalise a hex colour to `#rrggbb`. Supports 3 / 4 / 6 / 8 digit
/// forms. Returns None for invalid input (caller already gated on
/// `is_hex_color`, so this just computes)
pub fn normalize_hex_color(s: &str) -> Option<String> {
    let body = s.strip_prefix('#')?;
    let hex6 = match body.len() {
        3 => {
            let bytes = body.as_bytes();
            format!(
                "{0}{0}{1}{1}{2}{2}",
                bytes[0] as char, bytes[1] as char, bytes[2] as char
            )
        }
        4 => {
            // #rgba -> drop alpha
            let bytes = body.as_bytes();
            format!(
                "{0}{0}{1}{1}{2}{2}",
                bytes[0] as char, bytes[1] as char, bytes[2] as char
            )
        }
        6 => body.to_string(),
        8 => body[..6].to_string(), // #rrggbbaa -> drop alpha
        _ => return None,
    };
    Some(format!("#{}", hex6.to_ascii_lowercase()))
}

fn preview(content: &str) -> String {
    let first_line = content.lines().next().unwrap_or("").trim();
    let mut chars = first_line.chars();
    let truncated: String = chars.by_ref().take(PREVIEW_CHARS).collect();
    if chars.next().is_some() || content.lines().count() > 1 {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn format_relative(age_secs: i64) -> String {
    let s = age_secs.max(0);
    match s {
        0..=4 => "just now".into(),
        5..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_index() -> Arc<Index> {
        Arc::new(Index::in_memory().unwrap())
    }

    #[tokio::test]
    async fn empty_index_yields_no_candidates() {
        let p = ClipboardProvider::new(fresh_index());
        assert!(p.query(&Query::new("")).await.is_empty());
    }

    #[tokio::test]
    async fn empty_filter_returns_recent() {
        let idx = fresh_index();
        idx.record_clipboard("first", 100).unwrap();
        idx.record_clipboard("second", 200).unwrap();
        idx.record_clipboard("third", 300).unwrap();

        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].title, "third"); // newest first
        assert_eq!(out[0].kind, CandidateKind::Clipboard);
        assert!(out[0].bypass_rank);
    }

    #[tokio::test]
    async fn every_clipboard_row_exposes_preview_action() {
        // `->` on any clipboard row should open an inline preview.
        // The exact content kind doesn't matter - all four must carry
        // the `preview` action id
        let idx = fresh_index();
        idx.record_clipboard("just text", 100).unwrap();
        idx.record_clipboard("https://example.com", 200).unwrap();
        idx.record_clipboard(r#"{"a":1}"#, 300).unwrap();
        idx.record_clipboard("#ff00aa", 400).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), 4);
        for c in &out {
            assert!(
                c.actions.iter().any(|a| a.id == "preview"),
                "row {:?} missing preview action; actions = {:?}",
                c.title, c.actions.iter().map(|a| &a.id).collect::<Vec<_>>()
            );
        }
    }

    #[tokio::test]
    async fn activate_preview_shows_text_with_kind_label() {
        let idx = fresh_index();
        idx.record_clipboard(r#"{"a":1}"#, 100).unwrap();
        let p = ClipboardProvider::new(Arc::clone(&idx));
        let out = p.query(&Query::new("")).await;
        let effect = p.activate(&out[0].id, "preview").await.unwrap();
        match effect {
            Effect::ShowText { text, label, language, editable_path } => {
                assert_eq!(text, r#"{"a":1}"#);
                assert_eq!(label, "Clipboard · JSON");
                assert_eq!(language.as_deref(), Some("json"));
                assert!(editable_path.is_none());
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_preview_for_color_carries_hex_text() {
        // Users preview a color to read its literal value for reuse
        // (e.g. swap `#ff00aa` -> `#ff00bb`). Body must round-trip
        let idx = fresh_index();
        idx.record_clipboard("#ff00aa", 100).unwrap();
        let p = ClipboardProvider::new(Arc::clone(&idx));
        let out = p.query(&Query::new("")).await;
        let effect = p.activate(&out[0].id, "preview").await.unwrap();
        if let Effect::ShowText { text, label, .. } = effect {
            assert_eq!(text, "#ff00aa");
            assert_eq!(label, "Clipboard · Color");
        } else {
            panic!("expected ShowText");
        }
    }

    #[tokio::test]
    async fn subtitle_labels_row_as_clipboard() {
        // REGRESSION: clipboard rows that appear through cross-provider
        // fuzzy matching (e.g. typing "ni" surfaces an old "Backend:..."
        // copy) used to look indistinguishable from notes or other
        // sources - subtitle was just "40s ago". The origin label
        // must always be present so users can tell what they're
        // looking at at a glance
        let idx = fresh_index();
        idx.record_clipboard("Backend: secret", now_secs() - 40).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), 1);
        let sub = out[0].subtitle.as_deref().unwrap();
        assert!(sub.starts_with("clipboard · "), "got {sub:?}");
        assert!(sub.contains("ago") || sub.contains("just now"),
                "age present: {sub:?}");
    }

    #[tokio::test]
    async fn filter_applies_substring_match() {
        let idx = fresh_index();
        idx.record_clipboard("hello world", 100).unwrap();
        idx.record_clipboard("foo bar", 200).unwrap();
        idx.record_clipboard("say hello", 300).unwrap();

        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("hello")).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "say hello"); // newest match first
    }

    #[tokio::test]
    async fn preview_truncates_long_content() {
        let idx = fresh_index();
        let long = "a".repeat(200);
        idx.record_clipboard(&long, 100).unwrap();

        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.ends_with("…"));
        assert!(out[0].title.chars().count() <= PREVIEW_CHARS + 1);
    }

    #[tokio::test]
    async fn preview_only_first_line_for_multiline() {
        let idx = fresh_index();
        idx.record_clipboard("first line\nsecond line\nthird", 100).unwrap();

        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert_eq!(out[0].title, "first line…");
    }

    #[tokio::test]
    async fn activate_returns_copy_effect_with_full_content() {
        let idx = fresh_index();
        let long = "first line\nsecond line";
        idx.record_clipboard(long, 100).unwrap();

        let p = ClipboardProvider::new(idx.clone());
        let out = p.query(&Query::new("")).await;
        let id = out[0].id.clone();
        let effect = p.activate(&id, "default").await.unwrap();

        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, long),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_rejects_foreign_id() {
        let p = ClipboardProvider::new(fresh_index());
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_rejects_malformed_id() {
        let p = ClipboardProvider::new(fresh_index());
        assert!(p.activate(&"clip::abc".to_string(), "default").await.is_err());
    }

    #[tokio::test]
    async fn activate_rejects_unknown_id() {
        let p = ClipboardProvider::new(fresh_index());
        assert!(p.activate(&"clip::999".to_string(), "default").await.is_err());
    }

    // Pure-function tests on preview() and format_relative()

    #[test]
    fn preview_short_single_line() {
        assert_eq!(preview("hi"), "hi");
    }

    #[test]
    fn preview_trims_first_line() {
        assert_eq!(preview("  padded  "), "padded");
    }

    #[test]
    fn preview_appends_ellipsis_for_long() {
        let s = "x".repeat(100);
        let got = preview(&s);
        assert_eq!(got.chars().count(), PREVIEW_CHARS + 1);
        assert!(got.ends_with("…"));
    }

    #[test]
    fn preview_empty_is_empty() {
        assert_eq!(preview(""), "");
    }

    #[test]
    fn format_relative_just_now() {
        assert_eq!(format_relative(0), "just now");
        assert_eq!(format_relative(4), "just now");
    }

    #[test]
    fn format_relative_seconds() {
        assert_eq!(format_relative(30), "30s ago");
    }

    #[test]
    fn format_relative_minutes() {
        assert_eq!(format_relative(120), "2m ago");
    }

    #[test]
    fn format_relative_hours() {
        assert_eq!(format_relative(7200), "2h ago");
    }

    #[test]
    fn format_relative_days() {
        assert_eq!(format_relative(172_800), "2d ago");
    }

    #[test]
    fn format_relative_negative_is_just_now() {
        assert_eq!(format_relative(-10), "just now");
    }


    #[test]
    fn detect_plain_text() {
        assert_eq!(detect("hello world"), ContentKind::Text);
        assert_eq!(detect(""), ContentKind::Text);
        assert_eq!(detect("   "), ContentKind::Text);
    }

    #[test]
    fn detect_https_url() {
        assert_eq!(detect("https://example.com/path?q=1"), ContentKind::Url);
        assert_eq!(detect("http://example.com"), ContentKind::Url);
    }

    #[test]
    fn detect_url_rejects_whitespace() {
        assert_eq!(detect("https://ex.com with notes"), ContentKind::Text);
    }

    #[test]
    fn detect_hex_colors() {
        assert_eq!(detect("#fff"), ContentKind::HexColor);
        assert_eq!(detect("#ff00aa"), ContentKind::HexColor);
        assert_eq!(detect("#abcd"), ContentKind::HexColor);
        assert_eq!(detect("#abcdef12"), ContentKind::HexColor);
    }

    #[test]
    fn detect_hex_color_rejects_bogus() {
        assert_eq!(detect("#xyz"), ContentKind::Text);
        assert_eq!(detect("#12345"), ContentKind::Text); // 5 chars
        assert_eq!(detect("##abc"), ContentKind::Text);
    }

    #[test]
    fn detect_json_object_and_array() {
        assert_eq!(detect(r#"{"a":1}"#), ContentKind::Json);
        assert_eq!(detect("[1,2,3]"), ContentKind::Json);
    }

    #[test]
    fn detect_json_rejects_invalid() {
        assert_eq!(detect(r#"{"a":}"#), ContentKind::Text);
        assert_eq!(detect("not json"), ContentKind::Text);
    }

    #[test]
    fn normalize_hex_color_expands_short_forms() {
        assert_eq!(normalize_hex_color("#fff").as_deref(), Some("#ffffff"));
        assert_eq!(normalize_hex_color("#abc").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_hex_color("#abcdef").as_deref(), Some("#abcdef"));
        assert_eq!(normalize_hex_color("#abcdef12").as_deref(), Some("#abcdef"));
    }

    #[tokio::test]
    async fn url_item_gets_open_action() {
        let idx = fresh_index();
        idx.record_clipboard("https://example.com", 100).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert!(out[0].actions.iter().any(|a| a.id == "open-url"));
    }

    #[tokio::test]
    async fn json_item_gets_format_action() {
        let idx = fresh_index();
        idx.record_clipboard(r#"{"a":1}"#, 100).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert!(out[0].actions.iter().any(|a| a.id == "format-json"));
    }

    #[tokio::test]
    async fn hex_color_item_gets_swatch_icon() {
        let idx = fresh_index();
        idx.record_clipboard("#ff00aa", 100).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        match &out[0].icon {
            Icon::ColorSwatch(hex) => assert!(hex.starts_with('#')),
            other => panic!("expected ColorSwatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_open_url_emits_open_url() {
        let idx = fresh_index();
        idx.record_clipboard("https://example.com", 100).unwrap();
        let p = ClipboardProvider::new(idx.clone());
        let out = p.query(&Query::new("")).await;
        let eff = p.activate(&out[0].id, "open-url").await.unwrap();
        match eff {
            Effect::OpenUrl(s) => assert_eq!(s, "https://example.com"),
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_format_json_returns_pretty() {
        let idx = fresh_index();
        idx.record_clipboard(r#"{"a":1,"b":2}"#, 100).unwrap();
        let p = ClipboardProvider::new(idx.clone());
        let out = p.query(&Query::new("")).await;
        let eff = p.activate(&out[0].id, "format-json").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => {
                assert!(s.contains('\n'), "pretty output: {s:?}");
                assert!(s.contains("\"a\""));
            }
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    // Smart categorization: email / path / number / timestamp

    #[test]
    fn detects_plain_emails() {
        assert_eq!(detect("alice@example.com"), ContentKind::Email);
        assert_eq!(detect("first.last+tag@sub.example.co.uk"), ContentKind::Email);
    }

    #[test]
    fn email_detector_isnt_over_eager() {
        // URLs that happen to contain `@` (basic auth) already match
        // URL branch first - but verify the email branch itself
        // still rejects these shapes
        assert_ne!(detect("https://user@example.com/path"), ContentKind::Email);
        assert_ne!(detect("@handle"), ContentKind::Email);
        assert_ne!(detect("missing@domain"), ContentKind::Email); // no TLD dot
        assert_ne!(detect("has spaces @ not.allowed"), ContentKind::Email);
        assert_ne!(detect("double@@at.com"), ContentKind::Email);
    }

    #[test]
    fn detects_unix_timestamps_in_seconds_and_ms() {
        // 2024-01-01 in seconds / milliseconds
        assert_eq!(detect("1704067200"), ContentKind::UnixTs);
        assert_eq!(detect("1704067200000"), ContentKind::UnixTs);
    }

    #[test]
    fn timestamp_detector_skips_short_integers() {
        // A 4-digit ID is just a number, not a timestamp
        assert_eq!(detect("1234"), ContentKind::Number);
        assert_eq!(detect("42"), ContentKind::Number);
    }

    #[test]
    fn detects_numbers_with_base_prefixes() {
        assert_eq!(detect("0xff"), ContentKind::Number);
        assert_eq!(detect("0b1010"), ContentKind::Number);
        assert_eq!(detect("0o17"), ContentKind::Number);
        assert_eq!(detect("-42"), ContentKind::Number);
        assert_eq!(detect("1_000_000"), ContentKind::Number);
    }

    #[test]
    fn absolute_path_that_doesnt_exist_stays_text() {
        // Path detection gates on "exists on disk" - a non-existent
        // /tmp/foo shouldn't pull up the Open / Reveal actions because
        // they'd fail. Fuzzy launchers that lie about the kind are
        // worse than ones that dont detect at all
        assert_eq!(
            detect("/this/definitely/doesnt/exist/gyors-test"),
            ContentKind::Text,
        );
    }

    #[test]
    fn path_detection_sees_existing_directory() {
        // `/tmp` on macOS exists as a symlink and is a safe place to
        // pin an "exists on disk" assertion without creating files
        assert_eq!(detect("/tmp"), ContentKind::Path);
    }

    #[tokio::test]
    async fn email_row_exposes_compose_action() {
        let idx = fresh_index();
        idx.record_clipboard("alice@example.com", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert!(
            out[0].actions.iter().any(|a| a.id == "open-mail"),
            "actions: {:?}",
            out[0].actions,
        );
    }

    #[tokio::test]
    async fn mailto_activation_returns_openurl_effect() {
        let idx = fresh_index();
        idx.record_clipboard("alice@example.com", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        let effect = p.activate(&out[0].id, "open-mail").await.unwrap();
        match effect {
            Effect::OpenUrl(s) => assert_eq!(s, "mailto:alice@example.com"),
            other => panic!("expected OpenUrl, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn number_row_exposes_radix_actions() {
        let idx = fresh_index();
        idx.record_clipboard("255", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        let ids: Vec<_> = out[0].actions.iter().map(|a| a.id.clone()).collect();
        assert!(ids.contains(&"copy-hex".to_string()), "ids={ids:?}");
        assert!(ids.contains(&"copy-bin".to_string()));
        assert!(ids.contains(&"copy-oct".to_string()));
    }

    #[tokio::test]
    async fn radix_activations_produce_expected_encodings() {
        let idx = fresh_index();
        idx.record_clipboard("255", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        let hex = p.activate(&out[0].id, "copy-hex").await.unwrap();
        let bin = p.activate(&out[0].id, "copy-bin").await.unwrap();
        let oct = p.activate(&out[0].id, "copy-oct").await.unwrap();
        assert!(matches!(hex, Effect::CopyToClipboard(ref s) if s == "0xff"));
        assert!(matches!(bin, Effect::CopyToClipboard(ref s) if s == "0b11111111"));
        assert!(matches!(oct, Effect::CopyToClipboard(ref s) if s == "0o377"));
    }

    #[tokio::test]
    async fn timestamp_row_offers_datetime_action() {
        let idx = fresh_index();
        idx.record_clipboard("1704067200", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert!(
            out[0].actions.iter().any(|a| a.id == "format-datetime"),
            "actions: {:?}",
            out[0].actions,
        );
    }

    #[tokio::test]
    async fn format_datetime_yields_human_readable_utc() {
        let idx = fresh_index();
        // 2024-01-01T00:00:00 UTC
        idx.record_clipboard("1704067200", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        let effect = p.activate(&out[0].id, "format-datetime").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "2024-01-01 00:00:00 UTC"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn existing_path_row_offers_open_and_reveal() {
        let idx = fresh_index();
        idx.record_clipboard("/tmp", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        let ids: Vec<_> = out[0].actions.iter().map(|a| a.id.clone()).collect();
        assert!(ids.contains(&"open-path".to_string()), "ids={ids:?}");
        assert!(ids.contains(&"reveal-path".to_string()));
    }

    #[tokio::test]
    async fn reveal_activation_returns_reveal_effect() {
        let idx = fresh_index();
        idx.record_clipboard("/tmp", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        let effect = p.activate(&out[0].id, "reveal-path").await.unwrap();
        match effect {
            Effect::RevealInFinder(p) => assert_eq!(p, std::path::PathBuf::from("/tmp")),
            other => panic!("expected RevealInFinder, got {other:?}"),
        }
    }

    #[test]
    fn parse_number_accepts_mixed_shapes() {
        assert_eq!(parse_number("42").unwrap(), 42);
        assert_eq!(parse_number("0xff").unwrap(), 255);
        assert_eq!(parse_number("0b1010").unwrap(), 10);
        assert_eq!(parse_number("0o17").unwrap(), 15);
        assert_eq!(parse_number("1_000_000").unwrap(), 1_000_000);
        // Negative -> two's complement wrap. Lets user see the
        // bit pattern without an error
        assert_eq!(parse_number("-1").unwrap(), u64::MAX);
    }

    #[test]
    fn parse_number_rejects_garbage() {
        assert!(parse_number("").is_err());
        assert!(parse_number("hello").is_err());
        assert!(parse_number("0xZ").is_err());
    }

    #[test]
    fn format_unix_utc_formats_known_epoch_zero() {
        assert_eq!(format_unix_utc(0), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn parse_unix_ts_rejects_out_of_range_digits() {
        // Nine-digit bare number falls below the seconds-Unix window
        // (Sep 2001 onward) - parse_unix_ts should say no so the
        // candidate still classifies as a regular Number
        assert!(parse_unix_ts("999999999").is_none());
        // Twelve-digit falls in the pre-epoch window for ms
        assert!(parse_unix_ts("100000000000").is_none());
    }

    #[test]
    fn number_classifier_rejects_mixed_content() {
        // `42abc` is text, not a number - avoids false hits for ids
        // like `42abc-session`
        assert_eq!(detect("42abc"), ContentKind::Text);
    }

    #[tokio::test]
    async fn preview_action_label_reflects_detected_kind() {
        // Each kind's preview label helps user know what they're
        // looking at in the inline preview pane. Verify a couple of
        // the newer kinds land the right label
        let idx = fresh_index();
        idx.record_clipboard("alice@example.com", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        let effect = p.activate(&out[0].id, "preview").await.unwrap();
        match effect {
            Effect::ShowText { label, .. } => {
                assert!(label.contains("Email"), "label={label}");
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preview_action_label_for_unix_ts_row() {
        let idx = fresh_index();
        idx.record_clipboard("1704067200", 1).unwrap();
        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        let effect = p.activate(&out[0].id, "preview").await.unwrap();
        match effect {
            Effect::ShowText { label, .. } => {
                assert!(label.contains("Timestamp"), "label={label}");
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }


    #[test]
    fn image_path_strips_sentinel_prefix() {
        let path = image_path("gyors-image:/tmp/foo.png").unwrap();
        assert_eq!(path, std::path::PathBuf::from("/tmp/foo.png"));
        assert!(image_path("plain text").is_none());
        assert!(image_path("hello://not-a-clipboard-image").is_none());
    }

    /// Helper: write a tiny 1x1 PNG to a temp path so the
    /// dimension-probe / read-bytes paths have something to chew on
    fn write_tiny_png() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.png");
        let img = image::ImageBuffer::from_pixel(2, 3, image::Luma([255u8]));
        img.save_with_format(&path, image::ImageFormat::Png).unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn image_row_renders_with_image_path_icon_and_dimensions() {
        let (_dir, path) = write_tiny_png();
        let idx = fresh_index();
        idx.record_clipboard(
            &format!("{IMAGE_SENTINEL_PREFIX}{}", path.display()),
            500,
        )
        .unwrap();

        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), 1);
        let row = &out[0];
        match &row.icon {
            Icon::ImagePath(p) => assert_eq!(p, &path),
            other => panic!("expected ImagePath icon, got {other:?}"),
        }
        assert_eq!(row.title, "Image · 2×3", "got title: {}", row.title);
        // Dedicated action set - no `default`/Copy on text rows
        assert!(
            row.actions.iter().any(|a| a.label == "Copy image"),
            "actions: {:?}",
            row.actions.iter().map(|a| &a.label).collect::<Vec<_>>()
        );
        assert!(row.actions.iter().any(|a| a.id == "preview"));
        assert!(row.actions.iter().any(|a| a.id == "reveal-image"));
    }

    #[tokio::test]
    async fn image_row_default_activate_copies_png_bytes() {
        let (_dir, path) = write_tiny_png();
        let idx = fresh_index();
        idx.record_clipboard(
            &format!("{IMAGE_SENTINEL_PREFIX}{}", path.display()),
            600,
        )
        .unwrap();
        let p = ClipboardProvider::new(idx.clone());
        let out = p.query(&Query::new("")).await;
        let id = out[0].id.clone();
        match p.activate(&id, "default").await.unwrap() {
            Effect::CopyImagePng(b64) => {
                let bytes = BASE64_STANDARD.decode(&b64).unwrap();
                assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
            }
            other => panic!("expected CopyImagePng, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn image_row_preview_action_emits_show_image_png() {
        let (_dir, path) = write_tiny_png();
        let idx = fresh_index();
        idx.record_clipboard(
            &format!("{IMAGE_SENTINEL_PREFIX}{}", path.display()),
            700,
        )
        .unwrap();
        let p = ClipboardProvider::new(idx);
        let id = p.query(&Query::new("")).await[0].id.clone();
        match p.activate(&id, "preview").await.unwrap() {
            Effect::ShowImagePng(_) => {}
            other => panic!("expected ShowImagePng, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn image_row_reveal_action_emits_reveal_in_finder() {
        let (_dir, path) = write_tiny_png();
        let idx = fresh_index();
        idx.record_clipboard(
            &format!("{IMAGE_SENTINEL_PREFIX}{}", path.display()),
            800,
        )
        .unwrap();
        let p = ClipboardProvider::new(idx);
        let id = p.query(&Query::new("")).await[0].id.clone();
        match p.activate(&id, "reveal-image").await.unwrap() {
            Effect::RevealInFinder(p) => assert_eq!(p, path),
            other => panic!("expected RevealInFinder, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn image_row_with_missing_file_falls_back_to_image_label() {
        let idx = fresh_index();
        idx.record_clipboard(
            &format!("{IMAGE_SENTINEL_PREFIX}/tmp/does-not-exist-{}.png", uuid::Uuid::new_v4()),
            900,
        )
        .unwrap();
        let p = ClipboardProvider::new(idx.clone());
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), 1);
        // Title falls back to "Image" when the dimension probe fails;
        // row still appears so user can clean it up
        assert_eq!(out[0].title, "Image");
        // Activate errors out rather than panicking
        let id = out[0].id.clone();
        let res = p.activate(&id, "default").await;
        assert!(res.is_err(), "expected error for missing image, got {res:?}");
    }

    #[tokio::test]
    async fn text_and_image_rows_coexist() {
        let (_dir, path) = write_tiny_png();
        let idx = fresh_index();
        idx.record_clipboard("hello world", 100).unwrap();
        idx.record_clipboard(
            &format!("{IMAGE_SENTINEL_PREFIX}{}", path.display()),
            200,
        )
        .unwrap();
        idx.record_clipboard("#ff5733", 300).unwrap();

        let p = ClipboardProvider::new(idx);
        let out = p.query(&Query::new("")).await;
        assert_eq!(out.len(), 3);
        // Newest first
        assert!(out[0].title.starts_with('#'));
        match &out[1].icon {
            Icon::ImagePath(_) => {}
            other => panic!("expected ImagePath at index 1, got {other:?}"),
        }
        assert_eq!(out[2].title, "hello world");
    }
}
