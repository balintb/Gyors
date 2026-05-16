//! `gyors plugin ...` - developer tooling for plugin authors
//!
//! Three jobs:
//!
//! - `scaffold <name>`: spit out a starter `.gyorsplugin` manifest in
//!   the current directory so a new plugin is a one-command away from
//!   having shape. Bikeshed-free defaults, comments inside JSON
//!   (via sibling README).
//! - `validate <file>`: parse a `.gyorsplugin` manifest OR a raw
//!   plugins.json spec, run all the id / keyword / schema checks
//!   we'd apply at install time, and print a green/red summary so
//!   authors can iterate without opening Gyors.
//! - `test <file> [query]`: drive plugin end-to-end - substitute
//!   query, spawn `sh -c`, show the output + how Gyors would
//!   render it. the "unit test" for a shell plugin
//!
//! None of these touch user's installed plugin set. That matters:
//! Plugin authors iterating on a WIP manifest shouldn't be risking
//! their config, and CI can call `gyors plugin validate` as a lint

use anyhow::{Context, Result};
use gyors_plugin_host::{PluginManifest, ShellActivation, ShellPluginSpec};
use std::path::PathBuf;

/// Entry point. Takes the argv tail after `gyors plugin ...`
pub fn run(args: &[String]) -> Result<()> {
    let (head, rest) = match args.split_first() {
        Some(parts) => parts,
        None => {
            print_help();
            return Ok(());
        }
    };
    match head.as_str() {
        "scaffold" => cmd_scaffold(rest),
        "validate" => cmd_validate(rest),
        "test" => cmd_test(rest),
        "--help" | "-h" | "help" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("unknown plugin subcommand: {other}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    }
}

pub fn print_help() {
    println!("gyors plugin <subcommand>");
    println!();
    println!("  scaffold <name>       write a starter <name>.gyorsplugin manifest");
    println!("  validate <file>       check a manifest or plugins.json spec");
    println!("  test <file> [query]   run the plugin's command and show its output");
}


fn cmd_scaffold(rest: &[String]) -> Result<()> {
    let name = rest
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: gyors plugin scaffold <name>"))?;
    let id = normalize_id(name);
    let out_path: PathBuf = format!("{id}.gyorsplugin").into();
    if out_path.exists() {
        anyhow::bail!("{} already exists - refusing to overwrite", out_path.display());
    }
    let manifest = starter_manifest(&id, name);
    std::fs::write(&out_path, manifest.to_json())?;
    println!("✓ wrote {}", out_path.display());
    println!();
    println!("next: edit the `command` field, then:");
    println!("  gyors plugin validate {}", out_path.display());
    println!("  gyors plugin test {} \"some input\"", out_path.display());
    Ok(())
}

/// Turn a user-supplied name into a plugin id: lowercased, spaces and
/// punctuation collapsed to `-`, plugin-host's charset enforced. The
/// result is always safe to pass to `validate_id`
fn normalize_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for c in raw.chars() {
        let keep = c.is_ascii_alphanumeric();
        if keep {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("my-plugin");
    }
    out
}

fn starter_manifest(id: &str, display: &str) -> PluginManifest {
    let spec = ShellPluginSpec {
        id: id.to_string(),
        name: display.to_string(),
        description: "Describe what this plugin does here.".into(),
        keywords: vec![id.to_string()],
        command: "echo 'hello from {query}'".into(),
        on_activate: ShellActivation::Copy,
        icon: None,
        timeout_ms: 2000,
        version: Some("0.1.0".into()),
        source_url: None,
        author: None,
    };
    PluginManifest {
        gyors_plugin_manifest: gyors_plugin_host::MANIFEST_VERSION,
        shell: Some(spec),
    }
}


fn cmd_validate(rest: &[String]) -> Result<()> {
    let path = rest
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: gyors plugin validate <file>"))?;
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {path}"))?;
    let report = validate_raw(&raw);
    println!("{}", report.render(path));
    if report.fatal {
        std::process::exit(1);
    }
    Ok(())
}

/// Validation result. `fatal` issues fail the command; warnings print
/// but still return success so CI runs that `lint + test` stay green
/// when an author is only missing optional metadata (`version`, etc.)
#[derive(Debug, Default)]
pub struct ValidateReport {
    pub ok: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub fatal: bool,
}

impl ValidateReport {
    pub fn render(&self, path: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!("{path}\n"));
        for line in &self.ok {
            out.push_str(&format!("  ✓ {line}\n"));
        }
        for line in &self.warnings {
            out.push_str(&format!("  ! {line}\n"));
        }
        for line in &self.errors {
            out.push_str(&format!("  ✗ {line}\n"));
        }
        out.push_str(if self.fatal {
            "validation FAILED\n"
        } else {
            "validation OK\n"
        });
        out
    }
}

pub fn validate_raw(raw: &str) -> ValidateReport {
    let mut r = ValidateReport::default();

    // First: does it parse as JSON at all?
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            r.errors.push(format!("not valid JSON: {e}"));
            r.fatal = true;
            return r;
        }
    };
    r.ok.push("parses as JSON".into());

    // Manifest (.gyorsplugin) or raw spec (plugins.json entry)?
    let spec = if value.get("gyors_plugin_manifest").is_some() {
        match PluginManifest::parse(raw) {
            Ok(m) => {
                r.ok.push(format!(
                    "manifest schema v{}",
                    m.gyors_plugin_manifest
                ));
                m.shell
            }
            Err(e) => {
                r.errors.push(format!("manifest: {e}"));
                r.fatal = true;
                return r;
            }
        }
    } else {
        match serde_json::from_value::<ShellPluginSpec>(value.clone()) {
            Ok(s) => {
                r.ok.push("parses as shell plugin spec".into());
                Some(s)
            }
            Err(e) => {
                r.errors.push(format!("spec: {e}"));
                r.fatal = true;
                return r;
            }
        }
    };

    let Some(spec) = spec else {
        r.errors.push("no shell plugin declared".into());
        r.fatal = true;
        return r;
    };

    // Id / keyword shape via same rules the installer applies
    match gyors_plugin_host::validate_spec(&spec) {
        Ok(()) => r.ok.push(format!("id `{}` + keywords OK", spec.id)),
        Err(e) => {
            r.errors.push(format!("validate_spec: {e}"));
            r.fatal = true;
        }
    }

    if !spec.command.contains("{query}") {
        r.warnings.push(
            "command has no `{query}` placeholder - the user's input won't reach it".into(),
        );
    }
    if spec.keywords.is_empty() {
        r.errors.push("no keywords - the plugin would never trigger".into());
        r.fatal = true;
    }
    if spec.version.is_none() {
        r.warnings.push(
            "no `version` field - fine to ship, but update detection won't work".into(),
        );
    }
    if spec.source_url.is_none() {
        r.warnings.push("no `source_url` - users won't see a provenance link".into());
    }

    r
}


fn cmd_test(rest: &[String]) -> Result<()> {
    let path = rest
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: gyors plugin test <file> [query]"))?;
    let query: String = rest.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading {path}"))?;
    let spec = extract_spec(&raw)?;
    let plugin = gyors_plugin_host::ShellPlugin::new(spec.clone())?;

    let command = plugin.rendered_command(&query);
    println!("command: {command}");
    println!();

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .output()
        .context("spawning sh for plugin command")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("exit: {}", output.status.code().unwrap_or(-1));
    if !stdout.is_empty() {
        println!("stdout:\n{stdout}");
    }
    if !stderr.is_empty() {
        println!("stderr:\n{stderr}");
    }
    println!();
    println!(
        "Gyors would show one row titled {:?} - activating it would {}.",
        stdout.trim(),
        activation_description(spec.on_activate),
    );
    Ok(())
}

fn activation_description(a: ShellActivation) -> &'static str {
    match a {
        ShellActivation::Copy => "copy stdout to the clipboard",
        ShellActivation::Open => "pass stdout to `open`",
        ShellActivation::Shell => "execute stdout as a shell command (dangerous)",
        ShellActivation::None => "do nothing (command's side effects are the point)",
    }
}

/// Accept either a `.gyorsplugin` manifest or a raw spec. Lets the
/// author run `plugin test` on whatever shape they happen to be
/// editing that minute
fn extract_spec(raw: &str) -> Result<ShellPluginSpec> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    if value.get("gyors_plugin_manifest").is_some() {
        let manifest = PluginManifest::parse(raw)?;
        manifest
            .shell
            .ok_or_else(|| anyhow::anyhow!("manifest has no shell plugin"))
    } else {
        serde_json::from_value(value).context("not a manifest and not a spec")
    }
}

/// Path-agnostic scaffold for tests. Real `cmd_scaffold` writes next
/// to the CWD; tests need a tmpdir
#[cfg(test)]
fn scaffold_at(dir: &std::path::Path, name: &str) -> Result<PathBuf> {
    let id = normalize_id(name);
    let out_path = dir.join(format!("{id}.gyorsplugin"));
    if out_path.exists() {
        anyhow::bail!("{} already exists", out_path.display());
    }
    let manifest = starter_manifest(&id, name);
    std::fs::write(&out_path, manifest.to_json())?;
    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_id_collapses_punctuation_to_dashes() {
        assert_eq!(normalize_id("My Great Plugin"), "my-great-plugin");
        assert_eq!(normalize_id("foo/bar_baz"), "foo-bar-baz");
        assert_eq!(normalize_id("  spaces  "), "spaces");
        assert_eq!(normalize_id("!!!"), "my-plugin"); // everything stripped
        assert_eq!(normalize_id(""), "my-plugin");
    }

    #[test]
    fn normalize_id_keeps_ascii_alnum() {
        assert_eq!(normalize_id("abc123"), "abc123");
        assert_eq!(normalize_id("X9"), "x9");
    }

    #[test]
    fn starter_manifest_validates_clean() {
        let m = starter_manifest("test-plugin", "Test Plugin");
        let raw = m.to_json();
        let report = validate_raw(&raw);
        assert!(
            !report.fatal,
            "starter manifest should validate: errors={:?}",
            report.errors,
        );
    }

    #[test]
    fn validate_catches_invalid_json() {
        let report = validate_raw("not json at all");
        assert!(report.fatal);
        assert!(report.errors.iter().any(|e| e.contains("JSON")));
    }

    #[test]
    fn validate_flags_missing_query_placeholder() {
        let raw = r#"{
            "id": "x",
            "name": "X",
            "keywords": ["x"],
            "command": "echo hi",
            "on_activate": "copy"
        }"#;
        let report = validate_raw(raw);
        assert!(
            report.warnings.iter().any(|w| w.contains("{query}")),
            "warnings={:?}",
            report.warnings,
        );
    }

    #[test]
    fn validate_flags_reserved_id() {
        let raw = r#"{
            "id": "apps",
            "name": "Taken",
            "keywords": ["apps"],
            "command": "echo {query}",
            "on_activate": "copy"
        }"#;
        let report = validate_raw(raw);
        assert!(report.fatal, "errors={:?}", report.errors);
    }

    #[test]
    fn validate_flags_empty_keywords() {
        let raw = r#"{
            "id": "lonely",
            "name": "Lonely",
            "keywords": [],
            "command": "echo {query}",
            "on_activate": "copy"
        }"#;
        let report = validate_raw(raw);
        assert!(report.fatal, "empty keywords must fail");
        assert!(report.errors.iter().any(|e| e.contains("keywords")));
    }

    #[test]
    fn validate_warns_on_missing_version_but_still_ok() {
        let raw = r#"{
            "id": "proto",
            "name": "Proto",
            "keywords": ["proto"],
            "command": "echo {query}",
            "on_activate": "copy"
        }"#;
        let report = validate_raw(raw);
        assert!(
            !report.fatal,
            "missing version is a warning, not fatal: errors={:?}",
            report.errors,
        );
        assert!(
            report.warnings.iter().any(|w| w.contains("version")),
            "warnings={:?}",
            report.warnings,
        );
    }

    #[test]
    fn validate_accepts_manifest_shape_not_just_spec() {
        let m = starter_manifest("proto", "Proto");
        let raw = m.to_json();
        let report = validate_raw(&raw);
        assert!(!report.fatal);
        assert!(report.ok.iter().any(|o| o.contains("manifest schema")));
    }

    #[test]
    fn validate_rejects_wrong_manifest_version() {
        let raw = r#"{
            "gyors_plugin_manifest": 999,
            "shell": {
                "id": "x",
                "name": "X",
                "keywords": ["x"],
                "command": "echo {query}",
                "on_activate": "copy"
            }
        }"#;
        let report = validate_raw(raw);
        assert!(report.fatal);
    }

    #[test]
    fn extract_spec_reads_either_shape() {
        let m = starter_manifest("a", "A");
        let manifest_raw = m.to_json();
        let spec = extract_spec(&manifest_raw).unwrap();
        assert_eq!(spec.id, "a");

        let raw_spec = r#"{
            "id": "b",
            "name": "B",
            "keywords": ["b"],
            "command": "echo {query}",
            "on_activate": "copy"
        }"#;
        let spec = extract_spec(raw_spec).unwrap();
        assert_eq!(spec.id, "b");
    }

    #[test]
    fn scaffold_writes_file_with_expected_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = scaffold_at(dir.path(), "My Plugin").unwrap();
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "my-plugin.gyorsplugin",
        );
        assert!(path.exists());
    }

    #[test]
    fn scaffold_refuses_to_clobber() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_at(dir.path(), "proto").unwrap();
        let err = scaffold_at(dir.path(), "proto").unwrap_err().to_string();
        assert!(err.contains("already exists"), "err={err}");
    }

    #[test]
    fn scaffolded_file_passes_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = scaffold_at(dir.path(), "my plugin").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let report = validate_raw(&raw);
        assert!(
            !report.fatal,
            "scaffolded plugin must validate: errors={:?}",
            report.errors,
        );
    }
}
