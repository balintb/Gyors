//! Markdown notes provider. Scans a user-configured folder (default
//! `~/Documents/Notes` or `~/Notes`) for `.md` / `.markdown` files, caches
//! them at startup, reindexes on filesystem changes via FSEvents
//!
//! Keyword: `note` / `notes`.
//! - `note` / `notes` -> most-recent notes (up to 20)
//! - `note <filter>` -> substring match on title or filename
//! - `note new <title>` -> create a new note file and open it

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

const MAX_SCAN_DEPTH: usize = 5;
const RESULT_LIMIT: usize = 20;
const TITLE_SCAN_LINES: usize = 20;
/// Cap per-note indexed content so a stray multi-megabyte `.md` can't blow
/// up memory or search latency
pub const MAX_INDEXED_BYTES: usize = 256 * 1024;
/// How many characters of matched content to surface in the subtitle
const SNIPPET_RADIUS: usize = 40;

#[derive(Debug, Clone)]
pub struct Note {
    pub path: PathBuf,
    pub title: String,
    pub mtime: i64,
    /// Bounded (<= `MAX_INDEXED_BYTES`) verbatim file content. Used for
    /// full-text search and to surface matching snippets in the subtitle
    pub content: String,
    /// Pre-computed lowercase variants. The search path used to
    /// recompute these per keystroke (one allocation per note, per
    /// query) - with 1k notes x 256 KB that added up to visible lag
    /// on main-input typing. Caching at scan-time cuts the per-query
    /// cost to zero allocations, at the cost of 2x memory. Worth it
    pub title_lower: String,
    pub filename_lower: String,
    pub content_lower: String,
}

pub struct NotesProvider {
    state: Arc<ArcSwap<Vec<Note>>>,
    notes_folder: PathBuf,
    _watcher: Option<RecommendedWatcher>,
}

impl NotesProvider {
    pub async fn new() -> Self {
        let notes_folder = resolve_notes_folder();
        // Ensure the folder exists so watcher can attach and `note new ...`
        // can write without needing explicit `mkdir` at activation time
        let _ = std::fs::create_dir_all(&notes_folder);
        let initial = {
            let f = notes_folder.clone();
            tokio::task::spawn_blocking(move || scan_notes(&f))
                .await
                .unwrap_or_default()
        };
        let state = Arc::new(ArcSwap::from(Arc::new(initial)));
        let watcher = spawn_watcher(&notes_folder, Arc::clone(&state)).ok();
        Self {
            state,
            notes_folder,
            _watcher: watcher,
        }
    }

    pub fn len(&self) -> usize {
        self.state.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn notes_folder(&self) -> &Path {
        &self.notes_folder
    }
}

#[async_trait]
impl Provider for NotesProvider {
    fn id(&self) -> &str {
        "note"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(parsed) = parse_note_input(pattern) else {
            return vec![];
        };

        match parsed {
            ParsedNoteInput::Create(title) => match create_candidate(&title, &self.notes_folder) {
                Some(c) => vec![c],
                None => vec![],
            },
            ParsedNoteInput::NewNotePrompt => {
                vec![new_note_prompt_candidate()]
            }
            ParsedNoteInput::OpenOrCreate(title) => {
                // Reject paths we can't turn into safe slugs (e.g., just
                // `..`). Returning empty is preferable to silently
                // writing somewhere unexpected
                let Some(slug) = slugify_path(&title) else {
                    return vec![];
                };
                let path = self.notes_folder.join(format!("{slug}.md"));
                let exists = path.exists();
                let mut out = vec![open_or_create_candidate(
                    &title,
                    &path,
                    &self.notes_folder,
                    exists,
                )];

                // Autocomplete below the create/open row. Matches the
                // typed title against filename stems, path segments, and
                // H1 titles of existing notes
                let filter = title.to_lowercase();
                let notes = self.state.load();
                let mut matches: Vec<(i32, &Note)> = notes
                    .iter()
                    .filter(|n| !(exists && n.path == path))
                    .filter_map(|n| {
                        score_hash_autocomplete(n, &filter, &self.notes_folder).map(|s| (s, n))
                    })
                    .collect();
                matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.mtime.cmp(&a.1.mtime)));
                for (_, n) in matches.into_iter().take(RESULT_LIMIT) {
                    out.push(to_candidate(n, &filter, &self.notes_folder));
                }
                out
            }
            ParsedNoteInput::ListAll(filter_raw) => {
                // Same scoring as List, but no RESULT_LIMIT cap.
                // Designed for browsing when user doesn't
                // remember title. Always emits at least the
                // new-note prompt + folder header rows so typing
                // `notes all` never returns an empty list - a bare
                // panel lets unrelated providers' weak fuzzy matches
                // (e.g. System Prefs on "notes all" -> "accounts")
                // surface as if they answered query, which is
                // confusing
                let filter = filter_raw.trim().to_lowercase();
                let notes = self.state.load();
                let scored: Vec<(i32, &Note)> = if filter.is_empty() {
                    notes
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (1_000 - i as i32, n))
                        .collect()
                } else {
                    let mut hits: Vec<(i32, &Note)> = notes
                        .iter()
                        .filter_map(|n| score_note(n, &filter, &self.notes_folder).map(|s| (s, n)))
                        .collect();
                    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.mtime.cmp(&a.1.mtime)));
                    hits
                };
                let note_rows: Vec<Candidate> = scored
                    .into_iter()
                    .map(|(_, n)| to_candidate(n, &filter, &self.notes_folder))
                    .collect();

                let mut out: Vec<Candidate> = Vec::with_capacity(note_rows.len() + 4);
                // Header: always the "new note" prompt so user
                // has a one-keystroke path to create when the list
                // is empty or irrelevant
                out.push(new_note_prompt_candidate());

                if filter.is_empty() {
                    // Folder browse shortcuts only make sense without
                    // a filter (otherwise user has narrowed
                    // past the need for folder-level navigation)
                    for (name, count) in top_level_folders(&notes, &self.notes_folder) {
                        out.push(folder_candidate(&name, count));
                    }
                }

                if note_rows.is_empty() {
                    // No matches - make the emptiness visible instead
                    // of silently letting other providers fill the
                    // dropdown. If the filter is set, say so; else
                    // it's a fresh folder and we nudge toward creation
                    out.push(empty_list_all_candidate(&filter));
                } else {
                    out.extend(note_rows);
                }
                out
            }
            ParsedNoteInput::SearchContent(query_raw) => {
                let filter = query_raw.trim().to_lowercase();
                if filter.is_empty() {
                    return vec![];
                }
                let notes = self.state.load();
                let mut hits: Vec<(i32, &Note)> = notes
                    .iter()
                    .filter_map(|n| score_note_content(n, &filter).map(|s| (s, n)))
                    .collect();
                hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.mtime.cmp(&a.1.mtime)));
                if hits.is_empty() {
                    return vec![no_content_match_candidate(&filter)];
                }
                hits.into_iter()
                    .map(|(_, n)| to_candidate(n, &filter, &self.notes_folder))
                    .collect()
            }
            ParsedNoteInput::List(filter_raw) => {
                let filter = filter_raw.trim().to_lowercase();
                let filter_trimmed = filter_raw.trim();
                let notes = self.state.load();
                let mut scored: Vec<(i32, &Note)> = if filter.is_empty() {
                    notes
                        .iter()
                        .take(RESULT_LIMIT)
                        .enumerate()
                        .map(|(i, n)| (1_000 - i as i32, n))
                        .collect()
                } else {
                    let mut hits: Vec<(i32, &Note)> = notes
                        .iter()
                        .filter_map(|n| score_note(n, &filter, &self.notes_folder).map(|s| (s, n)))
                        .collect();
                    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.mtime.cmp(&a.1.mtime)));
                    hits.into_iter().take(RESULT_LIMIT).collect()
                };
                let mut out: Vec<Candidate> = scored
                    .drain(..)
                    .map(|(_, n)| to_candidate(n, &filter, &self.notes_folder))
                    .collect();
                if filter.is_empty() {
                    // Header rows for the bare list: prompt + one row per
                    // top-level folder so users can drill into subfolders
                    // without typing `#folder/` manually
                    let folders = top_level_folders(&notes, &self.notes_folder);
                    let mut header = Vec::with_capacity(1 + folders.len());
                    header.push(new_note_prompt_candidate());
                    for (name, count) in &folders {
                        header.push(folder_candidate(name, *count));
                    }
                    header.extend(out);
                    out = header;
                } else if out.is_empty() {
                    if let Some(c) = create_candidate(filter_trimmed, &self.notes_folder) {
                        out.push(c);
                    }
                }
                out
            }
        }
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        if id == NEW_NOTE_PROMPT_ID {
            return Ok(Effect::SetInput("newnote ".into()));
        }
        // `findnote` returned no matches - row is informational,
        // pressing Enter just closes the quick prompt without side effects
        if id.starts_with("note::findnote::empty::") {
            return Ok(Effect::None);
        }
        // `notes all` with no results - same treatment
        if id == "note::list-empty" {
            return Ok(Effect::None);
        }
        // Folder-browse row - activates into the `#<folder>/` view which
        // shares code with the hash autocomplete path
        if let Some(name) = id.strip_prefix("note::folder::") {
            return Ok(Effect::SetInput(format!("#{name}/")));
        }
        // `#<title>` open-or-create row - re-checks filesystem at
        // activate time in case file was created or deleted between
        // query and Enter. `slugify_path` handles nested folder syntax
        // (`work/meeting`) and rejects `..` components so we can't
        // accidentally write outside the notes root
        if let Some(title) = id.strip_prefix("note::openOrCreate::") {
            let slug = slugify_path(title)
                .ok_or_else(|| anyhow::anyhow!("invalid note title: {title:?}"))?;
            let path = self.notes_folder.join(format!("{slug}.md"));
            // Cheap lexical pre-check rejects `..`
            // escapes before any filesystem op. The post-mkdir
            // canonical check below catches symlink escapes that
            // the lexical pass can't see
            ensure_in_root(&path, &self.notes_folder)?;
            if !path.exists() {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                ensure_real_path_in_root(&path, &self.notes_folder)?;
                let h1 = last_segment(title);
                std::fs::write(&path, format!("# {h1}\n\n"))?;
            } else {
                // Existing note - still verify it lives under the
                // real notes folder. A pre-existing symlink could
                // redirect Edit to e.g. ~/.ssh/config
                ensure_real_path_in_root(&path, &self.notes_folder)?;
            }
            return Ok(Effect::EditNote(path));
        }
        if let Some(title) = id.strip_prefix("note::create::") {
            let slug = slugify_path(title)
                .ok_or_else(|| anyhow::anyhow!("invalid note title: {title:?}"))?;
            let path = self.notes_folder.join(format!("{slug}.md"));
            ensure_in_root(&path, &self.notes_folder)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            ensure_real_path_in_root(&path, &self.notes_folder)?;
            if !path.exists() {
                let h1 = last_segment(title);
                std::fs::write(&path, format!("# {h1}\n\n"))?;
            }
            return Ok(Effect::EditNote(path));
        }

        let path_s = id
            .strip_prefix("note::")
            .ok_or_else(|| anyhow::anyhow!("invalid note candidate id: {id}"))?;
        let path = PathBuf::from(path_s);
        note_action_effect(&path, action)
    }
}

/// Single dispatch table for note-targeted actions. Shared between
/// direct activation (row -> Enter or -> action-menu -> Enter) and chain
/// activation (`note <filter> > <action>`), so every action handler
/// lives in exactly one place
fn note_action_effect(path: &Path, action: &str) -> Result<Effect> {
    let path_s = path.to_string_lossy().to_string();
    match action {
        "default" => Ok(Effect::EditNote(path.to_path_buf())),
        "preview" => {
            // Read at activate time - file may have changed
            // since the last scan. Empty file on disk is legal; only
            // a missing file is a hard error
            let rendered = std::fs::read_to_string(path).unwrap_or_default();
            let label = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("note")
                .to_string();
            Ok(Effect::ShowText {
                text: rendered,
                label,
                language: Some("markdown".into()),
                // Carry path through so preview UI can rebind
                // Enter to "open in editor" - natural next action
                // after glancing at a note
                editable_path: Some(path.to_path_buf()),
            })
        }
        "open-external" => Ok(Effect::OpenPath(path.to_path_buf())),
        "reveal" => Ok(Effect::RevealInFinder(path.to_path_buf())),
        "copy-path" => Ok(Effect::CopyToClipboard(path_s)),
        "copy-content" => {
            let content = std::fs::read_to_string(path)?;
            Ok(Effect::CopyToClipboard(content))
        }
        "duplicate" => {
            let new_path = duplicate_note(path)?;
            Ok(Effect::EditNote(new_path))
        }
        "trash" => Ok(Effect::TrashFile(path.to_path_buf())),
        other => anyhow::bail!("unknown action for note: {other}"),
    }
}

/// Stable id for the "+ New note" discoverability row. Also used by the
/// cmdN shortcut (which just re-activates this id) so both entry points
/// share behaviour and tests
pub const NEW_NOTE_PROMPT_ID: &str = "note::prompt-new";

/// All keywords accepted in place of canonical `note` / `notes`.
/// Kept centralised so both parser and `hints.rs` stay in sync
const NOTE_LIST_KEYWORDS: &[&str] = &["note", "notes", "n"];
/// Keywords that create a note outright: `newnote <title>` / `nn <title>`
const NEW_NOTE_KEYWORDS: &[&str] = &["newnote", "nn"];

/// Keywords that search inside note bodies. Distinct from the list
/// keywords because scoring differs: `findnote` ranks content hits
/// first (the user is looking for *what* the note says, not a title
/// they already know)
const FIND_NOTE_KEYWORDS: &[&str] = &["findnote", "searchnotes", "searchnote"];

/// What user is asking for when their input begins with a note keyword.
/// Values are owned so lifetimes dont leak up into query method - a
/// trivial allocation per keystroke, which is dwarfed by actual search
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ParsedNoteInput {
    /// `note` / `notes` / `n` / `note <filter>` / ...
    /// Capped at `RESULT_LIMIT` rows; use `ListAll` for the full view
    List(String),
    /// `note all` / `notes all` / `note all <filter>` - same as List
    /// but uncapped, for browsing large notes folders where user
    /// doesn't remember title well enough to narrow
    ListAll(String),
    /// `findnote <query>` / `searchnotes <query>` - content-first
    /// search: ranks body matches ahead of title matches, surfaces
    /// matching snippet prominently in the subtitle
    SearchContent(String),
    /// `note new <title>` / `newnote <title>` / `nn <title>`
    Create(String),
    /// Bare `newnote` / `nn` / `note new` - prime input so user can
    /// type a title without leaving the launcher
    NewNotePrompt,
    /// `#<title>` shorthand. Opens an existing note with that slug if one
    /// exists, otherwise creates it. Leading whitespace after `#` is
    /// trimmed (`#  hello` == `#hello`)
    OpenOrCreate(String),
}

pub fn parse_note_input(s: &str) -> Option<ParsedNoteInput> {
    // `#<title>` quick-create shorthand
    if let Some(rest) = s.strip_prefix('#') {
        let title = rest.trim();
        if title.is_empty() {
            return Some(ParsedNoteInput::NewNotePrompt);
        }
        return Some(ParsedNoteInput::OpenOrCreate(title.to_string()));
    }

    // `findnote` / `searchnotes` - content search. Bare keyword is
    // treated as "list all" so user sees something useful while
    // thinking about what to type
    for kw in FIND_NOTE_KEYWORDS {
        if s == *kw {
            return Some(ParsedNoteInput::ListAll(String::new()));
        }
        let with_space = format!("{kw} ");
        if let Some(rest) = s.strip_prefix(&with_space) {
            let q = rest.trim();
            return Some(if q.is_empty() {
                ParsedNoteInput::ListAll(String::new())
            } else {
                ParsedNoteInput::SearchContent(q.to_string())
            });
        }
    }

    // `newnote` / `nn` family - one-shot create path
    for kw in NEW_NOTE_KEYWORDS {
        if s == *kw {
            return Some(ParsedNoteInput::NewNotePrompt);
        }
        let with_space = format!("{kw} ");
        if let Some(rest) = s.strip_prefix(&with_space) {
            let title = rest.trim();
            return Some(if title.is_empty() {
                ParsedNoteInput::NewNotePrompt
            } else {
                ParsedNoteInput::Create(title.to_string())
            });
        }
    }

    // `note` / `notes` / `n` list family
    let mut rest: Option<&str> = None;
    for kw in NOTE_LIST_KEYWORDS {
        if s == *kw {
            rest = Some("");
            break;
        }
        let with_space = format!("{kw} ");
        if let Some(r) = s.strip_prefix(&with_space) {
            rest = Some(r);
            break;
        }
    }
    let rest = rest?;

    if rest == "new" {
        return Some(ParsedNoteInput::NewNotePrompt);
    }
    if let Some(title) = rest.strip_prefix("new ") {
        let title = title.trim();
        return Some(if title.is_empty() {
            ParsedNoteInput::NewNotePrompt
        } else {
            ParsedNoteInput::Create(title.to_string())
        });
    }
    // `note all` / `notes all [filter]` - bypass the RESULT_LIMIT cap
    if rest == "all" {
        return Some(ParsedNoteInput::ListAll(String::new()));
    }
    if let Some(filter) = rest.strip_prefix("all ") {
        return Some(ParsedNoteInput::ListAll(filter.trim().to_string()));
    }
    Some(ParsedNoteInput::List(rest.to_string()))
}

/// Back-compat shim for the handful of tests that only need the raw
/// filter string from a list-mode parse
#[cfg(test)]
fn parse_note_keyword(s: &str) -> Option<String> {
    match parse_note_input(s)? {
        ParsedNoteInput::List(r) => Some(r),
        _ => None,
    }
}

/// Return a score for how strongly `filter_lower` matches the note, or None
/// if it doesn't match at all. Higher scores win. Tiers roughly mirror user
/// intent: "I'm looking for a note named X" > "...with X in its filename" >
/// "...in this folder" > "...that mentions X somewhere inside."
pub fn score_note(n: &Note, filter_lower: &str, notes_folder: &Path) -> Option<i32> {
    // Hot path: keep all work against pre-lowercased fields cached in
    // `Note` at scan time. Avoids per-keystroke allocation + lowercase
    // pass over every note's body (which hurt for users with hundreds
    // of notes)
    let title_lower = &n.title_lower;
    let filename_lower = &n.filename_lower;
    let stem_lower = n
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let rel_lower = display_relative(&n.path, notes_folder).to_lowercase();
    let content_lower = &n.content_lower;

    let mut score = 0;
    let mut hit = false;
    if title_lower == filter_lower {
        score += 1000;
        hit = true;
    } else if title_lower.starts_with(filter_lower) {
        score += 700;
        hit = true;
    } else if title_lower.contains(filter_lower) {
        score += 500;
        hit = true;
    }
    if filename_lower.contains(filter_lower) {
        score += 300;
        hit = true;
    }
    // Folder-path match - `n work/meet` finds notes inside `work/` even
    // when title doesn't contain "work"
    if rel_lower != *filename_lower && rel_lower.contains(filter_lower) {
        score += 200;
        hit = true;
    }
    if let Some(pos) = content_lower.find(filter_lower) {
        let position_bonus = (100i32).saturating_sub(pos as i32 / 80);
        score += 100 + position_bonus.max(0);
        hit = true;
    }
    // Fuzzy fallback: when the filter doesn't appear verbatim but is
    // close enough to *any token* of title, stem, or filename,
    // treat it as a hit. Previously this measured edit distance
    // against the whole string only - fine for `helo` vs `hello.md`
    // but a false negative for `helo` vs `hello-world.md` (edit
    // distance to the whole "hello-world" stem is 7). Tokenising
    // catches the common case of a typo matching one word of a
    // multi-word title
    if !hit && filter_lower.len() >= 3 {
        let max_d: usize = if filter_lower.len() <= 4 { 1 } else { 2 };
        let mut d = usize::MAX;
        // Whole strings first - always useful for short inputs
        if !stem_lower.is_empty() {
            d = d.min(strsim::levenshtein(filter_lower, &stem_lower));
        }
        if !title_lower.is_empty() {
            d = d.min(strsim::levenshtein(filter_lower, title_lower));
        }
        // Then each token. Splits on whitespace *and* non-alnum so
        // "hello-world.md" / "hello_world.md" / "hello world" all
        // tokenise to ["hello", "world"]
        for token in tokens(title_lower)
            .chain(tokens(&stem_lower))
            .chain(tokens(filename_lower))
        {
            if token.is_empty() || token.len() < 3 {
                continue;
            }
            d = d.min(strsim::levenshtein(filter_lower, token));
            if d == 0 {
                break;
            } // exact token match - can't improve
        }
        if d > 0 && d <= max_d {
            // Score between 80..150 - above pure content hits, below
            // every substring match so exact matches always win
            let bonus = 150_i32.saturating_sub((d as i32) * 40);
            score += bonus.max(0);
            hit = true;
        }
    }
    if hit {
        Some(score)
    } else {
        None
    }
}

/// Split a lowercased string into alphanumeric tokens. Delimiters
/// are anything non-alphanumeric (spaces, dashes, shows,
/// dots...). Used by the fuzzy fallback so a typo like "helo" matches
/// "hello-world.md" via its "hello" token
fn tokens(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
}

/// Content-first scoring for `findnote <query>`. Title / filename /
/// folder matches still count (a user who types a query that happens
/// to match both content and title probably wanted both surfaced),
/// but *body matches dominate*: the scoring weights are inverted
/// relative to `score_note` so a mentions query wins
/// over a note whose title merely contains it
pub fn score_note_content(n: &Note, filter_lower: &str) -> Option<i32> {
    // Hot path: use the scan-time cache, same reasoning as `score_note`
    let title_lower = &n.title_lower;
    let filename_lower = &n.filename_lower;
    let content_lower = &n.content_lower;

    let mut score = 0;
    let mut hit = false;

    // Content match is the primary signal here. Bonus the earliest
    // position so a match at top of the note ranks above one
    // buried pages in (which is where matching snippet will
    // show up in row subtitle anyway)
    if let Some(pos) = content_lower.find(filter_lower) {
        let position_bonus = (200i32).saturating_sub(pos as i32 / 40);
        score += 1_000 + position_bonus.max(0);
        hit = true;
        // Frequency boost: a mentions the term five times
        // is almost certainly *about* it. Count cheap matches, cap
        // at a sane ceiling so a pathological repetition doesn't
        // dominate. Weighted high (relative to title bonuses below)
        // so a note mentioning the term four times beats a note that
        // merely has it in the H1 once
        let occurrences = content_lower.matches(filter_lower).count().min(20) as i32;
        score += occurrences * 40;
    }

    // Title / filename hits are *small* bonuses on top of content
    // matches - a pure title hit never beats a body-rich hit, since
    // user's intent with `findnote` is body search
    if title_lower.contains(filter_lower) {
        score += 60;
        hit = true;
    }
    if filename_lower.contains(filter_lower) {
        score += 30;
        hit = true;
    }

    if hit {
        Some(score)
    } else {
        None
    }
}

fn empty_list_all_candidate(filter: &str) -> Candidate {
    let (title, subtitle) = if filter.is_empty() {
        (
            "No notes yet".to_string(),
            "Press ↵ above to create your first note, or type `newnote <title>`".to_string(),
        )
    } else {
        (
            format!("No notes match \"{filter}\""),
            "Try a shorter filter, or `findnote` to search inside bodies".to_string(),
        )
    };
    Candidate {
        id: "note::list-empty".into(),
        title,
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol("tray".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn no_content_match_candidate(query: &str) -> Candidate {
    Candidate {
        id: format!("note::findnote::empty::{query}"),
        title: format!("No notes contain \"{query}\""),
        subtitle: Some("Try a different query, or `notes all` to browse".into()),
        icon: Icon::SfSymbol("magnifyingglass".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("OK")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Scoring for the `#<title>` autocomplete suggestions - deliberately
/// narrower than the general `score_note` used by `note <filter>` search
///
/// `#` is a quick-create gesture, so we only want suggestions that look
/// like they might be same note user's about to type (filename,
/// folder, or title match). Content matches would bury the create row
/// under unrelated notes that happen to mention the substring
pub fn score_hash_autocomplete(n: &Note, filter_lower: &str, notes_folder: &Path) -> Option<i32> {
    if filter_lower.is_empty() {
        return None;
    }
    let filename_lower = n
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let stem_lower = n
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let rel_lower = display_relative(&n.path, notes_folder).to_lowercase();
    let rel_stem_lower = rel_lower
        .strip_suffix(".md")
        .unwrap_or(&rel_lower)
        .to_string();
    let title_lower = n.title.to_lowercase();

    // Exact relative-path match (e.g., `#work/meeting` -> `work/meeting.md`)
    if rel_stem_lower == filter_lower {
        return Some(950);
    }
    if stem_lower == filter_lower {
        return Some(900);
    }
    // Path prefix: user types `#work/` or `#work/mee`
    if rel_stem_lower.starts_with(filter_lower) {
        return Some(800);
    }
    if stem_lower.starts_with(filter_lower) {
        return Some(700);
    }
    if filename_lower.contains(filter_lower) {
        return Some(500);
    }
    // Any segment of the relative path matched - e.g., `#meet` finds
    // `work/meetings/q1.md` via the "meetings" folder
    if rel_lower.contains(filter_lower) {
        return Some(450);
    }
    if title_lower.starts_with(filter_lower) {
        return Some(400);
    }
    if title_lower.contains(filter_lower) {
        return Some(250);
    }
    None
}

/// Produce a short, single-line content snippet centered on the first
/// occurrence of `filter_lower`. Returns `None` if the filter is empty or
/// absent from the content
pub fn content_snippet(n: &Note, filter_lower: &str) -> Option<String> {
    if filter_lower.is_empty() {
        return None;
    }
    let content_lower = n.content.to_lowercase();
    let pos = content_lower.find(filter_lower)?;
    // Walk back to the nearest char boundary up to SNIPPET_RADIUS bytes
    // before the match, then forward past it. Use char_indices to avoid
    // slicing through a multi-byte UTF-8 boundary
    let start = n.content[..pos]
        .char_indices()
        .rev()
        .take(SNIPPET_RADIUS)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end_target = pos + filter_lower.len() + SNIPPET_RADIUS;
    let end = n.content[pos..]
        .char_indices()
        .take_while(|(i, _)| pos + i < end_target)
        .last()
        .map(|(i, c)| pos + i + c.len_utf8())
        .unwrap_or(n.content.len());
    let slice = &n.content[start..end.min(n.content.len())];
    // Collapse newlines/tabs so it stays on one line in the UI
    let flat: String = slice
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut out = flat.trim().to_string();
    if start > 0 {
        out = format!("…{out}");
    }
    if end < n.content.len() {
        out.push('…');
    }
    Some(out)
}

fn to_candidate(n: &Note, filter_lower: &str, notes_folder: &Path) -> Candidate {
    let filename_text = n
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let rel = display_relative(&n.path, notes_folder);
    // Show matching content snippet when the filter hit body; fall
    // back to the relative path so users see what folder a note lives in
    // without a wall of absolute-path noise
    let subtitle = match content_snippet(n, filter_lower) {
        Some(s) => s,
        None => rel.clone(),
    };
    let search_text = format!(
        "{} {} {} {} {}",
        n.title, n.title, filename_text, rel, n.content
    );
    Candidate {
        id: format!("note::{}", n.path.display()),
        title: n.title.clone(),
        subtitle: Some(subtitle),
        icon: Icon::SfSymbol("doc.text.fill".into()),
        kind: CandidateKind::Custom("note".into()),
        actions: vec![
            Action::primary("Edit"),
            // Right-arrow shortcut: the ViewModel's `enterActionsMode`
            // special-cases the `preview` action id and jumps straight
            // to the inline preview, skipping actions list. Lets a
            // single -> render the note without opening editor or
            // the multi-action menu
            Action::new("preview", "Preview"),
            Action::new("open-external", "Open in External App"),
            Action::new("reveal", "Reveal in Finder"),
            Action::new("duplicate", "Duplicate"),
            Action::new("copy-path", "Copy Path"),
            Action::new("copy-content", "Copy Content"),
            Action::new("trash", "Move to Trash"),
        ],
        search_text,
        bypass_rank: true,
    }
}

fn create_candidate(title: &str, folder: &Path) -> Option<Candidate> {
    let slug = slugify_path(title)?;
    let path = folder.join(format!("{slug}.md"));
    let rel = display_relative(&path, folder);
    Some(Candidate {
        id: format!("note::create::{title}"),
        title: format!("Create note: {title}"),
        subtitle: Some(rel),
        icon: Icon::SfSymbol("square.and.pencil".into()),
        kind: CandidateKind::Custom("note-new".into()),
        actions: vec![Action::primary("Create & Open")],
        search_text: String::new(),
        bypass_rank: true,
    })
}

/// Walk the scanned notes and collect top-level subfolder names with the
/// count of notes inside each. Sorted alphabetically for stable ordering.
/// Only folders that actually contain notes are surfaced - empty folders
/// stay invisible until they have content
fn top_level_folders(notes: &[Note], root: &Path) -> Vec<(String, usize)> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for n in notes {
        let Ok(rel) = n.path.strip_prefix(root) else {
            continue;
        };
        let components: Vec<_> = rel.components().collect();
        // Must be at least <folder>/<file.md> - i.e. two components
        if components.len() < 2 {
            continue;
        }
        let Some(name) = components[0].as_os_str().to_str() else {
            continue;
        };
        *counts.entry(name.to_string()).or_insert(0) += 1;
    }
    counts.into_iter().collect()
}

/// Row representing a subfolder. Activating it primes `#<name>/` so the
/// user drops straight into that folder's autocomplete view (which
/// already lists its notes under a create row scoped to the folder)
fn folder_candidate(name: &str, count: usize) -> Candidate {
    let plural = if count == 1 { "note" } else { "notes" };
    Candidate {
        id: format!("note::folder::{name}"),
        title: format!("{name}/"),
        subtitle: Some(format!("{count} {plural}  ·  browse folder")),
        icon: Icon::SfSymbol("folder.fill".into()),
        kind: CandidateKind::Custom("note-folder".into()),
        actions: vec![Action::primary("Browse")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Discoverable entry point: shown at top of the note list when no
/// filter is active, and targeted by the cmdN shortcut. Activating it
/// primes input with `newnote ` so user can type a title and
/// press Enter to create
fn new_note_prompt_candidate() -> Candidate {
    Candidate {
        id: NEW_NOTE_PROMPT_ID.into(),
        title: "New note…".into(),
        subtitle: Some("Type a title, then press Enter  (⌘N)".into()),
        icon: Icon::SfSymbol("plus.square.fill".into()),
        kind: CandidateKind::Custom("note-new".into()),
        actions: vec![Action::primary("New Note")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// `#<title>` row. Label flips between "Open" and "Create" based on
/// whether the slugged path already exists, so user knows what's
/// about to happen. Subtitle is the relative path inside the notes
/// folder, so user sees where file lives / will live
fn open_or_create_candidate(
    title: &str,
    path: &Path,
    notes_folder: &Path,
    exists: bool,
) -> Candidate {
    let label = if exists {
        format!("Open note: {title}")
    } else {
        format!("Create note: {title}")
    };
    let symbol = if exists {
        "doc.text.fill"
    } else {
        "plus.square.fill"
    };
    Candidate {
        id: format!("note::openOrCreate::{title}"),
        title: label,
        subtitle: Some(display_relative(path, notes_folder)),
        icon: Icon::SfSymbol(symbol.into()),
        kind: CandidateKind::Custom(if exists {
            "note".into()
        } else {
            "note-new".into()
        }),
        actions: vec![Action::primary(if exists {
            "Open"
        } else {
            "Create & Open"
        })],
        search_text: String::new(),
        bypass_rank: true,
    }
}

/// Last `/`-separated segment of a title, used as the `# <h1>` seed when
/// creating a note at a nested path (so `newnote work/sprint` writes
/// `# sprint` as the H1, not `# work/sprint`)
fn last_segment(title: &str) -> &str {
    title
        .rsplit_once('/')
        .map(|(_, last)| last.trim())
        .unwrap_or(title.trim())
}

/// Cheap lexical containment check. Resolves `..` / `.` textually
/// (no filesystem access) and rejects paths that would land outside
/// `root` without any further work. Use this BEFORE mkdir-p as a
/// fast pre-filter against obvious bad slugs
///
/// Does NOT defend against symlink escapes. if a symlink
/// inside the notes folder points outside it, lexical normalisation
/// is fooled. Call [`ensure_real_path_in_root`] AFTER the parent
/// exists for actual defence
fn ensure_in_root(path: &Path, root: &Path) -> Result<()> {
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let abs_root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let normalized = lexical_normalize(&abs_path);
    if !normalized.starts_with(&abs_root) {
        anyhow::bail!(
            "note path {} escapes notes folder {}",
            normalized.display(),
            abs_root.display()
        );
    }
    Ok(())
}

/// Real-path containment check using `canonicalize()`.
/// Resolves every symlink in `path.parent()` and asserts result
/// stays under `canonicalize(root)`. Call AFTER `create_dir_all` so
/// the parent exists (canonicalize requires that)
///
/// Why parent and not `path` itself: path's filename may not
/// exist yet (we're about to create it). Canonicalizing the parent
/// covers every symlink in the chain that COULD redirect the write
fn ensure_real_path_in_root(path: &Path, root: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("note path has no parent: {}", path.display()))?;
    let canon_parent = parent.canonicalize().with_context(|| {
        format!(
            "canonicalize note parent {} (must exist - did mkdir-p run?)",
            parent.display()
        )
    })?;
    let canon_root = root
        .canonicalize()
        .with_context(|| format!("canonicalize notes root {}", root.display()))?;
    if !canon_parent.starts_with(&canon_root) {
        anyhow::bail!(
            "note path {} escapes notes folder {} via symlink \
             (canonical parent {}, canonical root {})",
            path.display(),
            root.display(),
            canon_parent.display(),
            canon_root.display()
        );
    }
    Ok(())
}

/// Resolve `.` and `..` components lexically (no filesystem access) so
/// we can sanity-check paths before any file is created
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Copy `src` to a sibling path with a " copy" / " copy N" suffix so the
/// duplicate never clobbers an existing file. Returns the new path
pub fn duplicate_note(src: &Path) -> Result<PathBuf> {
    let parent = src
        .parent()
        .ok_or_else(|| anyhow::anyhow!("note has no parent dir"))?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("note has no file stem"))?;
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("md");
    // Strip any existing " copy" / " copy N" suffix so repeated duplicates
    // produce "foo copy", "foo copy 2", "foo copy 3" rather than
    // "foo copy copy copy"
    let base_stem = strip_copy_suffix(stem);
    for n in 1..=1000 {
        let candidate_stem = if n == 1 {
            format!("{base_stem} copy")
        } else {
            format!("{base_stem} copy {n}")
        };
        let dest = parent.join(format!("{candidate_stem}.{ext}"));
        if !dest.exists() {
            std::fs::copy(src, &dest)?;
            return Ok(dest);
        }
    }
    anyhow::bail!(
        "couldn't find a free name for duplicate of {}",
        src.display()
    )
}

fn strip_copy_suffix(stem: &str) -> String {
    if let Some(rest) = stem.strip_suffix(" copy") {
        return rest.to_string();
    }
    if let Some(idx) = stem.rfind(" copy ") {
        let tail = &stem[idx + " copy ".len()..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            return stem[..idx].to_string();
        }
    }
    stem.to_string()
}

pub fn resolve_notes_folder() -> PathBuf {
    // Route through `config::config_path()` so `GYORS_CONFIG_DIR`
    // override works here too - otherwise tests that redirect the
    // config still scan the developer's real `~/Documents/Gyors`
    let config_path = crate::config::config_path();
    if let Ok(text) = std::fs::read_to_string(&config_path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(folder) = value.get("notes_folder").and_then(|v| v.as_str()) {
                return expand_tilde(folder);
            }
        }
    }
    // Default: ~/Documents/Gyors. Caller creates this on first run so
    // watcher can attach and `note new ...` always has somewhere to land
    if let Some(home) = dirs::home_dir() {
        return home.join("Documents").join("Gyors");
    }
    PathBuf::from("Gyors")
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn scan_notes(root: &Path) -> Vec<Note> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    scan_dir(root, &mut out, 0);
    out.sort_by_key(|n| std::cmp::Reverse(n.mtime));
    out
}

fn scan_dir(dir: &Path, out: &mut Vec<Note>, depth: usize) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            scan_dir(&path, out, depth + 1);
            continue;
        }
        let is_md = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
            .unwrap_or(false);
        if !is_md {
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let content = read_bounded(&path);
        let title = title_from_content(&content).unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        });
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let title_lower = title.to_lowercase();
        let filename_lower = filename.to_lowercase();
        let content_lower = content.to_lowercase();
        out.push(Note {
            path,
            title,
            mtime,
            content,
            title_lower,
            filename_lower,
            content_lower,
        });
    }
}

/// Read up to `MAX_INDEXED_BYTES` of file as UTF-8. If byte cap
/// lands mid-codepoint, truncate down to the nearest valid char boundary
pub fn read_bounded(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let take = bytes.len().min(MAX_INDEXED_BYTES);
    match std::str::from_utf8(&bytes[..take]) {
        Ok(s) => s.to_string(),
        Err(e) => {
            let valid = e.valid_up_to();
            std::str::from_utf8(&bytes[..valid])
                .unwrap_or("")
                .to_string()
        }
    }
}

/// Extract an H1 ("# Title") from already-loaded content. Kept public for
/// tests
pub fn title_from_content(content: &str) -> Option<String> {
    for line in content.lines().take(TITLE_SCAN_LINES) {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("# ") {
            let title = rest.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut last_dash = false;
    for c in title.chars() {
        if c.is_alphanumeric() {
            for low in c.to_lowercase() {
                out.push(low);
            }
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Slugify a title that may contain `/` path separators - each segment is
/// slugified independently so folder structure is preserved
///
/// `"Work/Meeting Notes"` -> `"work/meeting-notes"`
///
/// Empty segments (`a//b`), `.` components, and `..` components are
/// stripped, so users can't escape the notes root (`..` -> rejected,
/// final slug might end up empty -> returns `None`)
///
/// Returns `None` for inputs that produce no usable slug (pure
/// whitespace, only reserved components, empty title, etc.) so callers
/// can cheaply early-out without synthesising garbage paths
pub fn slugify_path(raw: &str) -> Option<String> {
    let cleaned = raw.trim().trim_matches('/');
    let segments: Vec<String> = cleaned
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .map(slugify)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// Display path for a note relative to the notes folder. Used in
/// subtitles so users see `work/meetings/q1.md` instead of the full
/// absolute path, making folder location immediately obvious
pub fn display_relative(note_path: &Path, notes_folder: &Path) -> String {
    match note_path.strip_prefix(notes_folder) {
        Ok(rel) => rel.display().to_string(),
        Err(_) => note_path.display().to_string(),
    }
}

fn spawn_watcher(root: &Path, state: Arc<ArcSwap<Vec<Note>>>) -> Result<RecommendedWatcher> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    if root.exists() {
        let _ = watcher.watch(root, RecursiveMode::Recursive);
    }
    let root = root.to_path_buf();
    std::thread::spawn(move || loop {
        if rx.recv().is_err() {
            return;
        }
        // Debounce a short burst of events
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        let fresh = scan_notes(&root);
        tracing::debug!("notes reindexed: {} entries", fresh.len());
        state.store(Arc::new(fresh));
    });
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_test_provider(notes: Vec<Note>, folder: PathBuf) -> NotesProvider {
        NotesProvider {
            state: Arc::new(ArcSwap::from(Arc::new(notes))),
            notes_folder: folder,
            _watcher: None,
        }
    }

    fn write_note(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    fn note(path: PathBuf, title: &str, mtime: i64, content: &str) -> Note {
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let title_lower = title.to_lowercase();
        let filename_lower = filename.to_lowercase();
        let content_lower = content.to_lowercase();
        Note {
            path,
            title: title.into(),
            mtime,
            content: content.into(),
            title_lower,
            filename_lower,
            content_lower,
        }
    }


    #[tokio::test]
    async fn no_match_without_keyword() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        assert!(p.query(&Query::new("hello")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_lists_notes_with_prompt_row_first() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("a.md"), "a", 10, ""),
                note(td.path().join("b.md"), "b", 20, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note")).await;
        assert_eq!(out.len(), 3, "2 notes + 1 prompt row");
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID, "prompt row is first");
    }

    #[tokio::test]
    async fn bare_keyword_with_no_notes_still_shows_prompt_row() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("note")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);
    }

    #[tokio::test]
    async fn notes_alias_works() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("a.md"), "a", 0, "")],
            td.path().to_path_buf(),
        );
        // 1 note + 1 prompt row
        assert_eq!(p.query(&Query::new("notes")).await.len(), 2);
        assert_eq!(p.query(&Query::new("notes ")).await.len(), 2);
    }

    #[tokio::test]
    async fn filtered_list_has_no_prompt_row() {
        // Once user starts filtering, the "+ New note" row would just
        // clutter narrow searches - it only appears in the empty-filter
        // list view
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("a.md"), "Alpha", 0, "")],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note alpha")).await;
        assert_eq!(out.len(), 1);
        assert!(out.iter().all(|c| c.id != NEW_NOTE_PROMPT_ID));
    }

    #[tokio::test]
    async fn bare_newnote_yields_prompt_row() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("newnote")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);

        let out = p.query(&Query::new("newnote ")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);
    }

    #[tokio::test]
    async fn newnote_with_title_yields_create_candidate() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("newnote meeting notes")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("note::create::"));
        assert!(out[0].title.contains("meeting notes"));
    }

    #[tokio::test]
    async fn newnote_alias_matches_note_new_behaviour() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let via_alias = p.query(&Query::new("newnote shopping")).await;
        let via_long = p.query(&Query::new("note new shopping")).await;
        assert_eq!(via_alias.len(), 1);
        assert_eq!(via_long.len(), 1);
        assert_eq!(via_alias[0].id, via_long[0].id);
    }

    #[tokio::test]
    async fn note_new_without_title_yields_prompt_row() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("note new ")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);
    }

    #[tokio::test]
    async fn activate_prompt_row_primes_input() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&NEW_NOTE_PROMPT_ID.to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::SetInput(s) => assert_eq!(s, "newnote "),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }


    #[tokio::test]
    async fn n_alias_bare_lists_notes_plus_prompt() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("a.md"), "Alpha", 0, "")],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("n")).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);
    }

    #[tokio::test]
    async fn n_alias_filters_notes() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("a.md"), "Alpha", 0, ""),
                note(td.path().join("b.md"), "Beta", 0, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("n alph")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Alpha");
    }

    #[tokio::test]
    async fn nn_alias_is_prompt_when_bare() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("nn")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);
    }

    #[tokio::test]
    async fn nn_alias_with_title_creates() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("nn shopping")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("note::create::"));
    }


    #[tokio::test]
    async fn hash_shorthand_shows_create_when_file_missing() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("#hello")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "note::openOrCreate::hello");
        assert!(
            out[0].title.contains("Create note"),
            "title was {:?}",
            out[0].title
        );
    }

    #[tokio::test]
    async fn hash_shorthand_shows_open_when_file_exists() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("hello.md");
        fs::write(&path, "# hello\n").unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("#hello")).await;
        assert_eq!(out.len(), 1);
        assert!(
            out[0].title.contains("Open note"),
            "title was {:?}",
            out[0].title
        );
    }

    #[tokio::test]
    async fn hash_shorthand_no_leading_whitespace() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let a = p.query(&Query::new("#hello")).await;
        let b = p.query(&Query::new("# hello")).await;
        let c = p.query(&Query::new("#  hello")).await;
        assert_eq!(a[0].id, b[0].id);
        assert_eq!(b[0].id, c[0].id);
    }

    #[tokio::test]
    async fn bare_hash_is_prompt_row() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("#")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);
    }

    #[tokio::test]
    async fn activate_hash_opens_existing_without_touching_content() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("hello.md");
        fs::write(&path, "# hello\n\nexisting body").unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&"note::openOrCreate::hello".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::EditNote(p) => assert_eq!(p, path),
            other => panic!("expected EditNote, got {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# hello\n\nexisting body"
        );
    }

    #[tokio::test]
    async fn activate_hash_creates_when_missing() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&"note::openOrCreate::brand-new".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::EditNote(path) => {
                assert!(path.exists());
                assert!(fs::read_to_string(&path)
                    .unwrap()
                    .starts_with("# brand-new"));
            }
            other => panic!("expected EditNote, got {other:?}"),
        }
    }


    #[tokio::test]
    async fn hash_shows_create_first_plus_matching_notes() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("hello.md"), "Hello", 10, ""),
                note(td.path().join("helper-notes.md"), "Helper Notes", 20, ""),
                note(td.path().join("unrelated.md"), "Unrelated", 30, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("#hel")).await;
        // 1 (create/open) + 2 matching notes - but not "unrelated"
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].id, "note::openOrCreate::hel", "create row is first");
        let rest: Vec<&String> = out.iter().skip(1).map(|c| &c.id).collect();
        assert!(rest.iter().any(|id| id.ends_with("hello.md")));
        assert!(rest.iter().any(|id| id.ends_with("helper-notes.md")));
    }

    #[tokio::test]
    async fn hash_autocomplete_excludes_exact_match_to_avoid_duplicate() {
        // When `#hello` is typed and `hello.md` exists, the create-row
        // itself already points at that file (as an Open row). The
        // autocomplete pass must not surface a second row for the same
        // file underneath
        let td = TempDir::new().unwrap();
        let path = td.path().join("hello.md");
        fs::write(&path, "# hello\n").unwrap();
        let p = make_test_provider(
            vec![note(path.clone(), "Hello", 10, "")],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("#hello")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("Open note"));
    }

    #[tokio::test]
    async fn hash_autocomplete_includes_partial_matches_with_exact_exists() {
        // Exact `hello.md` exists but user typed `#hel` - we still want
        // to see sibling notes like `helper-notes.md` under the create
        // row for quick navigation
        let td = TempDir::new().unwrap();
        fs::write(td.path().join("hel.md"), "# hel\n").unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("hel.md"), "Hel", 10, ""),
                note(td.path().join("helper.md"), "Helper", 20, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("#hel")).await;
        // Create/open row + 1 sibling (helper.md). `hel.md` is the exact
        // hit already represented by the primary row
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "note::openOrCreate::hel");
        assert!(out[1].id.ends_with("helper.md"));
    }

    #[tokio::test]
    async fn hash_autocomplete_ranks_filename_stem_match_above_title_match() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                // Title match only ("alpha" is in H1 but not filename)
                note(td.path().join("zzz.md"), "Alpha notes", 0, ""),
                // Filename stem match
                note(td.path().join("alpha-report.md"), "Quarterly", 0, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("#alpha")).await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].id, "note::openOrCreate::alpha", "create row first");
        // Alpha-report.md beats zzz.md because filename stem starts with query
        assert!(out[1].id.ends_with("alpha-report.md"));
        assert!(out[2].id.ends_with("zzz.md"));
    }


    #[test]
    fn score_hash_autocomplete_none_for_empty_filter() {
        let n = note(PathBuf::from("/x.md"), "Title", 0, "body");
        assert!(score_hash_autocomplete(&n, "", Path::new("/")).is_none());
    }

    #[test]
    fn score_hash_autocomplete_none_when_absent() {
        let n = note(PathBuf::from("/alpha.md"), "Alpha", 0, "body");
        assert!(score_hash_autocomplete(&n, "zzz", Path::new("/")).is_none());
    }

    #[test]
    fn score_hash_autocomplete_ignores_content_only_matches() {
        let n = note(PathBuf::from("/xyz.md"), "Xyz", 0, "body mentions alpha");
        assert!(score_hash_autocomplete(&n, "alpha", Path::new("/")).is_none());
    }

    #[test]
    fn score_hash_autocomplete_tiers() {
        let root = Path::new("/notes");
        let exact = note(root.join("alpha.md"), "Alpha", 0, "");
        let prefix = note(root.join("alphabet.md"), "Alphabet", 0, "");
        let contains = note(root.join("beta-alpha-meta.md"), "Note", 0, "");
        let title_prefix = note(root.join("other.md"), "Alpha story", 0, "");
        let title_contains = note(root.join("other.md"), "Story about alpha", 0, "");

        let s_exact = score_hash_autocomplete(&exact, "alpha", root).unwrap();
        let s_prefix = score_hash_autocomplete(&prefix, "alpha", root).unwrap();
        let s_contains = score_hash_autocomplete(&contains, "alpha", root).unwrap();
        let s_title_prefix = score_hash_autocomplete(&title_prefix, "alpha", root).unwrap();
        let s_title_contains = score_hash_autocomplete(&title_contains, "alpha", root).unwrap();

        assert!(s_exact > s_prefix, "{s_exact} > {s_prefix}");
        assert!(s_prefix > s_contains, "{s_prefix} > {s_contains}");
        assert!(
            s_contains > s_title_prefix,
            "{s_contains} > {s_title_prefix}"
        );
        assert!(
            s_title_prefix > s_title_contains,
            "{s_title_prefix} > {s_title_contains}"
        );
    }


    #[tokio::test]
    async fn activate_duplicate_copies_and_opens() {
        let td = TempDir::new().unwrap();
        let src = td.path().join("original.md");
        fs::write(&src, "# orig\n\nbody").unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&format!("note::{}", src.display()), "duplicate")
            .await
            .unwrap();
        match eff {
            Effect::EditNote(dst) => {
                assert!(dst.exists());
                assert_ne!(dst, src);
                assert_eq!(fs::read_to_string(&dst).unwrap(), "# orig\n\nbody");
                assert!(dst.file_stem().unwrap().to_str().unwrap().contains("copy"));
            }
            other => panic!("expected EditNote, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_increments_suffix_when_copy_exists() {
        let td = TempDir::new().unwrap();
        let a = td.path().join("note.md");
        fs::write(&a, "x").unwrap();
        let dup1 = duplicate_note(&a).unwrap();
        assert_eq!(dup1.file_name().unwrap().to_str().unwrap(), "note copy.md");
        let dup2 = duplicate_note(&a).unwrap();
        assert_eq!(
            dup2.file_name().unwrap().to_str().unwrap(),
            "note copy 2.md"
        );
        let dup3 = duplicate_note(&a).unwrap();
        assert_eq!(
            dup3.file_name().unwrap().to_str().unwrap(),
            "note copy 3.md"
        );
    }

    #[test]
    fn duplicate_of_copy_doesnt_stutter() {
        let td = TempDir::new().unwrap();
        let existing_copy = td.path().join("note copy.md");
        fs::write(&existing_copy, "y").unwrap();
        let dup = duplicate_note(&existing_copy).unwrap();
        assert_eq!(dup.file_name().unwrap().to_str().unwrap(), "note copy 2.md");
    }

    #[test]
    fn strip_copy_suffix_cases() {
        assert_eq!(strip_copy_suffix("note"), "note");
        assert_eq!(strip_copy_suffix("note copy"), "note");
        assert_eq!(strip_copy_suffix("note copy 2"), "note");
        assert_eq!(strip_copy_suffix("note copy 27"), "note");
        assert_eq!(strip_copy_suffix("note copy abc"), "note copy abc"); // not a number
        assert_eq!(strip_copy_suffix("copy"), "copy"); // bare "copy" is not a suffix
    }


    #[tokio::test]
    async fn activate_trash_yields_trash_file_effect() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&"note::/tmp/x.md".to_string(), "trash")
            .await
            .unwrap();
        match eff {
            Effect::TrashFile(path) => assert_eq!(path, PathBuf::from("/tmp/x.md")),
            other => panic!("expected TrashFile, got {other:?}"),
        }
    }

    // Candidate shape: Open-or-Create label flip

    #[test]
    fn open_or_create_labels_reflect_existence() {
        let root = Path::new("/tmp");
        let path_missing = root.join("nonexistent.md");
        let c = open_or_create_candidate("hello", &path_missing, root, false);
        assert!(c.title.contains("Create note"));

        let c = open_or_create_candidate("hello", &path_missing, root, true);
        assert!(c.title.contains("Open note"));
    }

    #[test]
    fn parse_note_input_variants() {
        use ParsedNoteInput::*;
        let list = |s: &str| Some(List(s.to_string()));
        let create = |s: &str| Some(Create(s.to_string()));
        let prompt = Some(NewNotePrompt);

        // List family
        assert_eq!(parse_note_input("note"), list(""));
        assert_eq!(parse_note_input("notes"), list(""));
        assert_eq!(parse_note_input("n"), list(""));
        assert_eq!(parse_note_input("note hello"), list("hello"));
        assert_eq!(parse_note_input("notes world"), list("world"));
        assert_eq!(parse_note_input("n meeting"), list("meeting"));

        // Create family (all aliases + `note new` long form)
        assert_eq!(parse_note_input("note new "), prompt);
        assert_eq!(parse_note_input("note new"), prompt); // trimmed-trailing-space case
        assert_eq!(parse_note_input("note new foo"), create("foo"));
        assert_eq!(parse_note_input("newnote"), prompt);
        assert_eq!(parse_note_input("newnote "), prompt);
        assert_eq!(parse_note_input("newnote foo"), create("foo"));
        assert_eq!(parse_note_input("nn"), prompt);
        assert_eq!(parse_note_input("nn "), prompt);
        assert_eq!(parse_note_input("nn foo"), create("foo"));

        // `#` shorthand
        assert_eq!(parse_note_input("#"), prompt);
        assert_eq!(
            parse_note_input("#hello"),
            Some(OpenOrCreate("hello".into()))
        );
        assert_eq!(
            parse_note_input("# hello"),
            Some(OpenOrCreate("hello".into()))
        );
        assert_eq!(
            parse_note_input("#   hello world"),
            Some(OpenOrCreate("hello world".into()))
        );
        assert_eq!(
            parse_note_input("#  foo  "),
            Some(OpenOrCreate("foo".into()))
        );

        // Non-matches - no accidental prefix leakage
        assert_eq!(parse_note_input("noteworthy"), None);
        assert_eq!(parse_note_input("newnoting"), None);
        assert_eq!(parse_note_input("nnm"), None); // `nn` must be followed by space or EOL
        assert_eq!(parse_note_input("nothing"), None); // `n` must be followed by space or EOL
        assert_eq!(parse_note_input(""), None);
    }

    #[test]
    fn parse_note_input_findnote_family() {
        use ParsedNoteInput::*;
        let search = |s: &str| Some(SearchContent(s.to_string()));
        let all = |s: &str| Some(ListAll(s.to_string()));

        // `findnote` + aliases -> SearchContent (with non-empty query)
        assert_eq!(parse_note_input("findnote foo bar"), search("foo bar"));
        assert_eq!(parse_note_input("searchnotes hello"), search("hello"));
        assert_eq!(parse_note_input("searchnote world"), search("world"));

        // Bare `findnote` (no query) -> ListAll so user sees
        // something while thinking about what to type
        assert_eq!(parse_note_input("findnote"), all(""));
        assert_eq!(parse_note_input("findnote "), all(""));
        assert_eq!(parse_note_input("searchnotes  "), all(""));

        // `findnotely` must NOT match - keyword needs whitespace after
        assert_eq!(parse_note_input("findnotely"), None);
    }

    #[test]
    fn parse_note_input_all_variant_lifts_cap() {
        use ParsedNoteInput::*;
        assert_eq!(parse_note_input("note all"), Some(ListAll(String::new())));
        assert_eq!(parse_note_input("notes all"), Some(ListAll(String::new())));
        assert_eq!(
            parse_note_input("note all meeting"),
            Some(ListAll("meeting".into()))
        );
        assert_eq!(
            parse_note_input("notes all work/q1"),
            Some(ListAll("work/q1".into()))
        );
        // `n all` takes same path
        assert_eq!(parse_note_input("n all"), Some(ListAll(String::new())));
    }

    #[tokio::test]
    async fn findnote_matches_content_not_just_title() {
        // Critical regression: a says "schema migration" in
        // its body but has a title of "March 3" must surface when the
        // user searches `findnote schema`
        let td = TempDir::new().unwrap();
        let notes = vec![
            note(
                td.path().join("march-3.md"),
                "March 3",
                0,
                "# March 3\n\nDiscussed the schema migration in depth.",
            ),
            note(
                td.path().join("unrelated.md"),
                "Unrelated",
                0,
                "# Unrelated\n\nNothing here.",
            ),
        ];
        let p = make_test_provider(notes, td.path().to_path_buf());
        let out = p.query(&Query::new("findnote schema")).await;
        assert!(
            out.iter().any(|c| c.id.contains("march-3.md")),
            "body-only match must surface: {:?}",
            out.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn findnote_ranks_content_above_title_only_match() {
        // Title-only hit gets a modest bonus; content-only hit scores
        // higher because user's intent with `findnote` is body
        // search
        let td = TempDir::new().unwrap();
        let notes = vec![
            note(
                td.path().join("title-only.md"),
                "foo mentioned once",
                0,
                "# foo mentioned once\n\nNothing else about it.",
            ),
            note(
                td.path().join("content-rich.md"),
                "March 3",
                0,
                "# March 3\n\nfoo foo foo - lots of body content about foo.",
            ),
        ];
        let p = make_test_provider(notes, td.path().to_path_buf());
        let out = p.query(&Query::new("findnote foo")).await;
        // Must have both, content-rich first
        assert!(!out.is_empty());
        assert!(
            out[0].id.contains("content-rich.md"),
            "content-rich must rank first, got {:?}",
            out.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn findnote_empty_result_yields_guidance_row() {
        let td = TempDir::new().unwrap();
        let notes = vec![note(td.path().join("x.md"), "X", 0, "# X\n\nnothing")];
        let p = make_test_provider(notes, td.path().to_path_buf());
        let out = p.query(&Query::new("findnote absentword")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("note::findnote::empty::"));
        assert!(out[0].title.contains("absentword"));
    }

    #[tokio::test]
    async fn findnote_guidance_row_does_nothing_loudly() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&"note::findnote::empty::anything".to_string(), "default")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::None));
    }

    #[tokio::test]
    async fn notes_all_on_empty_folder_speaks_up_instead_of_staying_silent() {
        // REGRESSION: user typed `notes all` with no notes in their
        // folder, saw "Settings: Internet Accounts" instead (weak
        // fuzzy match from another provider filled the empty list).
        // `notes all` must always emit note-related rows so no other
        // provider's noise takes the top spot
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("notes all")).await;
        assert!(!out.is_empty(), "empty folder must still return rows");
        assert!(
            out.iter().any(|c| c.id == NEW_NOTE_PROMPT_ID),
            "new-note prompt is always present"
        );
        assert!(
            out.iter().any(|c| c.id == "note::list-empty"),
            "empty-folder guidance row present: {:?}",
            out.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
        // All rows bypass_rank so they sit above any unrelated fuzzy
        // matches from other providers
        assert!(
            out.iter().all(|c| c.bypass_rank),
            "every row is bypass_rank"
        );
    }

    #[tokio::test]
    async fn notes_all_with_filter_no_match_surfaces_guidance() {
        let td = TempDir::new().unwrap();
        let notes = vec![note(td.path().join("a.md"), "Alpha", 0, "# Alpha\n\nbody")];
        let p = make_test_provider(notes, td.path().to_path_buf());
        let out = p.query(&Query::new("notes all zzzzz")).await;
        let empty = out
            .iter()
            .find(|c| c.id == "note::list-empty")
            .expect("empty row present");
        assert!(
            empty.title.contains("zzzzz"),
            "title carries filter: {:?}",
            empty.title
        );
    }

    #[tokio::test]
    async fn notes_list_empty_row_activation_is_noop() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&"note::list-empty".to_string(), "default")
            .await
            .unwrap();
        assert!(matches!(eff, Effect::None));
    }

    #[tokio::test]
    async fn notes_all_bypasses_result_limit() {
        // Plain `notes` caps at RESULT_LIMIT = 20. `notes all` must
        // return every note in the folder
        let td = TempDir::new().unwrap();
        let many: Vec<Note> = (0..35)
            .map(|i| {
                note(
                    td.path().join(format!("n{i}.md")),
                    &format!("note-{i}"),
                    i as i64,
                    &format!("# note-{i}\n"),
                )
            })
            .collect();
        let p = make_test_provider(many, td.path().to_path_buf());

        let bare = p.query(&Query::new("notes")).await;
        // `notes` (bare) emits prompt row + folder rows + capped notes.
        // Count just the note rows
        let bare_notes = bare
            .iter()
            .filter(|c| {
                c.id.starts_with("note::") && !c.id.contains("prompt") && !c.id.contains("folder::")
            })
            .count();
        assert!(
            bare_notes <= RESULT_LIMIT,
            "bare notes respects cap: got {bare_notes}"
        );

        let all = p.query(&Query::new("notes all")).await;
        let all_notes = all
            .iter()
            .filter(|c| {
                c.id.starts_with("note::") && !c.id.contains("prompt") && !c.id.contains("folder::")
            })
            .count();
        assert_eq!(all_notes, 35, "notes all shows every note");
    }

    #[tokio::test]
    async fn notes_all_with_filter_narrows_but_still_uncapped() {
        let td = TempDir::new().unwrap();
        let many: Vec<Note> = (0..40)
            .map(|i| {
                let body = if i % 3 == 0 {
                    "spring planning"
                } else {
                    "other"
                };
                note(
                    td.path().join(format!("n{i}.md")),
                    &format!("n{i}"),
                    i as i64,
                    &format!("# n{i}\n\n{body}\n"),
                )
            })
            .collect();
        let p = make_test_provider(many, td.path().to_path_buf());
        let out = p.query(&Query::new("notes all spring")).await;
        let hits = out
            .iter()
            .filter(|c| {
                c.id.starts_with("note::") && !c.id.contains("prompt") && !c.id.contains("folder::")
            })
            .count();
        // 0..40 has 14 multiples of 3 (0, 3, 6, ..., 39) -> 14 matches
        assert_eq!(hits, 14, "every match surfaces: {hits}");
    }

    #[test]
    fn parse_note_keyword_compat_shim() {
        // The old helper survives for non-create paths; create variants
        // now route through parse_note_input
        assert_eq!(parse_note_keyword("note"), Some(String::new()));
        assert_eq!(parse_note_keyword("note hello"), Some("hello".into()));
        assert_eq!(parse_note_keyword("note new hi"), None); // Create path
        assert_eq!(parse_note_keyword("newnote hi"), None);
    }


    #[tokio::test]
    async fn filter_matches_title() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("a.md"), "Grocery list", 0, ""),
                note(td.path().join("b.md"), "Weekly plan", 0, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note grocer")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Grocery list");
    }

    #[tokio::test]
    async fn filter_matches_filename() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("project-alpha.md"), "Untitled", 0, "")],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note alpha")).await;
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn filter_matches_content_body() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("a.md"), "Unrelated", 0, "Random body text."),
                note(
                    td.path().join("b.md"),
                    "Also Unrelated",
                    0,
                    "Daily standup meeting notes about the Kepler migration.",
                ),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note kepler")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Also Unrelated");
    }

    #[tokio::test]
    async fn title_match_outranks_content_match() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                // Content-only hit, written most recently
                note(
                    td.path().join("b.md"),
                    "B note",
                    100,
                    "mentions alpha inside",
                ),
                // Title hit, older
                note(td.path().join("a.md"), "Alpha notes", 1, "unrelated body"),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note alpha")).await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "Alpha notes", "title match should come first");
    }

    #[tokio::test]
    async fn filename_match_outranks_content_match() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("alpha-report.md"), "Untitled", 0, "xyz"),
                note(
                    td.path().join("other.md"),
                    "Other",
                    0,
                    "alpha somewhere here",
                ),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note alpha")).await;
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].id,
            format!("note::{}", td.path().join("alpha-report.md").display())
        );
    }

    #[tokio::test]
    async fn exact_title_beats_starts_with_beats_contains() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("c.md"), "Meeting agenda", 0, ""),
                note(td.path().join("b.md"), "Meeting", 0, ""),
                note(td.path().join("a.md"), "Standup meeting with team", 0, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note meeting")).await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].title, "Meeting");
        assert_eq!(out[1].title, "Meeting agenda");
        assert_eq!(out[2].title, "Standup meeting with team");
    }

    #[tokio::test]
    async fn non_matching_filter_offers_create_fallback() {
        // UX: a filter that doesn't find anything shouldn't leave the
        // user staring at an empty panel - offer to create that note
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("a.md"), "Alpha", 0, "body"),
                note(td.path().join("b.md"), "Beta", 0, "body"),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note zzzz")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("note::create::"));
        assert!(out[0].title.contains("Create note"));
        assert!(out[0].title.contains("zzzz"));
    }

    #[tokio::test]
    async fn fallback_uses_filter_verbatim_for_title() {
        // Preserves user's exact casing/wording instead of the
        // lowercased filter used for matching
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("note Sprint Review")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("Sprint Review"));
    }

    #[tokio::test]
    async fn matching_filter_does_not_show_create_fallback() {
        // When the filter resolves to at least one note, dont clutter
        // results with a "Create note" row - user was searching
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("alpha.md"), "Alpha", 0, "")],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note alpha")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("note::"));
        assert!(!out[0].id.starts_with("note::create::"));
    }

    #[tokio::test]
    async fn fallback_via_n_alias() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("n brand new idea")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.starts_with("note::create::"));
    }

    #[tokio::test]
    async fn content_snippet_shown_in_subtitle_on_content_hit() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(
                td.path().join("b.md"),
                "Unrelated title",
                0,
                "Lorem ipsum dolor sit amet. Kepler migration is tricky. Fin.",
            )],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note kepler")).await;
        assert_eq!(out.len(), 1);
        let sub = out[0].subtitle.as_deref().unwrap_or("");
        assert!(
            sub.to_lowercase().contains("kepler"),
            "subtitle was: {sub:?}"
        );
    }

    #[tokio::test]
    async fn search_text_includes_content_for_fuzzy_fallback() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(
                td.path().join("a.md"),
                "Title",
                0,
                "secret-needle here",
            )],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note needle")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].search_text.contains("secret-needle"));
    }

    #[tokio::test]
    async fn default_list_orders_by_mtime_desc() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("new.md"), "new", 100, ""),
                note(td.path().join("old.md"), "old", 1, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note")).await;
        // Now includes the "+ New note" prompt row. Filter it out for
        // the ordering check
        let notes: Vec<&Candidate> = out.iter().filter(|c| c.id != NEW_NOTE_PROMPT_ID).collect();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].title, "new");
    }


    #[tokio::test]
    async fn new_note_shows_create_candidate() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("note new shopping list")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].title.contains("Create note"));
        assert!(out[0].id.starts_with("note::create::"));
    }

    #[tokio::test]
    async fn new_note_empty_title_shows_prompt_row() {
        // Used to return empty; now offers the discoverable prompt row so
        // users aren't stranded after typing `note new `
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let out = p.query(&Query::new("note new ")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);
    }


    #[tokio::test]
    async fn activate_default_emits_edit_note() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let path = td.path().join("x.md");
        let effect = p
            .activate(&format!("note::{}", path.display()), "default")
            .await
            .unwrap();
        match effect {
            Effect::EditNote(p) => assert_eq!(p, path),
            other => panic!("expected EditNote, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_open_external_emits_open_path() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let path = td.path().join("x.md");
        let effect = p
            .activate(&format!("note::{}", path.display()), "open-external")
            .await
            .unwrap();
        assert!(matches!(effect, Effect::OpenPath(_)));
    }

    #[tokio::test]
    async fn activate_reveal_reveals_in_finder() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let effect = p
            .activate(&"note::/tmp/x.md".to_string(), "reveal")
            .await
            .unwrap();
        assert!(matches!(effect, Effect::RevealInFinder(_)));
    }

    #[tokio::test]
    async fn activate_copy_path() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let effect = p
            .activate(&"note::/tmp/x.md".to_string(), "copy-path")
            .await
            .unwrap();
        match effect {
            Effect::CopyToClipboard(s) => assert_eq!(s, "/tmp/x.md"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_preview_shows_rendered_markdown() {
        let td = TempDir::new().unwrap();
        let path = write_note(td.path(), "hello.md", "# Header\n\n**bold** *italic*");
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let effect = p
            .activate(&format!("note::{}", path.display()), "preview")
            .await
            .unwrap();
        match effect {
            Effect::ShowText {
                text,
                label,
                language,
                editable_path,
            } => {
                assert!(text.contains("# Header"));
                assert_eq!(label, "hello");
                assert_eq!(language.as_deref(), Some("markdown"));
                assert_eq!(
                    editable_path,
                    Some(path.clone()),
                    "note preview carries its path for the ↵-edit shortcut"
                );
            }
            other => panic!("expected ShowText, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn note_candidates_expose_preview_action() {
        let td = TempDir::new().unwrap();
        let n = note(td.path().join("a.md"), "A", 0, "");
        let p = make_test_provider(vec![n], td.path().to_path_buf());
        let out = p.query(&Query::new("note ")).await;
        let note_row = out
            .iter()
            .find(|c| c.id.starts_with("note::") && !c.id.contains("prompt"));
        assert!(note_row.is_some());
        assert!(note_row.unwrap().actions.iter().any(|a| a.id == "preview"));
    }

    #[tokio::test]
    async fn activate_copy_content() {
        let td = TempDir::new().unwrap();
        let path = write_note(td.path(), "x.md", "# My Note\n\nBody here.");
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let effect = p
            .activate(&format!("note::{}", path.display()), "copy-content")
            .await
            .unwrap();
        if let Effect::CopyToClipboard(s) = effect {
            assert!(s.contains("My Note"));
            assert!(s.contains("Body here"));
        } else {
            panic!("expected CopyToClipboard");
        }
    }

    #[tokio::test]
    async fn activate_create_writes_file_and_emits_edit() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let effect = p
            .activate(&"note::create::Fresh Thoughts".to_string(), "default")
            .await
            .unwrap();
        match effect {
            Effect::EditNote(path) => {
                assert!(path.exists());
                let content = fs::read_to_string(&path).unwrap();
                assert!(content.starts_with("# Fresh Thoughts"));
            }
            other => panic!("expected EditNote, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_create_does_not_overwrite_existing() {
        let td = TempDir::new().unwrap();
        let slug_path = td.path().join("preserve-me.md");
        fs::write(&slug_path, "original content").unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let _ = p
            .activate(&"note::create::Preserve me".to_string(), "default")
            .await
            .unwrap();
        let content = fs::read_to_string(&slug_path).unwrap();
        assert_eq!(content, "original content");
    }

    #[tokio::test]
    async fn activate_unknown_action_errors() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        assert!(p
            .activate(&"note::/tmp/x.md".to_string(), "wat")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }


    #[test]
    fn content_snippet_none_for_empty_filter() {
        let n = note(PathBuf::from("/x.md"), "t", 0, "anything");
        assert!(content_snippet(&n, "").is_none());
    }

    #[test]
    fn content_snippet_none_when_absent() {
        let n = note(PathBuf::from("/x.md"), "t", 0, "nothing to see");
        assert!(content_snippet(&n, "missing").is_none());
    }

    #[test]
    fn content_snippet_centers_on_match_and_strips_newlines() {
        let body = format!(
            "{}needle{}",
            "lorem ipsum ".repeat(10), // ~120 chars before
            "\ndolor\n".repeat(5)
        );
        let n = note(PathBuf::from("/x.md"), "t", 0, &body);
        let s = content_snippet(&n, "needle").expect("match");
        assert!(s.starts_with('…'), "expected leading ellipsis: {s:?}");
        assert!(s.contains("needle"));
        assert!(!s.contains('\n'));
        assert!(!s.contains('\t'));
    }

    #[test]
    fn content_snippet_no_leading_ellipsis_when_match_is_at_start() {
        let n = note(PathBuf::from("/x.md"), "t", 0, "hello world");
        let s = content_snippet(&n, "hello").expect("match");
        assert!(!s.starts_with('…'));
    }

    #[test]
    fn content_snippet_utf8_safe_on_multibyte_boundaries() {
        // Place the match inside a sea of multi-byte characters so naive
        // byte-slicing would panic
        let padding = "漢字".repeat(30); // 60 UTF-8 multi-byte chars
        let body = format!("{padding}needle{padding}");
        let n = note(PathBuf::from("/x.md"), "t", 0, &body);
        let s = content_snippet(&n, "needle").expect("match");
        assert!(s.contains("needle"));
    }


    #[test]
    fn score_note_exact_title_hits_top_tier() {
        let n = note(PathBuf::from("/t.md"), "Alpha", 0, "");
        let score = score_note(&n, "alpha", Path::new("/")).unwrap();
        assert!(score >= 1000, "got {score}");
    }

    #[test]
    fn score_note_no_match_is_none() {
        let n = note(PathBuf::from("/t.md"), "Alpha", 0, "body");
        assert!(score_note(&n, "zzz", Path::new("/")).is_none());
    }

    #[test]
    fn score_note_content_only_gives_baseline_score() {
        let n = note(PathBuf::from("/t.md"), "Alpha", 0, "needle here");
        let score = score_note(&n, "needle", Path::new("/")).unwrap();
        assert!((100..500).contains(&score), "got {score}");
    }

    #[test]
    fn score_note_fuzzy_catches_single_char_typos() {
        // `helo` (typo for `hello`) should still match a `hello.md` note
        // so launcher suggests editing rather than offering to
        // create a near-duplicate
        let n = note(PathBuf::from("/hello.md"), "Hello", 0, "");
        assert!(score_note(&n, "helo", Path::new("/")).is_some());
    }

    #[test]
    fn score_note_fuzzy_respects_short_filter_threshold() {
        // For very short filters (<= 4 chars), only edit distance 1 is
        // accepted - distance 2 on "abc" would match everything
        let n = note(PathBuf::from("/abc.md"), "Abc", 0, "");
        assert!(score_note(&n, "azc", Path::new("/")).is_some()); // d=1
        assert!(score_note(&n, "xyz", Path::new("/")).is_none()); // d=3
    }

    #[test]
    fn score_note_fuzzy_skipped_for_exact_substring_match() {
        // Dont layer fuzzy on top of a clean substring hit - it would
        // inflate scores unpredictably. The strong tier already wins
        let n = note(PathBuf::from("/hello.md"), "Hello", 0, "");
        let substring = score_note(&n, "hello", Path::new("/")).unwrap();
        let fuzzy = score_note(&n, "helo", Path::new("/")).unwrap();
        assert!(substring > fuzzy, "substring {substring} > fuzzy {fuzzy}");
    }

    #[test]
    fn score_note_fuzzy_ignored_for_too_short_filters() {
        // Two-char filters are below the fuzzy threshold - they're
        // either substring hits or nothing
        let n = note(PathBuf::from("/hello.md"), "Hello", 0, "");
        assert!(score_note(&n, "xy", Path::new("/")).is_none());
    }

    // Chain tests moved to gyors-ipc, where the generic
    // `>` orchestrator now owns cross-provider action chaining.

    #[tokio::test]
    async fn note_filter_typo_matches_multi_word_title() {
        // REGRESSION: user had a note titled `Hello World` with
        // filename `hello-world.md`. Typing `note helo` surfaced a
        // Create row because the fuzzy comparison ran against whole
        // strings only (distance to "hello world" / "hello-world" is
        // 7, max_d is 1-2). Tokenising fixes it: "hello" is one of
        // the tokens, distance 1
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(
                td.path().join("hello-world.md"),
                "Hello World",
                0,
                "# Hello World\n",
            )],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note helo")).await;
        assert!(!out.is_empty(), "must have at least one hit");
        assert!(
            !out.iter().any(|c| c.id.starts_with("note::create::")),
            "no Create row when fuzzy matched: {:?}",
            out.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
        assert!(
            out.iter().any(|c| c.id.contains("hello-world.md")),
            "existing note surfaced: {:?}",
            out.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn note_filter_typo_matches_nested_folder_note() {
        // Nested path case: `work/projects/hello-meeting.md`. The
        // typo "helo" must still find it via the "hello" token in
        // the stem
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(
                td.path().join("work/projects/hello-meeting.md"),
                "Hello Meeting",
                0,
                "",
            )],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note helo")).await;
        assert!(
            !out.iter().any(|c| c.id.starts_with("note::create::")),
            "no Create row on nested typo"
        );
        assert!(
            out.iter().any(|c| c.id.ends_with("hello-meeting.md")),
            "nested note surfaced"
        );
    }

    #[tokio::test]
    async fn note_filter_typo_matches_underscored_filename() {
        // Same story for `hello_world.md` - show is a token
        // separator too
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("hello_world.md"), "Hello World", 0, "")],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note helo")).await;
        assert!(
            !out.iter().any(|c| c.id.starts_with("note::create::")),
            "no Create row: {:?}",
            out.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn note_filter_exact_substring_still_beats_fuzzy() {
        // Ensure the fuzzy widening didn't accidentally outrank a
        // real substring match. "hello" matches `hello-world` as an
        // exact stem substring (tier 300) - a fuzzy hit on "world"
        // should not win over that
        let td = TempDir::new().unwrap();
        let a = note(td.path().join("hello-world.md"), "Hello World", 1, "");
        let b = note(td.path().join("wold.md"), "Wold", 2, ""); // fuzzy to "world"
        let p = make_test_provider(vec![a, b], td.path().to_path_buf());
        let out = p.query(&Query::new("note world")).await;
        assert!(
            out[0].id.contains("hello-world.md"),
            "exact substring wins: {:?}",
            out.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tokens_splits_on_common_delimiters() {
        let got: Vec<&str> = tokens("hello-world_foo.bar baz").collect();
        assert_eq!(got, vec!["hello", "world", "foo", "bar", "baz"]);
    }

    #[test]
    fn tokens_skips_empty() {
        let got: Vec<&str> = tokens("--hello__world--").collect();
        assert_eq!(got, vec!["hello", "world"]);
    }

    #[tokio::test]
    async fn note_filter_with_typo_shows_existing_not_create() {
        // `note helo` when `hello.md` exists must not surface a Create row
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("hello.md"), "Hello", 0, "")],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note helo")).await;
        assert_eq!(out.len(), 1);
        assert!(
            !out[0].id.starts_with("note::create::"),
            "expected existing note, got {}",
            out[0].id
        );
    }


    #[test]
    fn slugify_path_simple() {
        assert_eq!(slugify_path("Hello World"), Some("hello-world".into()));
    }

    #[test]
    fn slugify_path_nested() {
        assert_eq!(
            slugify_path("Work/Meeting Notes"),
            Some("work/meeting-notes".into())
        );
    }

    #[test]
    fn slugify_path_deeply_nested() {
        assert_eq!(
            slugify_path("Work/Q1/Sprint Planning"),
            Some("work/q1/sprint-planning".into())
        );
    }

    #[test]
    fn slugify_path_trims_slashes() {
        assert_eq!(slugify_path("/work/notes/"), Some("work/notes".into()));
    }

    #[test]
    fn slugify_path_collapses_empty_segments() {
        assert_eq!(slugify_path("a//b///c"), Some("a/b/c".into()));
    }

    #[test]
    fn slugify_path_rejects_dotdot_segments() {
        assert_eq!(slugify_path(".."), None);
        assert_eq!(slugify_path("../foo"), Some("foo".into()));
        assert_eq!(slugify_path("foo/../bar"), Some("foo/bar".into()));
        assert_eq!(slugify_path("foo/.."), Some("foo".into()));
    }

    #[test]
    fn slugify_path_skips_dot_segments() {
        assert_eq!(slugify_path("./foo"), Some("foo".into()));
        assert_eq!(slugify_path("foo/./bar"), Some("foo/bar".into()));
    }

    #[test]
    fn slugify_path_none_when_empty() {
        assert_eq!(slugify_path(""), None);
        assert_eq!(slugify_path("   "), None);
        assert_eq!(slugify_path("///"), None);
        assert_eq!(slugify_path("!!!"), None); // all non-alphanumeric -> empty slug
    }

    #[test]
    fn slugify_path_trims_segment_whitespace() {
        assert_eq!(
            slugify_path(" work / meeting "),
            Some("work/meeting".into())
        );
    }


    #[test]
    fn display_relative_strips_root() {
        let root = PathBuf::from("/Users/me/Documents/Gyors");
        let note = PathBuf::from("/Users/me/Documents/Gyors/work/meeting.md");
        assert_eq!(display_relative(&note, &root), "work/meeting.md");
    }

    #[test]
    fn display_relative_falls_back_when_outside_root() {
        let root = PathBuf::from("/Users/me/Documents/Gyors");
        let note = PathBuf::from("/tmp/stray.md");
        assert_eq!(display_relative(&note, &root), "/tmp/stray.md");
    }


    #[test]
    fn ensure_in_root_accepts_nested() {
        let root = PathBuf::from("/root");
        assert!(ensure_in_root(&root.join("a/b/c.md"), &root).is_ok());
    }

    #[test]
    fn ensure_in_root_rejects_escape() {
        let root = PathBuf::from("/root");
        assert!(ensure_in_root(&root.join("../escape.md"), &root).is_err());
    }

    /// Regression. Pre-fix, a symlink inside the notes
    /// folder pointing outside it would let a write land wherever
    /// the symlink pointed (because the lexical check sees only
    /// the textual path). Post-fix, `ensure_real_path_in_root`
    /// canonicalises the parent and rejects
    #[test]
    fn ensure_real_path_in_root_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("notes");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // Plant a symlink `notes/escape` -> outside/. A
        // pre-fix `ensure_in_root` would accept any write under
        // `notes/escape/<anything>` because lexically it stays
        // under root
        symlink(&outside, root.join("escape")).unwrap();

        let target = root.join("escape").join("pwned.md");
        // Lexical check passes (path is textually under root)
        assert!(
            ensure_in_root(&target, &root).is_ok(),
            "lexical check should pass - that's the vulnerability"
        );
        // Real-path check rejects
        let err = ensure_real_path_in_root(&target, &root).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("escapes notes folder") || msg.contains("via symlink"),
            "expected symlink-escape error, got: {msg}"
        );
    }

    #[test]
    fn ensure_real_path_in_root_accepts_normal_nested_write() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("notes");
        let nested = root.join("work").join("sprint");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("plan.md");
        // Parent exists - canonicalize works. Result should still
        // be under canonicalized root
        assert!(ensure_real_path_in_root(&file, &root).is_ok());
    }

    #[test]
    fn lexical_normalize_folds_dotdot() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexical_normalize(Path::new("/a/./b")),
            PathBuf::from("/a/b")
        );
    }


    #[test]
    fn last_segment_cases() {
        assert_eq!(last_segment("foo"), "foo");
        assert_eq!(last_segment("a/b/c"), "c");
        assert_eq!(last_segment("work/meeting "), "meeting");
    }


    #[tokio::test]
    async fn create_nested_note_makes_folders_and_file() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&"note::create::Work/Sprint Planning".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::EditNote(path) => {
                assert_eq!(
                    path,
                    td.path().join("work").join("sprint-planning.md"),
                    "path"
                );
                assert!(path.parent().unwrap().exists(), "folder was created");
                let content = fs::read_to_string(&path).unwrap();
                // H1 is the *last* segment, not the full folder path
                assert!(
                    content.starts_with("# Sprint Planning"),
                    "content={content:?}"
                );
            }
            other => panic!("expected EditNote, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hash_nested_creates_then_subsequent_opens() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());

        // First activation creates
        let eff = p
            .activate(&"note::openOrCreate::projects/alpha".to_string(), "default")
            .await
            .unwrap();
        let path = match eff {
            Effect::EditNote(p) => p,
            other => panic!("expected EditNote, got {other:?}"),
        };
        assert_eq!(path, td.path().join("projects").join("alpha.md"));
        assert!(path.exists());

        // Mutate contents - re-activating mustn't overwrite
        fs::write(&path, "# alpha\n\nkept").unwrap();
        let eff2 = p
            .activate(&"note::openOrCreate::projects/alpha".to_string(), "default")
            .await
            .unwrap();
        match eff2 {
            Effect::EditNote(p) => assert_eq!(p, path),
            other => panic!("expected EditNote, got {other:?}"),
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "# alpha\n\nkept");
    }

    #[tokio::test]
    async fn hash_rejects_escaping_title() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        // `..` collapses inside slugify_path, leaving empty -> no candidate
        let out = p.query(&Query::new("#..")).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn search_matches_folder_segment() {
        // `note meet` should find a nested meeting note even when the
        // word "meet" doesn't appear in its title/filename
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("work/meetings/q1.md"), "Q1", 10, ""),
                note(td.path().join("recipes.md"), "Recipes", 20, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note meet")).await;
        assert_eq!(out.len(), 1);
        assert!(out[0].id.ends_with("q1.md"));
    }

    #[tokio::test]
    async fn candidate_subtitle_uses_relative_path() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("work/notes.md"), "Notes", 10, "")],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note notes")).await;
        assert_eq!(out.len(), 1);
        let sub = out[0].subtitle.as_deref().unwrap_or("");
        // Subtitle is the relative path, not the absolute tempdir path
        assert_eq!(sub, "work/notes.md", "got {sub:?}");
    }

    #[tokio::test]
    async fn hash_autocomplete_suggests_notes_in_same_folder() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("work/sprint.md"), "Sprint", 10, ""),
                note(td.path().join("work/retro.md"), "Retro", 20, ""),
                note(td.path().join("other.md"), "Other", 30, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("#work/")).await;
        // The create/open row for `work/` itself, plus the two notes that
        // live inside `work/`. `other.md` stays out
        assert!(
            out.iter().any(|c| c.id.ends_with("work/sprint.md")),
            "expected work/sprint.md in {:?}",
            out.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
        );
        assert!(out.iter().any(|c| c.id.ends_with("work/retro.md")));
        assert!(!out.iter().any(|c| c.id.ends_with("other.md")));
    }


    #[tokio::test]
    async fn bare_list_shows_folder_rows_with_counts() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("work/sprint.md"), "Sprint", 10, ""),
                note(td.path().join("work/retro.md"), "Retro", 11, ""),
                note(td.path().join("personal/gifts.md"), "Gifts", 12, ""),
                note(td.path().join("flat.md"), "Flat", 13, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note")).await;
        // Prompt + 2 folder rows (personal, work) + 4 note rows = 7
        assert_eq!(out.len(), 7);
        assert_eq!(out[0].id, NEW_NOTE_PROMPT_ID);
        assert_eq!(out[1].id, "note::folder::personal", "folders sorted alpha");
        assert_eq!(out[2].id, "note::folder::work");
        let personal = &out[1];
        assert!(personal.subtitle.as_deref().unwrap().contains("1 note"));
        let work = &out[2];
        assert!(work.subtitle.as_deref().unwrap().contains("2 notes"));
    }

    #[tokio::test]
    async fn flat_notes_dont_produce_folder_rows() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("a.md"), "A", 10, ""),
                note(td.path().join("b.md"), "B", 20, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note")).await;
        // Prompt + 2 notes - no folder rows injected
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|c| !c.id.starts_with("note::folder::")));
    }

    #[tokio::test]
    async fn folder_row_activation_drills_into_that_folder() {
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![note(td.path().join("work/sprint.md"), "Sprint", 10, "")],
            td.path().to_path_buf(),
        );
        let eff = p
            .activate(&"note::folder::work".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::SetInput(s) => assert_eq!(s, "#work/"),
            other => panic!("expected SetInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn filtered_list_does_not_inject_folder_rows() {
        // With a filter, path-segment scoring already surfaces nested
        // notes. Folder rows would just clutter narrow searches
        let td = TempDir::new().unwrap();
        let p = make_test_provider(
            vec![
                note(td.path().join("work/sprint.md"), "Sprint", 10, ""),
                note(td.path().join("work/retro.md"), "Retro", 20, ""),
            ],
            td.path().to_path_buf(),
        );
        let out = p.query(&Query::new("note work")).await;
        assert!(out.iter().all(|c| !c.id.starts_with("note::folder::")));
    }

    #[test]
    fn top_level_folders_counts_only_direct_parent() {
        // A nested-nested note counts toward its top-level folder, not
        // each intermediate level. That keeps the browse list tidy
        let root = Path::new("/root");
        let notes = vec![
            note(root.join("work/q1/meetings/kickoff.md"), "K", 0, ""),
            note(root.join("work/retro.md"), "R", 0, ""),
            note(root.join("personal/gifts.md"), "G", 0, ""),
        ];
        let folders = top_level_folders(&notes, root);
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0], ("personal".into(), 1));
        assert_eq!(folders[1], ("work".into(), 2));
    }

    #[test]
    fn top_level_folders_skips_root_level_notes() {
        let root = Path::new("/root");
        let notes = vec![
            note(root.join("flat.md"), "F", 0, ""),
            note(root.join("work/inside.md"), "I", 0, ""),
        ];
        let folders = top_level_folders(&notes, root);
        assert_eq!(folders, vec![("work".into(), 1)]);
    }

    #[test]
    fn folder_candidate_uses_plural_correctly() {
        let s1 = folder_candidate("work", 1).subtitle.unwrap();
        assert!(
            s1.contains("1 note") && !s1.contains("1 notes"),
            "got {s1:?}"
        );
        assert!(folder_candidate("work", 2)
            .subtitle
            .unwrap()
            .contains("2 notes"));
        assert!(folder_candidate("work", 0)
            .subtitle
            .unwrap()
            .contains("0 notes"));
    }

    //
    // Orchestrator runs every enabled provider in parallel; if
    // the notes provider gets gated off (the user wrote
    // `"enabled": "false"` in config.json), the `#hello` path went
    // from "one create row" to "no dropdown at all". These tests pin
    // down the three behaviours the UI relies on:
    //   1. Raw provider emits a candidate for `#hello`.
    //   2. Registry surfaces it under `query_all`.
    //   3. `query_all_except(&["files","shell"])` - path the
    //      orchestrator's default mode uses - also surfaces it.
    // When the notes provider is in the disabled gate, none of the
    // above should return a candidate

    #[tokio::test]
    async fn regression_hash_dropdown_via_registry_when_enabled() {
        use crate::registry::ProviderRegistry;
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let registry = ProviderRegistry::builder().add(p).build();
        let out = registry.query_all(&Query::new("#hello")).await;
        assert!(
            !out.is_empty(),
            "#hello must produce at least one candidate via the registry"
        );
        assert!(
            out.iter().any(|c| c.id.starts_with("note::openOrCreate::")),
            "registry must surface the openOrCreate row; got ids: {:?}",
            out.iter().map(|c| &c.id).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn regression_hash_dropdown_via_query_all_except() {
        // Mirrors orchestrator's default path: files + shell
        // excluded, everything else running
        use crate::registry::ProviderRegistry;
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let registry = ProviderRegistry::builder().add(p).build();
        let out = registry
            .query_all_except(&["files", "shell"], &Query::new("#hello"))
            .await;
        assert!(
            out.iter().any(|c| c.id.starts_with("note::openOrCreate::")),
            "default orchestrator path must include the notes hash row"
        );
    }

    #[tokio::test]
    async fn regression_bare_hash_dropdown_via_registry() {
        use crate::registry::ProviderRegistry;
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let registry = ProviderRegistry::builder().add(p).build();
        let out = registry.query_all(&Query::new("#")).await;
        assert!(
            out.iter().any(|c| c.id == NEW_NOTE_PROMPT_ID),
            "bare `#` must emit the new-note prompt row"
        );
    }

    #[tokio::test]
    async fn regression_hash_yields_nothing_when_notes_gate_disables() {
        // Intentional: users CAN disable notes, and the gate honours
        // that. Silent-kill is a feature, not a bug. Test pins
        // the behaviour so a future refactor doesn't accidentally
        // ignore the gate for core keywords
        use crate::registry::{new_disabled_set, ProviderRegistry};
        use std::collections::HashSet;
        use std::sync::Arc;
        let td = TempDir::new().unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let gate = new_disabled_set();
        let mut set = HashSet::new();
        set.insert("note".to_string());
        gate.store(Arc::new(set));
        let registry = ProviderRegistry::builder_with_gate(gate).add(p).build();
        let out = registry.query_all(&Query::new("#hello")).await;
        assert!(
            out.iter().all(|c| !c.id.starts_with("note::")),
            "gate must suppress notes candidates even on the `#` path"
        );
    }

    #[tokio::test]
    async fn regression_hash_with_existing_note_shows_open_row() {
        use crate::registry::ProviderRegistry;
        let td = TempDir::new().unwrap();
        let path = td.path().join("hello.md");
        fs::write(&path, "# hello\n").unwrap();
        let notes = vec![note(path.clone(), "hello", 0, "# hello\n")];
        let p = make_test_provider(notes, td.path().to_path_buf());
        let registry = ProviderRegistry::builder().add(p).build();
        let out = registry.query_all(&Query::new("#hello")).await;
        let row = out
            .iter()
            .find(|c| c.id.starts_with("note::openOrCreate::"))
            .expect("openOrCreate row must exist");
        assert!(
            row.title.contains("Open note"),
            "existing file should yield Open, got title {:?}",
            row.title
        );
    }

    #[tokio::test]
    async fn duplicate_preserves_parent_folder() {
        let td = TempDir::new().unwrap();
        let folder = td.path().join("work");
        fs::create_dir_all(&folder).unwrap();
        let src = folder.join("sprint.md");
        fs::write(&src, "# sprint").unwrap();
        let p = make_test_provider(vec![], td.path().to_path_buf());
        let eff = p
            .activate(&format!("note::{}", src.display()), "duplicate")
            .await
            .unwrap();
        match eff {
            Effect::EditNote(dst) => {
                assert_eq!(dst.parent(), Some(folder.as_path()));
                assert!(dst.file_name().unwrap().to_str().unwrap().contains("copy"));
            }
            other => panic!("expected EditNote, got {other:?}"),
        }
    }


    #[test]
    fn scan_notes_finds_md_files_and_caches_content() {
        let td = TempDir::new().unwrap();
        write_note(td.path(), "a.md", "# A\n\nBody of A");
        write_note(td.path(), "b.markdown", "# B");
        write_note(td.path(), "c.txt", "not markdown");
        let notes = scan_notes(td.path());
        assert_eq!(notes.len(), 2);
        let titles: Vec<_> = notes.iter().map(|n| n.title.clone()).collect();
        assert!(titles.contains(&"A".to_string()));
        assert!(titles.contains(&"B".to_string()));
        let a = notes.iter().find(|n| n.title == "A").unwrap();
        assert!(a.content.contains("Body of A"));
    }

    #[test]
    fn scan_notes_falls_back_to_filestem_for_title() {
        let td = TempDir::new().unwrap();
        write_note(td.path(), "noheading.md", "just body, no heading");
        let notes = scan_notes(td.path());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "noheading");
    }

    #[test]
    fn scan_notes_orders_by_mtime_desc() {
        let td = TempDir::new().unwrap();
        write_note(td.path(), "old.md", "# old");
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_note(td.path(), "new.md", "# new");
        let notes = scan_notes(td.path());
        assert_eq!(notes[0].title, "new");
    }

    #[test]
    fn scan_notes_skips_dotfolders() {
        let td = TempDir::new().unwrap();
        let hidden = td.path().join(".cache");
        fs::create_dir_all(&hidden).unwrap();
        write_note(&hidden, "a.md", "# A");
        write_note(td.path(), "visible.md", "# Visible");
        let notes = scan_notes(td.path());
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "Visible");
    }

    #[test]
    fn scan_notes_respects_depth() {
        let td = TempDir::new().unwrap();
        let deep = td.path().join("a/b/c/d/e/f/g");
        fs::create_dir_all(&deep).unwrap();
        write_note(&deep, "too-deep.md", "# x");
        let notes = scan_notes(td.path());
        assert!(notes.is_empty());
    }

    #[test]
    fn scan_notes_caps_huge_files() {
        let td = TempDir::new().unwrap();
        let mut huge = String::with_capacity(MAX_INDEXED_BYTES + 4096);
        huge.push_str("# Huge\n\n");
        while huge.len() < MAX_INDEXED_BYTES + 4096 {
            huge.push_str("x ");
        }
        write_note(td.path(), "big.md", &huge);
        let notes = scan_notes(td.path());
        assert_eq!(notes.len(), 1);
        assert!(notes[0].content.len() <= MAX_INDEXED_BYTES);
    }


    #[test]
    fn title_from_content_first_h1() {
        assert_eq!(
            title_from_content("# My Title\n\nBody"),
            Some("My Title".into())
        );
    }

    #[test]
    fn title_from_content_skips_empty_heading() {
        assert_eq!(
            title_from_content("#    \n# Real Title\n"),
            Some("Real Title".into())
        );
    }

    #[test]
    fn title_from_content_missing_heading() {
        assert_eq!(title_from_content("No heading here\njust text"), None);
    }

    #[test]
    fn read_bounded_truncates_but_stays_utf8() {
        let td = TempDir::new().unwrap();
        let mut body = "漢".repeat((MAX_INDEXED_BYTES / 3) + 50); // each char is 3 UTF-8 bytes
        body.push('X');
        let path = write_note(td.path(), "big.md", &body);
        let s = read_bounded(&path);
        assert!(s.len() <= MAX_INDEXED_BYTES);
        assert!(s.starts_with('漢'));
    }


    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("  Two   Spaces  "), "two-spaces");
        assert_eq!(slugify("With! Punctuation?"), "with-punctuation");
    }

    #[test]
    fn slugify_unicode_alphanumerics_preserved() {
        assert_eq!(slugify("café"), "café");
        assert_eq!(slugify("Ω version 2"), "ω-version-2");
    }

    #[test]
    fn slugify_edge_cases() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn expand_tilde_bare_tilde() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde("~"), home);
        }
    }

    #[test]
    fn expand_tilde_prefix() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde("~/Documents"), home.join("Documents"));
        }
    }

    #[test]
    fn expand_tilde_absolute_unchanged() {
        assert_eq!(
            expand_tilde("/absolute/path"),
            PathBuf::from("/absolute/path")
        );
    }

    #[test]
    fn resolve_notes_folder_defaults_to_documents_gyors() {
        // This test doesn't write a config file; it exercises default
        // branch. On any dev machine with a HOME, we expect Documents/Gyors
        if let Some(home) = dirs::home_dir() {
            let folder = resolve_notes_folder();
            // Tolerate the case where a config already points elsewhere on
            // test machine - only assert default branch path when
            // no config applies
            let config_path = dirs::data_local_dir().map(|b| b.join("Gyors").join("config.json"));
            let config_has_override = config_path
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| v.get("notes_folder").cloned())
                .is_some();
            if !config_has_override {
                assert_eq!(folder, home.join("Documents").join("Gyors"));
            }
        }
    }
}
