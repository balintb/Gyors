//! `scratch` - a persistent markdown drawer
//!
//! Notes need titles, filenames, folders. A scratchpad needs none of
//! that. It's one well-known file user can jot into without the
//! mental overhead of "is this worth a note?" - closer to a
//! REPL-for-thoughts than to a document
//!
//! File lives next to `config.json` (same `GYORS_CONFIG_DIR`
//! override applies) so a notes-folder move / rename doesn't take it
//! with it. On first use we create it empty; activating keyword
//! opens the inline editor on that path - same rendering pipeline as
//! notes, just pinned to one buffer
//!
//! Keyword matching is intentionally exact (`scratch` / `scratchpad`):
//! Fuzzy matching would scatter this row across every query that has
//! an "s" in it, which isn't what users expect from a launcher

use anyhow::Result;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::path::{Path, PathBuf};

pub struct ScratchpadProvider {
    path: PathBuf,
}

impl Default for ScratchpadProvider {
    fn default() -> Self {
        Self::new_at(default_scratchpad_path())
    }
}

impl ScratchpadProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `scratchpad.md` alongside user's `config.json`. Kept out of
/// the notes folder on purpose - moving the notes folder shouldn't
/// take your scratchpad with it, and mixing it into the notes index
/// would clutter note-search results with a pseudo-note
pub fn default_scratchpad_path() -> PathBuf {
    crate::config::config_path()
        .parent()
        .map(|p| p.join("scratchpad.md"))
        .unwrap_or_else(|| PathBuf::from("scratchpad.md"))
}

/// Match `scratch` or `scratchpad` (case-insensitive). Free text after
/// a separating space is returned so `scratch some thought` can route
/// to an "append" candidate later. Returns `None` when query
/// doesn't invoke keyword at all
fn strip_keyword(pattern: &str) -> Option<&str> {
    let trimmed = pattern.trim();
    for kw in ["scratchpad", "scratch"] {
        if trimmed.eq_ignore_ascii_case(kw) {
            return Some("");
        }
        if trimmed.len() > kw.len() {
            let (head, rest) = trimmed.split_at(kw.len());
            if head.eq_ignore_ascii_case(kw) && rest.starts_with(char::is_whitespace) {
                return Some(rest.trim_start());
            }
        }
    }
    None
}

/// Ensure the scratchpad file exists so editor has something to
/// open - a missing file would either surface an error to user or
/// leave editor empty without making clear that the first save
/// will create it. Creating up-front keeps the UX obvious
fn ensure_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, b"")?;
    Ok(())
}

fn open_candidate(path: &Path) -> Candidate {
    Candidate {
        id: "scratch::open".into(),
        title: "Scratchpad".into(),
        subtitle: Some(path.display().to_string()),
        icon: Icon::SfSymbol("scribble.variable".into()),
        kind: CandidateKind::Action,
        actions: vec![
            Action::primary("Open"),
            Action::new("copy-all", "Copy All"),
            Action::new("reveal", "Reveal in Finder"),
            Action::new("clear", "Clear"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn append_candidate(text: &str) -> Candidate {
    // Preview snippet - keeps row useful without letting a long
    // paste blow out the subtitle. 60 chars is where the cell starts
    // ellipsizing on a stock 800pt panel, so we truncate there
    let preview: String = text.chars().take(60).collect();
    let subtitle = if text.chars().count() > 60 {
        format!("… {preview}…")
    } else {
        format!("Appends + opens: {preview}")
    };
    Candidate {
        id: format!("scratch::append::{text}"),
        title: "Append to Scratchpad".into(),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol("text.badge.plus".into()),
        kind: CandidateKind::Action,
        // The primary action drops user into the inline editor
        // with the appended text already in place. "Silent append"
        // is still available as the secondary action for users who
        // want to paste-and-go without surfacing editor; it
        // remains addressable via cmdEnter-style action chooser
        actions: vec![
            Action::primary("Append and Edit"),
            Action::new("append-silent", "Append Silently"),
        ],
        search_text: String::new(),
        bypass_rank: true,
    }
}

#[async_trait]
impl Provider for ScratchpadProvider {
    fn id(&self) -> &str {
        "scratchpad"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(rest) = strip_keyword(query.pattern()) else {
            return Vec::new();
        };
        let mut out = vec![open_candidate(&self.path)];
        if !rest.is_empty() {
            out.push(append_candidate(rest));
        }
        out
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        if id == "scratch::open" {
            ensure_file(&self.path)?;
            return match action {
                "default" | "" => Ok(Effect::EditNote(self.path.clone())),
                "copy-all" => {
                    let content = std::fs::read_to_string(&self.path).unwrap_or_default();
                    Ok(Effect::CopyToClipboard(content))
                }
                "reveal" => Ok(Effect::RevealInFinder(self.path.clone())),
                "clear" => {
                    std::fs::write(&self.path, b"")?;
                    Ok(Effect::None)
                }
                other => anyhow::bail!("unknown scratchpad action: {other}"),
            };
        }
        if let Some(text) = id.strip_prefix("scratch::append::") {
            ensure_file(&self.path)?;
            append_to_file(&self.path, text)?;
            // Default is "append + open inline editor" so user
            // sees the freshly-appended text in context and can edit
            // immediately. `append-silent` is the fire-and-forget
            // path for users paste-spamming into the scratchpad.
            // Legacy `append-open` aliases to the new default to
            // keep any frecency / hotkey muscle memory working
            return Ok(match action {
                "append-silent" => Effect::None,
                _ => Effect::EditNote(self.path.clone()),
            });
        }
        anyhow::bail!("invalid scratchpad id: {id}")
    }
}

/// Append `text` to the scratchpad. A blank line precedes each new
/// chunk so separate `scratch ...` calls stay visually distinct in the
/// resulting markdown - no merged paragraphs, no lost context
fn append_to_file(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let prefix = if path.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
        "\n\n"
    } else {
        ""
    };
    writeln!(f, "{prefix}{text}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn provider_in(dir: &Path) -> ScratchpadProvider {
        ScratchpadProvider::new_at(dir.join("scratchpad.md"))
    }

    #[tokio::test]
    async fn keyword_alone_surfaces_open_candidate() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let out = p.query(&Query::new("scratch")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "scratch::open");
    }

    #[tokio::test]
    async fn keyword_with_text_also_offers_append() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let out = p.query(&Query::new("scratch hello there")).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "scratch::open");
        assert!(out[1].id.starts_with("scratch::append::hello there"));
    }

    #[tokio::test]
    async fn alias_scratchpad_works_too() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let out = p.query(&Query::new("scratchpad")).await;
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn unrelated_queries_stay_quiet() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        assert!(p.query(&Query::new("")).await.is_empty());
        assert!(p.query(&Query::new("hello")).await.is_empty());
        // `scratched` is not keyword (no whitespace after the
        // match) - fuzzy-looking substrings mustn't trigger
        assert!(p.query(&Query::new("scratched")).await.is_empty());
    }

    #[tokio::test]
    async fn open_activation_emits_edit_note_effect() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let effect = p.activate(&"scratch::open".into(), "default").await.unwrap();
        match effect {
            Effect::EditNote(path) => {
                assert_eq!(path, dir.path().join("scratchpad.md"));
                assert!(path.exists(), "file must be created on first activation");
            }
            other => panic!("expected EditNote, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn copy_all_returns_file_contents() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        std::fs::write(dir.path().join("scratchpad.md"), "remember this\n").unwrap();
        let effect = p.activate(&"scratch::open".into(), "copy-all").await.unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "remember this\n"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn clear_action_truncates_file() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        std::fs::write(dir.path().join("scratchpad.md"), "junk").unwrap();
        let _ = p.activate(&"scratch::open".into(), "clear").await.unwrap();
        let content = std::fs::read_to_string(dir.path().join("scratchpad.md")).unwrap();
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn reveal_action_points_at_file() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let effect = p.activate(&"scratch::open".into(), "reveal").await.unwrap();
        match effect {
            Effect::RevealInFinder(path) => {
                assert_eq!(path, dir.path().join("scratchpad.md"));
            }
            other => panic!("expected RevealInFinder, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn append_action_writes_to_file_with_separator() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let path = dir.path().join("scratchpad.md");
        std::fs::write(&path, "first").unwrap();
        let _ = p
            .activate(&"scratch::append::second".into(), "default")
            .await
            .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.starts_with("first"),
            "prior content preserved, got {content:?}",
        );
        assert!(content.contains("second"), "appended text landed, got {content:?}");
        assert!(
            content.contains("first\n\n"),
            "blank-line separator between chunks, got {content:?}",
        );
    }

    /// Default append now lands user in the inline editor with the
    /// freshly-appended text in view. This is the "scratchpad should
    /// be editable inline" behaviour - no more silent appends that
    /// leave user wondering where their text went
    #[tokio::test]
    async fn append_default_action_opens_editor_inline() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let effect = p
            .activate(&"scratch::append::a thought".into(), "default")
            .await
            .unwrap();
        match effect {
            Effect::EditNote(path) => {
                assert_eq!(path, dir.path().join("scratchpad.md"));
                let content = std::fs::read_to_string(&path).unwrap();
                assert!(
                    content.contains("a thought"),
                    "text must be appended before the editor opens, got {content:?}",
                );
            }
            other => panic!("expected EditNote (inline edit), got {other:?}"),
        }
    }

    /// `append-silent` is the legacy paste-and-go behaviour. Kept
    /// available so a future hotkey / chain consumer can express
    /// "fire and forget" without surfacing editor
    #[tokio::test]
    async fn append_silent_action_does_not_open_editor() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let path = dir.path().join("scratchpad.md");
        let effect = p
            .activate(&"scratch::append::quiet".into(), "append-silent")
            .await
            .unwrap();
        assert!(matches!(effect, Effect::None), "expected None, got {effect:?}");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("quiet"), "silent variant still appends");
    }

    /// Legacy `append-open` (the OLD secondary action) aliases to the
    /// new default. Anyone with a saved keystroke / hotkey pointing
    /// at `append-open` still gets the inline edit they expected
    #[tokio::test]
    async fn append_open_legacy_alias_still_opens_editor() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let effect = p
            .activate(&"scratch::append::hello".into(), "append-open")
            .await
            .unwrap();
        match effect {
            Effect::EditNote(_) => {}
            other => panic!("expected EditNote, got {other:?}"),
        }
    }

    /// Pin the new action shape so a refactor that reverts the order
    /// (or renames the silent variant) fails test rather than
    /// user's muscle memory
    #[tokio::test]
    async fn append_candidate_advertises_edit_first_silent_second() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let out = p.query(&Query::new("scratch hello")).await;
        let append_row = out.iter().find(|c| c.id.starts_with("scratch::append::"))
            .expect("append row present");
        assert_eq!(append_row.actions.len(), 2);
        assert_eq!(append_row.actions[0].id, "default");
        assert_eq!(append_row.actions[0].label, "Append and Edit");
        assert_eq!(append_row.actions[1].id, "append-silent");
    }

    #[tokio::test]
    async fn unknown_action_errs() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        assert!(p
            .activate(&"scratch::open".into(), "dance-jig")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn foreign_id_refused() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        assert!(p.activate(&"apps::x".into(), "default").await.is_err());
    }

    #[test]
    fn strip_keyword_handles_noise() {
        assert_eq!(strip_keyword("scratch"), Some(""));
        assert_eq!(strip_keyword("scratchpad"), Some(""));
        assert_eq!(strip_keyword("  scratch  "), Some(""));
        assert_eq!(strip_keyword("scratch hi there"), Some("hi there"));
        assert_eq!(strip_keyword("scratchpad  buy milk"), Some("buy milk"));
        assert_eq!(strip_keyword("scratched"), None);
        assert_eq!(strip_keyword("sc"), None);
        assert_eq!(strip_keyword(""), None);
    }

    #[tokio::test]
    async fn append_candidate_subtitle_truncates_long_text() {
        // Subtitles that sprawl past panel width look like garbage
        // - cap the preview at 60 visible chars with an ellipsis so
        // user still sees the start of what they're about to
        // append
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let long = "x".repeat(100);
        let out = p
            .query(&Query::new(format!("scratch {long}")))
            .await;
        assert_eq!(out.len(), 2);
        let sub = out[1].subtitle.as_deref().unwrap_or("");
        assert!(sub.contains("…"), "expected ellipsis in {sub:?}");
    }

    #[tokio::test]
    async fn append_survives_unicode_content() {
        // Emoji in append text - dont truncate mid-grapheme, dont
        // panic. Users will paste all kinds of things in here.
        // Default action now opens editor (changed from
        // Effect::None when the "scratchpad inline edit" UX
        // landed); use the explicit silent variant to keep this
        // test focused on byte-handling, not the effect shape
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        let effect = p
            .activate(
                &"scratch::append::🎉 shipped feature 🎉".into(),
                "append-silent",
            )
            .await
            .unwrap();
        assert!(matches!(effect, Effect::None));
        let content = std::fs::read_to_string(dir.path().join("scratchpad.md")).unwrap();
        assert!(content.contains("🎉 shipped"), "got {content:?}");
    }

    #[tokio::test]
    async fn malformed_append_id_errors_without_panic() {
        let dir = tempdir().unwrap();
        let p = provider_in(dir.path());
        // `scratch::bogus` isn't `scratch::open` and isn't an append
        // shape - must err cleanly
        assert!(p.activate(&"scratch::bogus".into(), "default").await.is_err());
    }
}
