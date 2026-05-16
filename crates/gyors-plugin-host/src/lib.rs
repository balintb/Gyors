//! Plugin host
//!
//! Two flavours, both registered as `Provider`s in the main registry
//! and treated identically from that point forward (chain/pipeline
//! integration, config-toggle UI, frecency ranking all "for free"):
//!
//! - Shell plugins - inline entries in
//!   `~/Library/Application Support/Gyors/plugins.json`. Each entry
//!   specifies a keyword list and a shell command with `{query}`
//!   substitution. Zero-executable plugins - perfect for wrapping
//!   a one-liner like `curl wttr.in/{query}`. See `ShellPlugin`
//!
//! - Process plugins - executables in
//!   `~/Library/Application Support/Gyors/plugins/` that implement
//!   the three-subcommand JSON protocol below. Full programmatic
//!   control over candidate generation + activation effects
//!
//! Process plugins use this protocol:
//!
//! ```text
//!   <plugin-bin> info
//!     -> stdout: {"id": "...", "name": "...", "keywords": ["k1", ...], "description": "..."}
//!
//!   <plugin-bin> query <keyword> <filter>
//!     -> stdout: [<Candidate JSON>, ...]
//!
//!   <plugin-bin> activate <candidate-id> <action-id>
//!     -> stdout: <Effect JSON>
//! ```
//!
//! Exit code 0 = success. Non-zero = treat as "no results" / "no-op
//! effect" and log a warning. Plugins write to stderr for
//! diagnostics; Gyors captures and logs those on failure
//!
//! Design choices:
//!
//! - Keyword-gated. A plugin's `query` is only invoked when the
//!   user's input starts with one of its declared keywords. That
//!   keeps the hot path cheap (no fork-per-keystroke for idle
//!   plugins) and the mental model obvious
//!
//! - Per-query process spawn. Simple to reason about, no stdio
//!   pipe plumbing, crash isolation is trivial (plugin OOM doesn't
//!   take Gyors with it). Timeout via `tokio::time::timeout` so a
//!   slow plugin can't freeze the launcher
//!
//! - Untyped metadata via JSON. Plugin authors can use any
//!   language. Rust plugins get it for free via serde on Candidate /
//!   Effect; shell plugins jq-assemble strings
//!
//! Escape hatch: plugins whose `info` times out or fails are simply
//! skipped at discovery. They dont block startup

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use gyors_core::{Candidate, CandidateId, Effect, Provider, Query};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Metadata a plugin advertises via `info`. Rust side only reads
/// fields it uses today; extra keys plugin emits are
/// tolerated so protocol can grow without breaking old Gyorss
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Unique id. Used as provider id + prefix of all
    /// candidate ids plugin emits (enforced at registration)
    pub id: String,
    /// Human-readable name shown in diagnostics / About pane
    pub name: String,
    /// Short description for discoverability docs
    #[serde(default)]
    pub description: String,
    /// Keywords that trigger this plugin. User's input must
    /// start with one of these (followed by a space, or end-of-
    /// input for the bare-keyword case) for plugin to run
    pub keywords: Vec<String>,
}

/// Handle for an on-disk plugin executable. Keeps the binary path
/// + parsed manifest so we dont re-invoke `info` on every query
pub struct Plugin {
    pub info: PluginInfo,
    pub path: PathBuf,
}

/// Hard timeouts per sub-invocation. Prevents a runaway plugin from
/// freezing the launcher. `info` happens once at startup; `query`
/// is hot-path; `activate` is a user-initiated action so we allow a
/// bit more headroom
const INFO_TIMEOUT: Duration = Duration::from_secs(2);
const QUERY_TIMEOUT: Duration = Duration::from_millis(1500);
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);

impl Plugin {
    /// Ask a plugin executable to describe itself. Runs with a tight
    /// timeout so a misbehaving plugin can't stall startup
    pub async fn load(path: PathBuf) -> Result<Self> {
        let stdout = run_plugin(&path, &["info"], INFO_TIMEOUT).await?;
        let info: PluginInfo = serde_json::from_slice(&stdout)
            .with_context(|| format!("plugin `info` not valid JSON: {}", path.display()))?;
        if info.id.trim().is_empty() {
            bail!("plugin at {} declared empty id", path.display());
        }
        if info.keywords.is_empty() {
            bail!("plugin {} declared no keywords", info.id);
        }
        Ok(Self { info, path })
    }

    /// Invoke plugin's `query` subcommand for a given keyword +
    /// filter string. Returns an empty list on timeout / non-zero
    /// exit - we log the failure but never surface a "plugin
    /// crashed" row in user's results, since that's noise
    async fn query_impl(&self, keyword: &str, filter: &str) -> Vec<Candidate> {
        let args: [&str; 3] = ["query", keyword, filter];
        match run_plugin(&self.path, &args, QUERY_TIMEOUT).await {
            Ok(bytes) => match serde_json::from_slice::<Vec<Candidate>>(&bytes) {
                Ok(mut cands) => {
                    // Namespace every candidate id with plugin
                    // id so activation routing knows which plugin
                    // to dispatch to. If plugin already prefixed
                    // correctly, leave it alone
                    let prefix = format!("{}::", self.info.id);
                    for c in &mut cands {
                        if !c.id.starts_with(&prefix) {
                            c.id = format!("{prefix}{}", c.id);
                        }
                    }
                    cands
                }
                Err(e) => {
                    tracing::warn!(
                        plugin = %self.info.id,
                        error = %e,
                        "plugin query JSON decode failed"
                    );
                    Vec::new()
                }
            },
            Err(e) => {
                tracing::warn!(plugin = %self.info.id, error = %e, "plugin query failed");
                Vec::new()
            }
        }
    }

    /// Invoke plugin's `activate` subcommand. The returned
    /// Effect flows back through the normal Effect-dispatch path
    /// (Swift runs it). Errors land as `Effect::None` so user
    /// sees a no-op rather than a crash
    async fn activate_impl(&self, id: &str, action: &str) -> Result<Effect> {
        // Strip provider-id prefix - plugins emit ids in their
        // own namespace, we added prefix at query-time for
        // routing. Pass the stripped id back so plugin sees
        // what it emitted
        let prefix = format!("{}::", self.info.id);
        let stripped = id.strip_prefix(&prefix).unwrap_or(id);
        let stdout = run_plugin(
            &self.path,
            &["activate", stripped, action],
            ACTIVATE_TIMEOUT,
        )
        .await?;
        let eff: Effect = serde_json::from_slice(&stdout)
            .with_context(|| "plugin activate output was not valid Effect JSON")?;
        Ok(eff)
    }

    /// Does user's input pattern invoke this plugin? Matches
    /// when `pattern` equals (case-insensitive) any declared
    /// keyword, OR starts with one followed by whitespace. The
    /// returned filter preserves user's original casing - only
    /// keyword match is ASCII-case-blind
    fn matches(&self, pattern: &str) -> Option<(&str, String)> {
        let trimmed = pattern.trim();
        for kw in &self.info.keywords {
            if trimmed.eq_ignore_ascii_case(kw) {
                return Some((kw.as_str(), String::new()));
            }
            if trimmed.len() > kw.len() {
                let (head, rest) = trimmed.split_at(kw.len());
                if head.eq_ignore_ascii_case(kw) && rest.starts_with(char::is_whitespace) {
                    return Some((kw.as_str(), rest.trim_start().to_string()));
                }
            }
        }
        None
    }
}

#[async_trait]
impl Provider for Plugin {
    fn id(&self) -> &str {
        &self.info.id
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some((kw, filter)) = self.matches(query.pattern()) else {
            return Vec::new();
        };
        self.query_impl(kw, &filter).await
    }

    async fn activate(&self, id: &CandidateId, action: &str) -> Result<Effect> {
        self.activate_impl(id, action).await
    }
}

/// Run a plugin subcommand and return its stdout bytes. Fails the
/// future on timeout, non-zero exit, or IO error
async fn run_plugin(bin: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(std::process::Stdio::null());
    // Scrub the inherited environment. The launcher's
    // shell env can include credentials user exported for
    // their dev tools (OPENAI_API_KEY, GITHUB_TOKEN, AWS_*,
    // OP_SESSION_*). A plugin that prints `std::env::vars()` (or
    // exfiltrates via DNS) walks off with the lot. Plugins get a
    // minimal allowlist: PATH (so they can find /usr/bin tools),
    // HOME (so they can find user dirs), LANG (so libc locale-
    // aware code doesn't crash). Everything else stays in our
    // process. If a plugin needs a specific env var, declare it
    // explicitly in plugins.json (future) - implicit inheritance
    // is the bug
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    if let Ok(lang) = std::env::var("LANG") {
        cmd.env("LANG", lang);
    }
    // Without `kill_on_drop`, a timeout that calls `child.kill().await`
    // and then returns leaves the SIGKILL'd process as a zombie until
    // its parent (us) calls `wait()`. The `let _ = child.kill().await`
    // path below doesn't wait, and the `child` then drops with no
    // automatic reap. Setting `kill_on_drop(true)` enables Tokio's
    // drop impl to issue both kill and wait, so timed-out plugins
    // never accumulate zombies
    cmd.kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    // Cap stdout/stderr reads. A plugin that emits bytes
    // faster than the wall-clock timeout fires (or just pipes
    // /dev/zero) would otherwise grow buffers until the OS
    // OOM-killed the launcher. `Take` wraps each pipe and reports
    // EOF after the limit, so buffer can hold at most CAP+1
    // bytes. We read one byte past the cap so we can DETECT
    // overflow afterwards (buf.len() > cap) and surface as a
    // proper error
    let mut stdout_pipe = AsyncReadExt::take(
        child.stdout.take().expect("stdout piped"),
        (MAX_PLUGIN_STDOUT_BYTES + 1) as u64,
    );
    let mut stderr_pipe = AsyncReadExt::take(
        child.stderr.take().expect("stderr piped"),
        (MAX_PLUGIN_STDERR_BYTES + 1) as u64,
    );

    let run = async {
        let stdout_fut = stdout_pipe.read_to_end(&mut stdout_buf);
        let stderr_fut = stderr_pipe.read_to_end(&mut stderr_buf);
        let wait_fut = child.wait();
        let (_, _, status) = tokio::join!(stdout_fut, stderr_fut, wait_fut);
        Ok::<_, anyhow::Error>(status?)
    };

    let status = match tokio::time::timeout(timeout, run).await {
        Ok(r) => r?,
        Err(_) => {
            let _ = child.kill().await;
            bail!("plugin {} timed out after {:?}", bin.display(), timeout);
        }
    };

    if stdout_buf.len() > MAX_PLUGIN_STDOUT_BYTES {
        bail!(
            "plugin {} stdout exceeded {} bytes",
            bin.display(),
            MAX_PLUGIN_STDOUT_BYTES
        );
    }
    if stderr_buf.len() > MAX_PLUGIN_STDERR_BYTES {
        bail!(
            "plugin {} stderr exceeded {} bytes",
            bin.display(),
            MAX_PLUGIN_STDERR_BYTES
        );
    }

    if !status.success() {
        let err = String::from_utf8_lossy(&stderr_buf);
        bail!(
            "plugin {} exited with {status} - stderr: {}",
            bin.display(),
            err.trim()
        );
    }
    Ok(stdout_buf)
}

/// Per-subprocess output caps. Plugins that emit faster
/// than timeout fires (or pipe `/dev/zero`) would otherwise
/// grow the read buffer until the OS OOM-killed the launcher.
/// Stdout is the larger of the two because legitimate plugins do
/// emit thousands of result rows; stderr is small because nothing
/// legitimate prints a 4MB error message
pub const MAX_PLUGIN_STDOUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PLUGIN_STDERR_BYTES: usize = 1024 * 1024;

/// Default plugin directory under user's support directory.
/// Tests redirect via `GYORS_PLUGINS_DIR` so they dont depend on
/// the developer's real ~/Library/Application Support/Gyors/plugins
pub fn default_plugin_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("GYORS_PLUGINS_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    dirs::data_local_dir()
        .map(|b| b.join("Gyors").join("plugins"))
        .unwrap_or_else(|| PathBuf::from("plugins"))
}

/// Hard cap on plugins discovered per launch. Same-user
/// malware that drops 10,000 stub binaries into plugins dir
/// would otherwise force the launcher to spawn 10,000 `info`
/// subprocesses on startup. The cap is alphabetical by filename so
/// "which 50 got loaded" is predictable rather than filesystem-
/// order roulette
pub const MAX_PLUGIN_DISCOVERY: usize = 50;

/// Scan `dir` for executable files and load each as a Plugin.
/// Malformed plugins are logged and skipped - one bad apple never
/// kills the launcher. Returns an empty list when the directory
/// doesn't exist (common on fresh installs)
///
/// Capped at `MAX_PLUGIN_DISCOVERY` entries . When the
/// directory holds more than the cap we sort filenames and load
/// the first N, warning once at discovery time so user sees
/// in their logs that some plugins were skipped
pub async fn discover(dir: &Path) -> Vec<Plugin> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_executable_file(p))
        .collect();
    // Deterministic order so cap-truncation cut point is
    // reproducible. Sort by full path - filenames in a single dir
    // are unique so this collapses to filename ordering
    candidates.sort();
    let total = candidates.len();
    if total > MAX_PLUGIN_DISCOVERY {
        tracing::warn!(
            total,
            cap = MAX_PLUGIN_DISCOVERY,
            "plugin dir exceeds cap; loading first {} alphabetically",
            MAX_PLUGIN_DISCOVERY,
        );
        candidates.truncate(MAX_PLUGIN_DISCOVERY);
    }
    let mut plugins = Vec::with_capacity(candidates.len());
    for path in candidates {
        match Plugin::load(path.clone()).await {
            Ok(p) => {
                tracing::info!(
                    plugin = %p.info.id,
                    keywords = ?p.info.keywords,
                    path = %path.display(),
                    "plugin loaded"
                );
                plugins.push(p);
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "plugin load failed; skipping"
                );
            }
        }
    }
    plugins
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    // Executable-by-owner is the cheapest proxy for "meant to be
    // run." Plugins are usually chmod +x'd by their author
    let mode = meta.permissions().mode();
    (mode & 0o100) != 0
}

//
// Inline JSON plugins. No separate executable - plugin is a
// shell-command template declared in `plugins.json`. Gyors spawns
// `sh -c` with user's filter substituted for `{query}`, captures
// stdout, and shows it as a single row

/// What to do when user activates (Enter) the output row
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ShellActivation {
    /// Copy the full output to clipboard. Default - matches
    /// the "I wanted that string" instinct
    #[default]
    Copy,
    /// Treat the output as a URL (or shell-openable path) and hand
    /// it to macOS's `open`
    Open,
    /// Execute the output as a shell command. Dangerous - opt-in via
    /// explicit configuration only
    Shell,
    /// No-op. Useful when the command itself already has side
    /// effects (e.g. `say`, `afplay`) and the output is informational
    None,
}


/// Raw `plugins.json` entry. Deserialized as-is; wrapped in a
/// `ShellPlugin` for the Provider impl
///
/// Serializable too - the install flow reads user-provided JSON
/// (from a URI / `.gyorsplugin` file), validates it as a spec, and
/// writes it back into user's `plugins.json`. `version` /
/// `source_url` / `author` are optional metadata that make installed
/// plugins id-d + update-able; shell plugins written by hand can
/// leave them out
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ShellPluginSpec {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub keywords: Vec<String>,
    /// Shell command with `{query}` placeholder. Run via `sh -c`
    /// after `{query}` is substituted with shell-escaped filter
    /// text user typed
    pub command: String,
    #[serde(default, skip_serializing_if = "is_default_activation")]
    pub on_activate: ShellActivation,
    /// Optional SF Symbol name (e.g. `cloud.sun`, `hammer`). Falls
    /// back to a generic terminal glyph
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Per-plugin command timeout. Default 2000 ms
    #[serde(default = "default_shell_timeout_ms")]
    pub timeout_ms: u64,
    /// Installed-package metadata - populated when plugin came
    /// from a `.gyorsplugin` manifest or a `gyors://plugin/install`
    /// URI. Hand-authored plugins typically leave these empty
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Homepage / repo URL. Used by the Plugins UI to surface a
    /// "Show Source" action and (future) auto-update checks
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

fn default_shell_timeout_ms() -> u64 {
    2000
}

fn is_default_activation(a: &ShellActivation) -> bool {
    matches!(a, ShellActivation::Copy)
}

pub struct ShellPlugin {
    pub spec: ShellPluginSpec,
}

impl ShellPlugin {
    pub fn new(spec: ShellPluginSpec) -> Result<Self> {
        if spec.id.trim().is_empty() {
            bail!("shell plugin has empty id");
        }
        if spec.keywords.is_empty() {
            bail!("shell plugin `{}` declared no keywords", spec.id);
        }
        if spec.command.trim().is_empty() {
            bail!("shell plugin `{}` has empty command", spec.id);
        }
        Ok(Self { spec })
    }

    /// Return shell command that `sh -c` will actually run,
    /// with user's query interpolated and escaped. Used by the
    /// `gyors plugin test` dev tool so plugin authors can see what's
    /// being executed without a live Gyors panel. Kept separate from
    /// `run_command` (which also spawns the process) so test
    /// tool can dry-run first and add its own exec policy
    pub fn rendered_command(&self, query: &str) -> String {
        self.spec.command.replace("{query}", &shell_escape(query))
    }

    /// Mirror of `Plugin::matches` - only invoke the command when the
    /// user's input starts with one of our keywords
    fn matches(&self, pattern: &str) -> Option<String> {
        let trimmed = pattern.trim();
        for kw in &self.spec.keywords {
            if trimmed.eq_ignore_ascii_case(kw) {
                return Some(String::new());
            }
            if trimmed.len() > kw.len() {
                let (head, rest) = trimmed.split_at(kw.len());
                if head.eq_ignore_ascii_case(kw) && rest.starts_with(char::is_whitespace) {
                    return Some(rest.trim_start().to_string());
                }
            }
        }
        None
    }

    async fn run_command(&self, query: &str) -> String {
        let cmd = self.spec.command.replace("{query}", &shell_escape(query));
        let timeout = Duration::from_millis(self.spec.timeout_ms);
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(&cmd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            // Same anti-zombie reason as `run_plugin`: timeout
            // path kills the child and drops it without an
            // explicit wait
            .kill_on_drop(true);
        // See comment in `run_plugin`. Same scrub: drop
        // every env var, restore only the allowlist a shell needs
        // to find tools + locale
        command.env_clear();
        command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
        if let Ok(home) = std::env::var("HOME") {
            command.env("HOME", home);
        }
        if let Ok(lang) = std::env::var("LANG") {
            command.env("LANG", lang);
        }
        let Ok(mut child) = command.spawn() else {
            return String::new();
        };
        let mut stdout_buf = Vec::new();
        let raw_stdout = match child.stdout.take() {
            Some(p) => p,
            None => return String::new(),
        };
        // Same OOM defence as typed-plugin path.
        // ShellPlugins are also user-supplied subprocesses; a `cat
        // /dev/zero` in plugins.json would otherwise grow the
        // buffer unbounded until the OS killed the launcher
        let mut stdout_pipe =
            AsyncReadExt::take(raw_stdout, (MAX_PLUGIN_STDOUT_BYTES + 1) as u64);
        let run = async {
            let _ = stdout_pipe.read_to_end(&mut stdout_buf).await;
            let _ = child.wait().await;
        };
        if tokio::time::timeout(timeout, run).await.is_err() {
            let _ = child.kill().await;
            return String::new();
        }
        if stdout_buf.len() > MAX_PLUGIN_STDOUT_BYTES {
            tracing::warn!(
                plugin = %self.spec.id,
                cap = MAX_PLUGIN_STDOUT_BYTES,
                "shell plugin stdout exceeded cap; output discarded"
            );
            return String::new();
        }
        String::from_utf8_lossy(&stdout_buf)
            .trim_end_matches('\n')
            .to_string()
    }
}

#[async_trait]
impl Provider for ShellPlugin {
    fn id(&self) -> &str {
        &self.spec.id
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some(filter) = self.matches(query.pattern()) else {
            return Vec::new();
        };
        let output = self.run_command(&filter).await;
        if output.is_empty() {
            return Vec::new();
        }
        // Subtitle shows plugin name + activation hint so the
        // user knows what Enter does. Title carries the output
        // trimmed to one line so it stays list-shaped
        let first_line = output.lines().next().unwrap_or("").to_string();
        let activation_hint = match self.spec.on_activate {
            ShellActivation::Copy => "↵ copy",
            ShellActivation::Open => "↵ open",
            ShellActivation::Shell => "↵ run",
            ShellActivation::None => "↵ ok",
        };
        let subtitle = if output.lines().count() > 1 {
            format!(
                "{} · {activation_hint} · {} lines",
                self.spec.name,
                output.lines().count()
            )
        } else {
            format!("{} · {activation_hint}", self.spec.name)
        };
        use gyors_core::{Action, CandidateKind, Icon};
        // Payload-in-id pattern: encode the output so activation can
        // retrieve it without re-running the command. Kept base64-
        // url-safe so it's a single token over FFI. For multi-line
        // output we're capped at a few KB; larger would land in the
        // activation path's second `run_command` call instead
        use base64::prelude::*;
        let payload = BASE64_URL_SAFE_NO_PAD.encode(output.as_bytes());
        let id = format!("{}::{payload}", self.spec.id);
        let icon_name = self.spec.icon.clone().unwrap_or_else(|| "terminal".into());
        vec![Candidate {
            id,
            title: if first_line.is_empty() {
                self.spec.name.clone()
            } else {
                first_line
            },
            subtitle: Some(subtitle),
            icon: Icon::SfSymbol(icon_name),
            kind: CandidateKind::Custom("shell-plugin".into()),
            actions: vec![Action::primary(activation_hint)],
            search_text: String::new(),
            bypass_rank: true,
        }]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> Result<Effect> {
        use base64::prelude::*;
        let prefix = format!("{}::", self.spec.id);
        let payload = id
            .strip_prefix(&prefix)
            .ok_or_else(|| anyhow::anyhow!("id not ours: {id}"))?;
        let bytes = BASE64_URL_SAFE_NO_PAD
            .decode(payload)
            .context("shell plugin id base64")?;
        let output = String::from_utf8(bytes).context("shell plugin id utf8")?;
        Ok(match self.spec.on_activate {
            ShellActivation::Copy => Effect::CopyToClipboard(output),
            ShellActivation::Open => Effect::OpenUrl(output),
            // Shell-mode plugins emit a CONFIRMING variant
            // so Swift shell shows the exact command + plugin id
            // before /bin/sh sees it. A plugin that was benign at
            // install time can change its stdout after a self-update,
            // so each activation re-trusts via user confirmation.
            // Plugin id is threaded through so UI can
            // surface "<Plugin Name> wants to run:"
            ShellActivation::Shell => Effect::ConfirmRunShell {
                command: output,
                plugin_id: self.spec.id.clone(),
            },
            ShellActivation::None => Effect::None,
        })
    }
}

/// Default `plugins.json` path. Honors `GYORS_CONFIG_DIR` so tests
/// redirect via same override they use for the main config
pub fn default_shell_plugins_path() -> PathBuf {
    if let Ok(dir) = std::env::var("GYORS_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("plugins.json");
        }
    }
    dirs::data_local_dir()
        .map(|b| b.join("Gyors").join("plugins.json"))
        .unwrap_or_else(|| PathBuf::from("plugins.json"))
}

/// Hard cap on a single `plugins.json`. Legitimate
/// manifests are kilobytes; anything north of this is either a bug
/// or an attempt to OOM the launcher by handing it a massive blob
/// to deserialize. The FFI install path caps at 4MB via
/// `MAX_JSON_LEN`; this is the disk-reload mirror
pub const MAX_PLUGINS_JSON_BYTES: u64 = 256 * 1024;

/// Read a JSON file under a stat-and-take cap. Refuses entries
/// whose `metadata().len()` is over the cap before opening, and
/// also re-checks after the read so a TOCTOU race that grows the
/// file mid-read can't slip past
fn read_file_capped(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::Read;
    let meta = std::fs::metadata(path)?;
    if meta.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "file {} is {} bytes, cap is {}",
                path.display(),
                meta.len(),
                max_bytes,
            ),
        ));
    }
    let mut f = std::fs::File::open(path)?;
    let mut s = String::new();
    f.by_ref()
        .take(max_bytes + 1)
        .read_to_string(&mut s)?;
    if s.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file {} grew past cap mid-read", path.display()),
        ));
    }
    Ok(s)
}

/// Read + parse `plugins.json`, returning one `ShellPlugin` per
/// entry. Missing file -> empty list (common on fresh installs).
/// Malformed entries are logged and dropped; rest still load
pub fn load_shell_plugins(path: &Path) -> Vec<ShellPlugin> {
    let Ok(text) = read_file_capped(path, MAX_PLUGINS_JSON_BYTES) else {
        return Vec::new();
    };
    // Accept two shapes: a top-level array, or an object with a
    // `plugins` array. The object form leaves room for future
    // top-level settings (timeouts, paths) without a migration
    let specs: Vec<ShellPluginSpec> = match serde_json::from_str::<Vec<ShellPluginSpec>>(&text) {
        Ok(v) => v,
        Err(_) => match serde_json::from_str::<ShellPluginsFile>(&text) {
            Ok(f) => f.plugins,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "plugins.json parse failed; ignoring"
                );
                return Vec::new();
            }
        },
    };
    let mut out = Vec::new();
    for spec in specs {
        match ShellPlugin::new(spec) {
            Ok(p) => {
                tracing::info!(
                    plugin = %p.spec.id,
                    keywords = ?p.spec.keywords,
                    "shell plugin loaded"
                );
                out.push(p);
            }
            Err(e) => {
                tracing::warn!(error = %e, "shell plugin skipped");
            }
        }
    }
    out
}

#[derive(Deserialize)]
struct ShellPluginsFile {
    plugins: Vec<ShellPluginSpec>,
}

/// Public wrapper around the installer's internal id check. Useful
/// for developer tools (`gyors plugin validate`, CI linters) that want
/// to run same gate without taking a dependency on `upsert_*`
/// side effects. Validates the id's charset, length, and the
/// reserved-word list against built-in provider keywords
pub fn validate_spec(spec: &ShellPluginSpec) -> Result<()> {
    validate_id(&spec.id)?;
    Ok(())
}

/// Insert a plugin spec into `plugins.json`, replacing any existing
/// entry with same `id`. Creates file (array form) if it
/// doesn't exist. Returns `Replaced` when an upsert happened, `Added`
/// for a new entry
pub fn upsert_shell_plugin(path: &Path, spec: ShellPluginSpec) -> Result<UpsertOutcome> {
    validate_id(&spec.id)?;
    let mut specs = read_specs(path)?;
    let mut outcome = UpsertOutcome::Added;
    if let Some(existing) = specs.iter_mut().find(|s| s.id == spec.id) {
        *existing = spec;
        outcome = UpsertOutcome::Replaced;
    } else {
        specs.push(spec);
    }
    write_specs(path, &specs)?;
    Ok(outcome)
}

/// Remove plugin with the given id. Returns `Ok(true)` if the
/// file changed, `Ok(false)` if the id wasn't present
pub fn remove_shell_plugin(path: &Path, id: &str) -> Result<bool> {
    let mut specs = read_specs(path)?;
    let before = specs.len();
    specs.retain(|s| s.id != id);
    if specs.len() == before {
        return Ok(false);
    }
    write_specs(path, &specs)?;
    Ok(true)
}

/// Look up a single plugin spec by id. Useful for update checks and
/// "is this already installed?" queries before the confirmation
/// modal fires
pub fn find_shell_plugin(path: &Path, id: &str) -> Option<ShellPluginSpec> {
    let specs = read_specs(path).ok()?;
    specs.into_iter().find(|s| s.id == id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Added,
    Replaced,
}

/// Read the `plugins.json` file (accepting both array and object
/// shapes), returning parsed specs. Missing file -> empty list
/// is the "nothing yet" state, not an error
fn read_specs(path: &Path) -> Result<Vec<ShellPluginSpec>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = read_file_capped(path, MAX_PLUGINS_JSON_BYTES)
        .with_context(|| format!("read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Accept either shape. Prefer array; fall back to object wrap
    match serde_json::from_str::<Vec<ShellPluginSpec>>(&text) {
        Ok(v) => Ok(v),
        Err(_) => {
            let wrap: ShellPluginsFile =
                serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
            Ok(wrap.plugins)
        }
    }
}

/// Write specs back as a pretty-printed JSON array (the simpler
/// shape). Creates the parent dir if needed. Atomic-ish: writes to
/// a temp file then renames so a crashed mid-write can't leave the
/// user with an empty or half-written `plugins.json`
fn write_specs(path: &Path, specs: &[ShellPluginSpec]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let text = serde_json::to_string_pretty(specs).context("serialize plugins")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Reject ids that would collide with built-in providers or inject
/// path segments. Plugin authors get a clean error, users are
/// protected from "install this harmless weather plugin" links
/// shadowing a core keyword like `config`
fn validate_id(id: &str) -> Result<()> {
    let id = id.trim();
    if id.is_empty() {
        bail!("plugin id cannot be empty");
    }
    // Alphanumeric plus `-_` only. Protects against path-traversal
    // attempts via `id`, and keeps ids filesystem-safe for any
    // future per-plugin caches
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("plugin id `{id}` must be [A-Za-z0-9_-]");
    }
    // Built-in provider ids that must never be shadowed. Keep in
    // sync with `gyors-providers::registry::ALWAYS_ON_PROVIDERS` and
    // provider ids registered in `gyors-ipc::GyorsBridge::new`
    const RESERVED: &[&str] = &[
        "apps",
        "calc",
        "clip",
        "config",
        "shell",
        "note",
        "files",
        "emoji",
        "color",
        "time",
        "timer",
        "units",
        "currency",
        "hint",
        "kill",
        "qr",
        "ai",
        "ait",
        "ssh",
        "json",
        "jwt",
        "regex",
        "tab",
        "recent",
        "case",
        "text",
        "sys",
        "pref",
        "screenshot",
        "snip",
        "window",
        "generators",
        "repo",
        "web",
        "chain",
        "pipeline",
        "autoclose",
        "ipc",
        "scratch",
        "scratchpad",
        "cron",
    ];
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(id)) {
        bail!("plugin id `{id}` is reserved for a built-in provider");
    }
    Ok(())
}

//
// A `.gyorsplugin` file is a JSON manifest that wraps a plugin spec
// with install metadata (version, source_url, manifest version).
// Distributable, update-able, and clickable: macOS can associate
// the extension so double-clicking opens Gyors, which reads the
// manifest and runs the install flow

/// Current manifest schema version. Bump on breaking changes; older
/// Gyorss reading a newer manifest will report "unsupported" instead
/// of installing something they dont understand
pub const MANIFEST_VERSION: u32 = 1;

/// Parsed `.gyorsplugin` file. Fields match JSON exactly
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginManifest {
    /// Schema version - always `1` in this release. Consumers check
    /// this before trusting any other field
    pub gyors_plugin_manifest: u32,
    /// Shell plugin spec to install. Versioned + source-tagged
    pub shell: Option<ShellPluginSpec>,
    // `process: Option<ProcessPluginManifest>` reserved for future
    // use (distributes a prebuilt binary). Today process plugins
    // install by copying files into plugin dir manually
}

impl PluginManifest {
    /// Parse a JSON string as a manifest, verifying schema
    /// version is one we understand and that at least one plugin
    /// kind is present
    pub fn parse(s: &str) -> Result<Self> {
        let m: PluginManifest = serde_json::from_str(s).context("manifest JSON")?;
        if m.gyors_plugin_manifest != MANIFEST_VERSION {
            bail!(
                "unsupported manifest version {} (expected {MANIFEST_VERSION})",
                m.gyors_plugin_manifest,
            );
        }
        if m.shell.is_none() {
            bail!("manifest declares no plugin");
        }
        if let Some(spec) = &m.shell {
            validate_id(&spec.id)?;
        }
        Ok(m)
    }

    /// Produce a canonical pretty-printed JSON form for writing to a
    /// `.gyorsplugin` file
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Minimal shell-escape for `{query}` substitution. Wraps in single
/// quotes and escapes any embedded single quote. Prevents basic
/// injection so `{query}` can't break out of an argument and run
/// arbitrary shell. Rigid-style - we dont try to be clever
fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            // Close quote, emit escaped quote, reopen quote
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted fake plugin: writes a fixed info/query/activate
    /// response, so tests cover protocol end-to-end without
    /// depending on a real external binary
    fn write_fake_plugin(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[tokio::test]
    async fn loads_plugin_info_from_script() {
        let td = tempfile::tempdir().unwrap();
        let path = write_fake_plugin(
            td.path(),
            "greeter",
            r#"#!/bin/sh
case "$1" in
  info)
    cat <<EOF
{"id":"greeter","name":"Greeter","description":"says hi","keywords":["greet","hi"]}
EOF
    ;;
  query)
    cat <<EOF
[{"id":"row1","title":"Hello $3","subtitle":"from plugin","icon":{"SfSymbol":"hand.wave"},"kind":"Action","actions":[{"id":"default","label":"Wave"}],"search_text":"","bypass_rank":false}]
EOF
    ;;
  activate)
    echo '{"Notification":{"title":"hi","body":null}}'
    ;;
esac
"#,
        );
        let p = Plugin::load(path).await.unwrap();
        assert_eq!(p.info.id, "greeter");
        assert_eq!(p.info.keywords, vec!["greet", "hi"]);
    }

    #[tokio::test]
    async fn matches_keyword_variants() {
        let td = tempfile::tempdir().unwrap();
        let path = write_fake_plugin(
            td.path(),
            "k",
            r#"#!/bin/sh
echo '{"id":"k","name":"k","keywords":["kw","k2"]}'
"#,
        );
        let p = Plugin::load(path).await.unwrap();
        assert_eq!(p.matches("kw"), Some(("kw", "".into())));
        assert_eq!(p.matches("kw foo bar"), Some(("kw", "foo bar".into())));
        assert_eq!(p.matches("KW foo"), Some(("kw", "foo".into())));
        assert_eq!(p.matches("k2 other"), Some(("k2", "other".into())));
        // No match forms
        assert_eq!(p.matches("kwx"), None);
        assert_eq!(p.matches("other"), None);
        assert_eq!(p.matches(""), None);
    }

    #[tokio::test]
    async fn query_namespaces_candidate_ids() {
        let td = tempfile::tempdir().unwrap();
        let path = write_fake_plugin(
            td.path(),
            "greeter",
            r#"#!/bin/sh
case "$1" in
  info)
    echo '{"id":"greeter","name":"Greeter","keywords":["hi"]}' ;;
  query)
    echo '[{"id":"row1","title":"Hello","subtitle":"","icon":{"SfSymbol":"x"},"kind":"Action","actions":[{"id":"default","label":"Go"}],"search_text":"","bypass_rank":false}]'
    ;;
esac
"#,
        );
        let p = Plugin::load(path).await.unwrap();
        let q = Query::new("hi world");
        let out = p.query(&q).await;
        assert_eq!(out.len(), 1);
        assert!(
            out[0].id.starts_with("greeter::"),
            "expected provider-id prefix, got {}",
            out[0].id,
        );
    }

    #[tokio::test]
    async fn discover_scans_executables_only() {
        let td = tempfile::tempdir().unwrap();
        // Two real plugins + one non-executable sibling
        write_fake_plugin(
            td.path(),
            "a",
            r#"#!/bin/sh
echo '{"id":"a","name":"a","keywords":["a"]}'
"#,
        );
        write_fake_plugin(
            td.path(),
            "b",
            r#"#!/bin/sh
echo '{"id":"b","name":"b","keywords":["b"]}'
"#,
        );
        // Non-executable file (README or similar) should be skipped
        std::fs::write(td.path().join("README.md"), "# plugins").unwrap();

        let plugins = discover(td.path()).await;
        let ids: Vec<&str> = plugins.iter().map(|p| p.info.id.as_str()).collect();
        assert!(ids.contains(&"a"), "plugin `a` missing, got {ids:?}");
        assert!(ids.contains(&"b"), "plugin `b` missing, got {ids:?}");
        assert_eq!(plugins.len(), 2);
    }

    #[tokio::test]
    async fn one_bad_plugin_doesnt_take_others_with_it() {
        let td = tempfile::tempdir().unwrap();
        // Prints invalid JSON on info - must be skipped
        write_fake_plugin(
            td.path(),
            "broken",
            r#"#!/bin/sh
echo 'not json'
"#,
        );
        // Valid sibling - must still load
        write_fake_plugin(
            td.path(),
            "ok",
            r#"#!/bin/sh
echo '{"id":"ok","name":"ok","keywords":["ok"]}'
"#,
        );
        let plugins = discover(td.path()).await;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].info.id, "ok");
    }

    #[tokio::test]
    async fn plugins_without_keywords_dont_pass() {
        let td = tempfile::tempdir().unwrap();
        let path = write_fake_plugin(
            td.path(),
            "nokw",
            r#"#!/bin/sh
echo '{"id":"nokw","name":"no keywords","keywords":[]}'
"#,
        );
        assert!(Plugin::load(path).await.is_err());
    }

    #[tokio::test]
    async fn activate_roundtrip_preserves_namespaced_id() {
        // When Gyors passes us `greeter::row1` for activation, the
        // plugin's activate subcommand must see `row1` (the original
        // emission) - provider-id prefix is a Gyors-side
        // routing artifact
        let td = tempfile::tempdir().unwrap();
        let path = write_fake_plugin(
            td.path(),
            "greeter",
            r#"#!/bin/sh
case "$1" in
  info)
    echo '{"id":"greeter","name":"Greeter","keywords":["hi"]}' ;;
  activate)
    # Echo the id we received so the test can verify it.
    printf '{"CopyToClipboard":"got:%s"}' "$2"
    ;;
esac
"#,
        );
        let p = Plugin::load(path).await.unwrap();
        let eff = p
            .activate(&"greeter::row1".to_string(), "default")
            .await
            .unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "got:row1"),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    // ShellPlugin (JSON-defined inline plugins)

    fn spec(id: &str, cmd: &str, kws: &[&str]) -> ShellPluginSpec {
        ShellPluginSpec {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            keywords: kws.iter().map(|s| s.to_string()).collect(),
            command: cmd.into(),
            on_activate: ShellActivation::Copy,
            icon: None,
            timeout_ms: 2000,
            version: None,
            source_url: None,
            author: None,
        }
    }

    #[tokio::test]
    async fn shell_plugin_runs_command_and_emits_row() {
        // Simple echo plugin - sanity that the command is spawned,
        // output captured, and returned as a single candidate with
        // the expected title
        let p = ShellPlugin::new(spec("echo", "printf 'hello %s' {query}", &["ex"])).unwrap();
        let rows = p.query(&Query::new("ex world")).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "hello world");
    }

    #[tokio::test]
    async fn shell_plugin_disarms_little_bobby_tables() {
        // Attempted injection: user types `'; id; '` which without
        // escaping would break out of the quoted arg and run `id`
        // as its own command, concatenating its output. shell_escape
        // wraps in single quotes and escapes internals, so the
        // command sees the literal - ONE line of output, not two
        //
        // Verification: only ONE `got:` prefix in the output. If
        // injection landed, `id` would produce a second line with
        // a `uid=...` that lives outside the `got:` prefix string
        let p = ShellPlugin::new(spec("safe", "printf 'got:%s' {query}", &["safe"])).unwrap();
        let evil = "'; id; '";
        let rows = p.query(&Query::new(format!("safe {evil}"))).await;
        assert_eq!(rows.len(), 1);
        let matches: Vec<_> = rows[0].title.matches("got:").collect();
        assert_eq!(
            matches.len(),
            1,
            "injection succeeded - got: appears {} times: {}",
            matches.len(),
            rows[0].title,
        );
        assert!(
            !rows[0].title.contains("uid="),
            "`id` output leaked: {}",
            rows[0].title,
        );
    }

    #[tokio::test]
    async fn silent_commands_produce_silent_rows() {
        let p = ShellPlugin::new(spec("nil", "true", &["nil"])).unwrap();
        let rows = p.query(&Query::new("nil")).await;
        assert!(rows.is_empty(), "empty output should suppress the row");
    }

    #[tokio::test]
    async fn shell_plugin_matches_keyword_variants() {
        let p = ShellPlugin::new(spec("k", "echo yes", &["kw", "k2"])).unwrap();
        // Bare keyword
        assert!(!p.query(&Query::new("kw")).await.is_empty());
        // Keyword + filter
        assert!(!p.query(&Query::new("kw foo")).await.is_empty());
        // Case-insensitive
        assert!(!p.query(&Query::new("KW foo")).await.is_empty());
        // Second keyword
        assert!(!p.query(&Query::new("k2 bar")).await.is_empty());
        // No match
        assert!(p.query(&Query::new("other")).await.is_empty());
        assert!(p.query(&Query::new("kwx")).await.is_empty());
    }

    #[tokio::test]
    async fn shell_plugin_activate_copy_returns_output() {
        // Output round-trips via a base64 payload in candidate
        // id so activation doesn't need to re-run the command
        let p = ShellPlugin::new(spec("copier", "echo hello", &["copy"])).unwrap();
        let rows = p.query(&Query::new("copy")).await;
        let id = rows[0].id.clone();
        let eff = p.activate(&id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "hello"),
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_plugin_activation_variants() {
        // on_activate == Open -> OpenUrl
        let mut s = spec("opener", "echo https://example.com", &["open"]);
        s.on_activate = ShellActivation::Open;
        let p = ShellPlugin::new(s).unwrap();
        let rows = p.query(&Query::new("open")).await;
        match p.activate(&rows[0].id, "default").await.unwrap() {
            Effect::OpenUrl(u) => assert_eq!(u, "https://example.com"),
            other => panic!("got {other:?}"),
        }

        // Regression: on_activate == Shell MUST produce
        // ConfirmRunShell, not raw RunShell. If anyone reintroduces
        // the unconfirmed path, test breaks loudly. The
        // plugin_id is also threaded through so Swift UI can
        // attribute request
        let mut s = spec("runner", "echo afplay /tmp/x.wav", &["run"]);
        s.on_activate = ShellActivation::Shell;
        let p = ShellPlugin::new(s).unwrap();
        let rows = p.query(&Query::new("run")).await;
        match p.activate(&rows[0].id, "default").await.unwrap() {
            Effect::ConfirmRunShell { command, plugin_id } => {
                assert!(command.contains("afplay"));
                assert_eq!(plugin_id, "runner");
            }
            Effect::RunShell(_) => panic!(
                "shell-mode plugin emitted RunShell regression. \
                 Plugin stdout must route through ConfirmRunShell so the \
                 Swift shell can show the user the command before /bin/sh \
                 sees it."
            ),
            other => panic!("got {other:?}"),
        }

        // on_activate == None -> Effect::None
        let mut s = spec("info", "echo whatever", &["info"]);
        s.on_activate = ShellActivation::None;
        let p = ShellPlugin::new(s).unwrap();
        let rows = p.query(&Query::new("info")).await;
        assert!(matches!(
            p.activate(&rows[0].id, "default").await.unwrap(),
            Effect::None,
        ));
    }

    /// Regression. A plugin must NOT inherit the
    /// launcher's environment. We set a sentinel env var in the
    /// parent, point a plugin at `env`, and confirm the sentinel
    /// is absent from plugin's stdout. If `env_clear` is ever
    /// removed, the sentinel leaks and this test fails loudly
    #[tokio::test]
    async fn plugin_env_is_scrubbed() {
        let sentinel = "GYORS_TEST_SECRET";
        let value = "should-not-leak";
        // SAFETY: tests in this crate run sequentially per-process
        // by default. The launcher itself doesn't read this var.
        std::env::set_var(sentinel, value);

        let mut s = spec("env-probe", "/usr/bin/env", &["probe"]);
        s.on_activate = ShellActivation::Copy;
        let p = ShellPlugin::new(s).unwrap();
        let rows = p.query(&Query::new("probe")).await;
        assert!(!rows.is_empty(), "env plugin produced no rows");

        // Plugin's stdout becomes candidate id (base64).
        // Decode it back and check the sentinel isn't there
        use base64::prelude::*;
        let prefix = "env-probe::";
        let payload = rows[0]
            .id
            .strip_prefix(prefix)
            .expect("env-probe candidate id missing prefix");
        let stdout = BASE64_URL_SAFE_NO_PAD
            .decode(payload)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        assert!(
            !stdout.contains(value),
            "plugin inherited GYORS_TEST_SECRET regression. \
             stdout was: {stdout}"
        );
        // Confirm the allowlisted vars DO flow through (so plugins
        // can find their tools)
        assert!(
            stdout.contains("PATH=/usr/bin:/bin:/usr/sbin:/sbin"),
            "PATH not forwarded; stdout: {stdout}"
        );

        std::env::remove_var(sentinel);
    }

    #[test]
    fn shell_plugin_new_rejects_bad_specs() {
        assert!(
            ShellPlugin::new(spec("", "echo x", &["x"])).is_err(),
            "empty id must be rejected"
        );
        assert!(
            ShellPlugin::new(spec("id", "", &["x"])).is_err(),
            "empty command must be rejected"
        );
        assert!(
            ShellPlugin::new(spec("id", "echo x", &[])).is_err(),
            "no keywords must be rejected"
        );
    }

    #[test]
    fn load_shell_plugins_accepts_array_shape() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        std::fs::write(
            &path,
            r#"[
            {"id":"a","name":"A","keywords":["a"],"command":"echo a"},
            {"id":"b","name":"B","keywords":["b"],"command":"echo b","on_activate":"open"}
        ]"#,
        )
        .unwrap();
        let plugins = load_shell_plugins(&path);
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].spec.id, "a");
        assert_eq!(plugins[1].spec.on_activate, ShellActivation::Open);
    }

    #[test]
    fn load_shell_plugins_accepts_object_shape() {
        // Object wrapper leaves room for future top-level settings
        // without a migration; array is the minimal form
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        std::fs::write(
            &path,
            r#"{
            "plugins": [
                {"id":"only","name":"Only","keywords":["o"],"command":"echo o"}
            ]
        }"#,
        )
        .unwrap();
        let plugins = load_shell_plugins(&path);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].spec.id, "only");
    }

    #[test]
    fn load_shell_plugins_missing_file_returns_empty() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("nope.json");
        assert!(load_shell_plugins(&path).is_empty());
    }

    #[test]
    fn malformed_plugins_json_fails_quietly() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("bad.json");
        std::fs::write(&path, "not json at all").unwrap();
        assert!(load_shell_plugins(&path).is_empty());
    }

    #[test]
    fn load_shell_plugins_skips_bad_entries_keeps_good_ones() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("mixed.json");
        std::fs::write(
            &path,
            r#"[
            {"id":"","name":"bad","keywords":["x"],"command":"echo x"},
            {"id":"ok","name":"ok","keywords":["k"],"command":"echo k"}
        ]"#,
        )
        .unwrap();
        let plugins = load_shell_plugins(&path);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].spec.id, "ok");
    }

    #[test]
    fn shell_escape_puts_angry_quotes_in_their_place() {
        // Sanity-check the escape helper in isolation so integration
        // test for injection has a clear "where did it fail" pointer
        assert_eq!(shell_escape("abc"), "'abc'");
        assert_eq!(shell_escape(""), "''");
        // Single quote gets close-quoted, escaped, reopened
        assert_eq!(shell_escape("a'b"), r"'a'\''b'");
        // Semicolons / pipes / backticks pass through literally -
        // they're inert inside the single quotes
        assert_eq!(shell_escape("; rm -rf /"), "'; rm -rf /'");
    }


    #[test]
    fn upsert_adds_new_plugin_to_empty_file() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        let out = upsert_shell_plugin(&path, spec("weather", "echo x", &["w"])).unwrap();
        assert_eq!(out, UpsertOutcome::Added);
        let specs = read_specs(&path).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].id, "weather");
    }

    #[test]
    fn upsert_replaces_existing_plugin_by_id() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        upsert_shell_plugin(&path, spec("echo", "echo v1", &["e"])).unwrap();
        let mut updated = spec("echo", "echo v2", &["e"]);
        updated.version = Some("2.0.0".into());
        let out = upsert_shell_plugin(&path, updated).unwrap();
        assert_eq!(out, UpsertOutcome::Replaced);
        let specs = read_specs(&path).unwrap();
        assert_eq!(specs.len(), 1, "upsert should replace, not append");
        assert_eq!(specs[0].command, "echo v2");
        assert_eq!(specs[0].version.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn upsert_minds_its_own_business() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        upsert_shell_plugin(&path, spec("a", "echo a", &["a"])).unwrap();
        upsert_shell_plugin(&path, spec("b", "echo b", &["b"])).unwrap();
        upsert_shell_plugin(&path, spec("c", "echo c", &["c"])).unwrap();
        // Replace `b`; `a` and `c` must remain
        upsert_shell_plugin(&path, spec("b", "echo B", &["b"])).unwrap();
        let specs = read_specs(&path).unwrap();
        let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert_eq!(specs[1].command, "echo B");
    }

    #[test]
    fn remove_returns_true_when_present_false_otherwise() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        upsert_shell_plugin(&path, spec("gone", "echo g", &["g"])).unwrap();
        assert!(remove_shell_plugin(&path, "gone").unwrap());
        assert!(
            !remove_shell_plugin(&path, "gone").unwrap(),
            "second remove reports `nothing changed`"
        );
        let specs = read_specs(&path).unwrap();
        assert!(specs.is_empty());
    }

    #[test]
    fn find_returns_matching_spec() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        let mut sp = spec("pinned", "echo p", &["p"]);
        sp.version = Some("1.2.3".into());
        sp.source_url = Some("https://example.com/pinned".into());
        upsert_shell_plugin(&path, sp).unwrap();
        let got = find_shell_plugin(&path, "pinned").unwrap();
        assert_eq!(got.version.as_deref(), Some("1.2.3"));
        assert_eq!(
            got.source_url.as_deref(),
            Some("https://example.com/pinned")
        );
        assert!(find_shell_plugin(&path, "nope").is_none());
    }

    #[test]
    fn round_trip_preserves_optional_fields() {
        // Serialize -> read back - the optional fields (version /
        // source_url / author) survive without losing data or
        // sprouting unwanted nulls in JSON
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        let mut sp = spec("rt", "echo r", &["r"]);
        sp.version = Some("0.9.9".into());
        sp.source_url = Some("https://example.com".into());
        sp.author = Some("balintb".into());
        sp.description = "round trip test".into();
        upsert_shell_plugin(&path, sp.clone()).unwrap();
        let got = find_shell_plugin(&path, "rt").unwrap();
        assert_eq!(got.version, sp.version);
        assert_eq!(got.source_url, sp.source_url);
        assert_eq!(got.author, sp.author);
        assert_eq!(got.description, sp.description);
    }

    #[test]
    fn bad_ids_leave_no_trace_on_disk() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        // Path-traversal attempt
        assert!(upsert_shell_plugin(&path, spec("../../evil", "echo x", &["e"])).is_err());
        // Whitespace id
        assert!(upsert_shell_plugin(&path, spec(" ", "echo x", &["e"])).is_err());
        // Shadowing a built-in provider id
        assert!(upsert_shell_plugin(&path, spec("config", "echo x", &["cfg"])).is_err());
        assert!(upsert_shell_plugin(&path, spec("note", "echo x", &["n"])).is_err());
        // File must stay untouched (i.e. never created with garbage)
        assert!(
            !path.exists(),
            "bad-id upsert shouldn't have written anything"
        );
    }

    #[test]
    fn upsert_creates_parent_dir_if_missing() {
        let td = tempfile::tempdir().unwrap();
        let deep = td.path().join("nested").join("plugins.json");
        assert!(!deep.parent().unwrap().exists());
        upsert_shell_plugin(&deep, spec("ok", "echo o", &["o"])).unwrap();
        assert!(
            deep.exists(),
            "upsert should have auto-created the parent dir"
        );
    }

    #[test]
    fn read_specs_tolerates_empty_file() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        std::fs::write(&path, "").unwrap();
        assert!(read_specs(&path).unwrap().is_empty());
    }

    #[test]
    fn read_specs_tolerates_whitespace_only_file() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("plugins.json");
        std::fs::write(&path, "   \n\n").unwrap();
        assert!(read_specs(&path).unwrap().is_empty());
    }


    #[test]
    fn manifest_parse_happy_path() {
        let src = r#"{
            "gyors_plugin_manifest": 1,
            "shell": {
                "id": "weather",
                "name": "Weather",
                "version": "1.0.0",
                "source_url": "https://github.com/author/gyors-weather",
                "author": "author",
                "description": "wttr.in forecast",
                "keywords": ["weather", "w"],
                "command": "curl -sS 'wttr.in/{query}'",
                "on_activate": "copy",
                "icon": "cloud.sun"
            }
        }"#;
        let m = PluginManifest::parse(src).unwrap();
        assert_eq!(m.gyors_plugin_manifest, MANIFEST_VERSION);
        let spec = m.shell.unwrap();
        assert_eq!(spec.id, "weather");
        assert_eq!(spec.version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn manifest_parser_refuses_to_time_travel() {
        let src = r#"{"gyors_plugin_manifest": 999, "shell": null}"#;
        let err = PluginManifest::parse(src).unwrap_err().to_string();
        assert!(
            err.contains("unsupported manifest version"),
            "expected version-guard error, got: {err}"
        );
    }

    #[test]
    fn manifest_parse_rejects_empty_payload() {
        let src = r#"{"gyors_plugin_manifest": 1}"#;
        assert!(PluginManifest::parse(src).is_err());
    }

    #[test]
    fn reserved_ids_stay_reserved() {
        let src = r#"{
            "gyors_plugin_manifest": 1,
            "shell": {"id":"config","name":"bad","keywords":["cfg"],"command":"echo x"}
        }"#;
        let err = PluginManifest::parse(src).unwrap_err().to_string();
        assert!(
            err.contains("reserved"),
            "expected reserved-id error, got: {err}"
        );
    }

    #[test]
    fn manifest_parse_rejects_bad_id_characters() {
        let src = r#"{
            "gyors_plugin_manifest": 1,
            "shell": {"id":"has spaces","name":"x","keywords":["k"],"command":"echo x"}
        }"#;
        assert!(PluginManifest::parse(src).is_err());
    }

    #[test]
    fn manifest_roundtrip_via_to_json() {
        // A manifest written by to_json must re-parse cleanly
        let src = r#"{
            "gyors_plugin_manifest": 1,
            "shell": {
                "id":"echo","name":"Echo","keywords":["e"],
                "command":"echo {query}","version":"0.1.0"
            }
        }"#;
        let m1 = PluginManifest::parse(src).unwrap();
        let s = m1.to_json();
        let m2 = PluginManifest::parse(&s).unwrap();
        assert_eq!(m2.shell.unwrap().id, "echo");
    }

    #[test]
    fn validate_id_accepts_allowed_chars() {
        validate_id("a").unwrap();
        validate_id("abc123").unwrap();
        validate_id("my-plugin_2").unwrap();
        validate_id("x-Y-z").unwrap();
    }

    #[test]
    fn validate_id_blocks_dot_dot_slash_and_its_friends() {
        for bad in ["", " ", "../foo", "a/b", "a.b", "a b", "a!b", "unicode★"] {
            assert!(
                validate_id(bad).is_err(),
                "expected rejection of id `{bad}`"
            );
        }
    }

    #[tokio::test]
    async fn default_plugin_dir_honours_env_override() {
        std::env::set_var("GYORS_PLUGINS_DIR", "/tmp/gyors-plugins-test");
        assert_eq!(
            default_plugin_dir(),
            PathBuf::from("/tmp/gyors-plugins-test"),
        );
        std::env::remove_var("GYORS_PLUGINS_DIR");
    }


    /// Plant N executable shell-plugin stubs in `dir`, named with
    /// zero-padded indices so sort order is deterministic. Each
    /// stub responds to `info` with a unique id derived from its
    /// index, so resulting Plugin list has distinct rows
    fn plant_n_stub_plugins(dir: &Path, n: usize) {
        for i in 0..n {
            let name = format!("p{i:04}");
            // Use snake_case-safe ascii ids (`p0000`..) so the
            // resulting Plugin::info.id is a clean ascii string
            let body = format!(
                "#!/bin/sh\necho '{{\"id\":\"{name}\",\"name\":\"{name}\",\"keywords\":[\"{name}\"]}}'\n"
            );
            write_fake_plugin(dir, &name, &body);
        }
    }

    #[tokio::test]
    async fn discovery_under_cap_loads_everything() {
        let td = tempfile::tempdir().unwrap();
        plant_n_stub_plugins(td.path(), MAX_PLUGIN_DISCOVERY - 5);
        let plugins = discover(td.path()).await;
        assert_eq!(
            plugins.len(),
            MAX_PLUGIN_DISCOVERY - 5,
            "every plugin under the cap should load"
        );
    }

    #[tokio::test]
    async fn discovery_exactly_at_cap_loads_everything() {
        let td = tempfile::tempdir().unwrap();
        plant_n_stub_plugins(td.path(), MAX_PLUGIN_DISCOVERY);
        let plugins = discover(td.path()).await;
        assert_eq!(
            plugins.len(),
            MAX_PLUGIN_DISCOVERY,
            "boundary case: cap entries load (no off-by-one)"
        );
    }

    #[tokio::test]
    async fn discovery_above_cap_truncates_to_first_n_alphabetically() {
        // Plant 60 plugins (cap is 50). Discovery must drop the
        // last 10 alphabetically AND log the truncation. The cap
        // is what stops a same-user attacker from forcing the
        // launcher to fork 10k subprocesses on startup
        let td = tempfile::tempdir().unwrap();
        plant_n_stub_plugins(td.path(), MAX_PLUGIN_DISCOVERY + 10);
        let plugins = discover(td.path()).await;
        assert_eq!(
            plugins.len(),
            MAX_PLUGIN_DISCOVERY,
            "above-cap directory must truncate to MAX_PLUGIN_DISCOVERY"
        );
        // The first N alphabetically are `p0000`..`p0049`. Make
        // sure that's what we got - p0050 onwards must NOT appear
        let ids: std::collections::HashSet<&str> =
            plugins.iter().map(|p| p.info.id.as_str()).collect();
        for i in 0..MAX_PLUGIN_DISCOVERY {
            let expected = format!("p{i:04}");
            assert!(
                ids.contains(expected.as_str()),
                "missing expected plugin `{expected}` after cap"
            );
        }
        for i in MAX_PLUGIN_DISCOVERY..MAX_PLUGIN_DISCOVERY + 10 {
            let unexpected = format!("p{i:04}");
            assert!(
                !ids.contains(unexpected.as_str()),
                "plugin `{unexpected}` past the cap got loaded regression"
            );
        }
    }

    #[tokio::test]
    async fn discovery_skips_non_executables_before_counting_toward_cap() {
        // A directory full of non-executable junk (README, .DS_Store,
        // a regular text file dropped by a build tool) shouldn't
        // displace real plugins from the cap. Plant cap+5 real
        // plugins + 100 non-exec files: all real plugins (up to
        // the cap) must still load, and the junk doesn't count
        // toward the limit
        let td = tempfile::tempdir().unwrap();
        plant_n_stub_plugins(td.path(), MAX_PLUGIN_DISCOVERY - 5);
        for i in 0..100 {
            // No +x flag - won't pass `is_executable_file`
            std::fs::write(
                td.path().join(format!("junk-{i:03}.txt")),
                "not a plugin",
            )
            .unwrap();
        }
        let plugins = discover(td.path()).await;
        assert_eq!(
            plugins.len(),
            MAX_PLUGIN_DISCOVERY - 5,
            "non-executables must not consume cap slots"
        );
    }

    #[tokio::test]
    async fn discovery_missing_dir_returns_empty_no_panic() {
        // Defensive baseline kept alongside the cap tests so a
        // future contributor refactoring function can't break
        // the `mkdir`-not-yet-run case while focusing on the cap
        let p = std::path::PathBuf::from("/tmp/gyors-discovery-missing-12839h");
        let plugins = discover(&p).await;
        assert!(plugins.is_empty());
    }

    #[tokio::test]
    async fn discovery_cap_value_is_50() {
        // Pin the cap number so a reviewer can grep for it. If we
        // later raise or lower the cap they have to update this
        // test deliberately rather than letting silent drift slip
        // past code review
        assert_eq!(MAX_PLUGIN_DISCOVERY, 50);
    }


    /// Write a shell plugin that emits exactly `bytes` bytes of `x`
    /// on requested subcommand. Used to drive overflow tests
    /// against `run_plugin`'s typed-plugin path. We can't quote
    /// inside shell heredoc easily, so use printf with a fixed
    /// pattern
    fn write_overflow_plugin(dir: &Path, name: &str, stdout_bytes: usize) -> PathBuf {
        let body = format!(
            r#"#!/bin/sh
case "$1" in
  info)
    # Tiny well-formed manifest so Plugin::load passes.
    echo '{{"id":"{name}","name":"{name}","keywords":["{name}"]}}'
    ;;
  query)
    # Emit `stdout_bytes` bytes of `x` then EOF.
    yes x | tr -d '\n' | head -c {stdout_bytes}
    ;;
esac
"#
        );
        write_fake_plugin(dir, name, &body)
    }

    #[tokio::test]
    async fn run_plugin_succeeds_under_stdout_cap() {
        let td = tempfile::tempdir().unwrap();
        // Emit 100 KB - well under the 4 MB cap
        let path = write_overflow_plugin(td.path(), "small", 100 * 1024);
        let p = Plugin::load(path).await.unwrap();
        // We can't easily invoke the `query` subcommand through
        // Plugin::query() without setting up the full Query type,
        // but we can call run_plugin directly via the load path.
        // Re-spawn for query and assert bytes returned
        let bin = p.path.clone();
        let out = run_plugin(&bin, &["query", "", "x"], Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.len(), 100 * 1024, "under-cap output passes through");
    }

    #[tokio::test]
    async fn run_plugin_at_exact_cap_succeeds() {
        let td = tempfile::tempdir().unwrap();
        let path =
            write_overflow_plugin(td.path(), "boundary", MAX_PLUGIN_STDOUT_BYTES);
        let p = Plugin::load(path).await.unwrap();
        let out = run_plugin(&p.path, &["query", "", "x"], Duration::from_secs(15))
            .await
            .unwrap();
        assert_eq!(
            out.len(),
            MAX_PLUGIN_STDOUT_BYTES,
            "boundary case: exact-cap output still passes"
        );
    }

    #[tokio::test]
    async fn run_plugin_over_stdout_cap_bails_with_explicit_error() {
        // Emit cap + 1KB. The cap check must fire and return an
        // error containing the marker
        let td = tempfile::tempdir().unwrap();
        let path = write_overflow_plugin(
            td.path(),
            "overflow",
            MAX_PLUGIN_STDOUT_BYTES + 1024,
        );
        let p = Plugin::load(path).await.unwrap();
        let res = run_plugin(&p.path, &["query", "", "x"], Duration::from_secs(15)).await;
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains(""),
            "error message must cite, got: {err}"
        );
        assert!(
            err.contains("stdout exceeded"),
            "error must mention stdout, got: {err}"
        );
    }

    #[tokio::test]
    async fn run_plugin_dev_zero_doesnt_oom() {
        // The proof-of-concept attack: pipe /dev/zero. Without the
        // cap, the read buffer would grow until OS OOM. With it,
        // we bail in bounded memory + time
        let td = tempfile::tempdir().unwrap();
        let body = r#"#!/bin/sh
case "$1" in
  info)
    echo '{"id":"zerobomb","name":"zerobomb","keywords":["z"]}'
    ;;
  query)
    cat /dev/zero
    ;;
esac
"#;
        let path = write_fake_plugin(td.path(), "zerobomb", body);
        let p = Plugin::load(path).await.unwrap();
        // 10s timeout - the cap should trip well before any
        // reasonable timeout. Without the cap, this test would
        // either OOM the runner or push buffer to gigabytes
        // before the wall-clock fires
        let res = run_plugin(&p.path, &["query", "", "x"], Duration::from_secs(10)).await;
        // The exact failure mode (overflow vs timeout) depends on
        // pipe buffering rates - either is acceptable. The point
        // is we DONT allocate gigabytes before bailing
        assert!(res.is_err(), "/dev/zero plugin must fail one way or another");
    }

    #[tokio::test]
    async fn run_plugin_over_stderr_cap_bails() {
        // Same defence on the stderr side. Plugins that flood
        // stderr (a noisy library, attacker pivot) shouldn't be
        // able to OOM the parent either
        let td = tempfile::tempdir().unwrap();
        let body = format!(
            r#"#!/bin/sh
case "$1" in
  info)
    echo '{{"id":"stderrflood","name":"stderrflood","keywords":["s"]}}'
    ;;
  query)
    # Stderr overflow only - stdout stays empty.
    yes x | tr -d '\n' | head -c {} 1>&2
    ;;
esac
"#,
            MAX_PLUGIN_STDERR_BYTES + 1024
        );
        let path = write_fake_plugin(td.path(), "stderrflood", &body);
        let p = Plugin::load(path).await.unwrap();
        let res = run_plugin(&p.path, &["query", "", "x"], Duration::from_secs(15)).await;
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains(""),
            "stderr overflow must cite, got: {err}"
        );
        assert!(
            err.contains("stderr exceeded"),
            "error must mention stderr, got: {err}"
        );
    }

    #[tokio::test]
    async fn cap_values_match_spec() {
        // Pin actual numbers so a future contributor can't
        // silently relax the OOM defence by editing the const
        assert_eq!(MAX_PLUGIN_STDOUT_BYTES, 4 * 1024 * 1024);
        assert_eq!(MAX_PLUGIN_STDERR_BYTES, 1024 * 1024);
    }


    fn plugins_json_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_plugins_json(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("plugins.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn manifest_cap_pins_at_256k() {
        // Pin the cap so silent relaxation requires editing this
        // assertion deliberately
        assert_eq!(MAX_PLUGINS_JSON_BYTES, 256 * 1024);
    }

    #[test]
    fn load_shell_plugins_under_cap_works() {
        let td = plugins_json_dir();
        // 1KB manifest with one valid entry
        let body = r#"[{"id":"helloshell","name":"hi","keywords":["hi"],"command":"echo hi"}]"#;
        let path = write_plugins_json(td.path(), body);
        let loaded = load_shell_plugins(&path);
        assert_eq!(loaded.len(), 1, "small manifest should load");
        assert_eq!(loaded[0].spec.id, "helloshell");
    }

    #[test]
    fn load_shell_plugins_at_exact_cap_works() {
        // Synthesize a valid manifest exactly at the cap by
        // padding the description field with safe filler. The
        // valid+cap-sized case must still parse cleanly
        let td = plugins_json_dir();
        let head = r#"[{"id":"bigshell","name":"x","keywords":["x"],"command":"echo x","description":""#;
        let tail = "\"}]";
        let pad_len =
            MAX_PLUGINS_JSON_BYTES as usize - head.len() - tail.len();
        let body = format!("{head}{}{tail}", "a".repeat(pad_len));
        assert_eq!(body.len() as u64, MAX_PLUGINS_JSON_BYTES);
        let path = write_plugins_json(td.path(), &body);
        let loaded = load_shell_plugins(&path);
        assert_eq!(loaded.len(), 1, "boundary-sized manifest should still load");
    }

    #[test]
    fn load_shell_plugins_over_cap_returns_empty_no_oom() {
        // 512KB of filler - twice the cap. Function must
        // return [] rather than read the whole file into memory
        let td = plugins_json_dir();
        let head = r#"[{"id":"big","name":"x","keywords":["x"],"command":"echo x","description":""#;
        let tail = "\"}]";
        let body = format!("{head}{}{tail}", "a".repeat(512 * 1024));
        let path = write_plugins_json(td.path(), &body);
        let loaded = load_shell_plugins(&path);
        assert!(
            loaded.is_empty(),
            "over-cap manifest must NOT load"
        );
    }

    #[test]
    fn read_specs_over_cap_returns_error() {
        let td = plugins_json_dir();
        let body = "[".to_string() + &"a".repeat(MAX_PLUGINS_JSON_BYTES as usize);
        let path = write_plugins_json(td.path(), &body);
        let res = read_specs(&path);
        assert!(
            res.is_err(),
            "read_specs over-cap must error so install path can refuse"
        );
        let msg = format!("{:#}", res.unwrap_err());
        assert!(
            msg.contains("cap is") || msg.contains("read"),
            "error message should reference cap context, got: {msg}"
        );
    }

    #[test]
    fn read_file_capped_refuses_oversized_via_stat() {
        let td = plugins_json_dir();
        let path = write_plugins_json(td.path(), &"a".repeat(1024));
        let err = read_file_capped(&path, 100).unwrap_err();
        assert!(
            err.to_string().contains("1024") || err.to_string().contains("cap"),
            "stat-based refusal carries the actual size, got: {err}"
        );
    }

    #[test]
    fn read_file_capped_passes_under_limit() {
        let td = plugins_json_dir();
        let path = write_plugins_json(td.path(), "small payload");
        let out = read_file_capped(&path, 1024).unwrap();
        assert_eq!(out, "small payload");
    }

    #[test]
    fn read_file_capped_passes_exactly_at_limit() {
        let td = plugins_json_dir();
        let body = "x".repeat(100);
        let path = write_plugins_json(td.path(), &body);
        let out = read_file_capped(&path, 100).unwrap();
        assert_eq!(out.len(), 100);
    }
}
