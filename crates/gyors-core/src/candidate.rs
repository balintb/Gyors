use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type CandidateId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: CandidateId,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: Icon,
    pub kind: CandidateKind,
    pub actions: Vec<Action>,
    pub search_text: String,
    /// When true, orchestrator skips fuzzy ranking and places this
    /// candidate at top. Used by providers that produce intrinsic
    /// answers (the calculator's result, "ask AI", etc.)
    #[serde(default)]
    pub bypass_rank: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Icon {
    None,
    SfSymbol(String),
    Path(PathBuf),
    BundleIcon(PathBuf),
    /// A filled swatch rendered in the given color. Value is a `#rrggbb`
    /// hex string
    ColorSwatch(String),
    /// Render the given text string as the icon (e.g., an emoji char)
    Glyph(String),
    /// A bitmap image at the given path - loaded by Swift and shown
    /// as a thumbnail. Used for clipboard image entries so row
    /// previews actual content rather than a generic "image"
    /// placeholder. Path lives under
    /// `~/Library/Application Support/Gyors/clipboard-images/`
    ImagePath(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateKind {
    App,
    File,
    Calculation,
    Snippet,
    Clipboard,
    Web,
    Action,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub label: String,
}

impl Action {
    pub fn primary(label: impl Into<String>) -> Self {
        Self { id: "default".into(), label: label.into() }
    }

    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Effect {
    None,
    Hide,
    OpenPath(PathBuf),
    OpenUrl(String),
    CopyToClipboard(String),
    RevealInFinder(PathBuf),
    RunShell(String),
    /// Like `RunShell` but with a user-confirmation step. Used for
    /// commands whose contents come from outside trusted code -
    /// specifically, shell-mode plugin stdout (the plugin computed
    /// some string and is asking us to run it as a shell command).
    /// Swift shell shows a confirmation sheet displaying the
    /// exact text, plugin id that produced it, and Run / Cancel
    /// buttons. Run-without-confirm should NEVER be default for
    /// externally-sourced commands - that path is a remote-code
    /// execution vector if plugin gets compromised. See
    /// SECURITY.md
    ConfirmRunShell {
        command: String,
        plugin_id: String,
    },
    RunAppleScript(String),
    /// Replace the launcher's current query text with this string. Used by
    /// the CommandHintsProvider to prefill a keyword + space so user
    /// can continue typing without retyping prefix
    SetInput(String),
    /// Copy a PNG image to the system pasteboard. String is a
    /// base64-encoded PNG. Swift side decodes and writes via NSPasteboard
    CopyImagePng(String),
    /// Show a PNG image in a floating preview window. String is a
    /// base64-encoded PNG. Used by QR's "Show QR" action so users can
    /// scan with a phone without leaving the launcher
    ShowImagePng(String),
    /// Render `text` inline inside panel under a titled header.
    /// Used by conversions whose output is too large to fit in row
    /// subtitle (JSON -> TOML, pretty-printed JSON, ...) so -> previews the
    /// full result while Enter still copies
    ///
    /// `language` is a content hint the UI layer uses to pick a
    /// renderer: `"json"`, `"yaml"`, `"toml"` -> syntax-highlighted;
    /// `"markdown"` -> rendered with headings/bold/lists/code blocks;
    /// `None` -> plain monospace. All branches are gated by user's
    /// `preview_markdown` / `preview_syntax` config toggles
    ShowText {
        text: String,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// When set, this preview is for an editable file - typically
        /// a note. The UI rebinds Enter from "copy" to "edit" and routes
        /// into the inline editor without dismissing panel, so
        /// previewing then editing is a single keystroke
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editable_path: Option<PathBuf>,
    },
    /// Ask Swift shell to present a native directory picker
    /// (NSOpenPanel) and, on selection, write the chosen path under
    /// `config_key`. The `prompt` is the dialog's message text so the
    /// user knows which setting they're picking for. Used by
    /// `Path`-typed config fields (notes_folder today, other folder
    /// settings tomorrow) so user doesn't have to type or paste
    /// paths by hand
    PickDirectory {
        config_key: String,
        prompt: String,
    },
    /// Dispatch an AI query. String is user's question. Swift
    /// shell decides provider (Ollama / OpenAI / Anthropic) from config
    AskAi(String),
    /// Dispatch an AI transform - `instruction` is a directive (e.g.
    /// "Summarize concisely, max 3 sentences"), `text` is the content
    /// to transform. Swift shell routes these as system+user
    /// messages for chat-capable backends and as a merged prompt for
    /// Ollama's /api/generate. Used by `AiTransformsProvider`
    /// (summarize / explain / translate / rewrite / fix ...)
    AiTransform { text: String, instruction: String },
    /// Run an AI call, then pipe its answer through a pipeline. The
    /// Swift shell calls `AiClient.ask` (when `instruction` is None) or
    /// `AiClient.transform` (when set), then re-dispatches answer
    /// as source of a `pipeline::<base64>` activation so the
    /// existing `pipeline::execute` handler runs the stages.
    /// Lets `ai write a haiku | upper | copy` route AI output through
    /// same chain machinery as static-text sources
    AskAiThenPipe {
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instruction: Option<String>,
        stages: Vec<String>,
    },
    /// Resize/move the current frontmost window. String identifies a
    /// predefined geometry (`left-half`, `right-half`, ...). Swift uses
    /// AXUIElement rather than AppleScript so only Accessibility
    /// permission is needed (no Automation/AppleEvent TCC prompt)
    ArrangeWindow(String),
    /// Empty user's Trash natively via Swift's FileManager. Avoids the
    /// AppleScript/Automation TCC permission that blocks `tell Finder` when
    /// the bundle signature shifts across rebuilds
    EmptyTrash,
    /// Open the inline markdown editor on this file path. Handled by the
    /// ViewModel, which switches panel into editor mode
    EditNote(PathBuf),
    /// Move the given file to user's Trash via Swift's native
    /// `FileManager.trashItem` (undoable from Finder, no TCC dance)
    TrashFile(PathBuf),
    /// Start a live countdown in menu bar, fire a user notification
    /// when it reaches zero. `label` is optional; used in the
    /// notification body so user remembers which timer went off
    StartTimer { secs: u64, label: String },
    /// Cancel every running timer immediately
    CancelTimers,
    /// Open a small window listing running timers with their remaining
    /// time - discoverability for users who forget what they started
    ListTimers,
    /// Run `command` in a fresh Terminal window. Swift writes the
    /// command to a temp `.command` file and opens it with `open -a
    /// Terminal` - that path goes through Launch Services, not
    /// Apple Events, so it doesn't trip the Automation TCC class.
    /// Earlier `tell application "Terminal" to do script ...` worked
    /// but required user to grant "Gyors -> Terminal" Automation
    /// permission, which silently fails on Sequoia until allowed
    OpenInTerminal(String),
    /// Activate a theme by id at runtime. Swift `ThemeManager`
    /// updates `current` and writes the choice to disk in one
    /// step, so `config set theme <id>` (which already writes the
    /// JSON) re-applies live without restart. Without this effect,
    /// changing themes via config UI updated file but the
    /// running app kept rendering in the old theme until relaunch
    ApplyTheme(String),
    Notification { title: String, body: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_primary_uses_default_id() {
        let a = Action::primary("Open");
        assert_eq!(a.id, "default");
        assert_eq!(a.label, "Open");
    }

    #[test]
    fn bypass_rank_defaults_false_when_missing_from_json() {
        // Older serialized Candidates (before field was introduced)
        // should deserialize with bypass_rank = false
        let json = r#"{
            "id": "x",
            "title": "T",
            "subtitle": null,
            "icon": "None",
            "kind": "App",
            "actions": [],
            "search_text": "T"
        }"#;
        let c: Candidate = serde_json::from_str(json).unwrap();
        assert!(!c.bypass_rank);
        assert_eq!(c.kind, CandidateKind::App);
    }

    #[test]
    fn icon_variants_roundtrip() {
        for icon in [
            Icon::None,
            Icon::SfSymbol("waveform".into()),
            Icon::Path(PathBuf::from("/x")),
            Icon::BundleIcon(PathBuf::from("/Applications/Safari.app")),
        ] {
            let s = serde_json::to_string(&icon).unwrap();
            let _back: Icon = serde_json::from_str(&s).unwrap();
        }
    }
}
