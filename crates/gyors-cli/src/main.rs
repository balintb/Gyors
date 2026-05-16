mod plugin;
mod sync;

use anyhow::Result;
use gyors_core::{
    parse_mode, precision_bonus, Effect, NucleoRanker, Query, QueryMode, Ranker,
    ScoredCandidate, MAX_FRECENCY_BOOST,
};
use gyors_index::Index;
use gyors_providers::registry::new_disabled_set;
use gyors_providers::{
    AppsProvider, ConfigProvider, CurrencyProvider, GitReposProvider, NotesProvider,
    ProviderRegistry, ShortcutsProvider, SnippetsProvider, SshProvider,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const BYPASS_RANK_SCORE: i64 = 1_000_000;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();

    if let Some(first) = args.first() {
        if first == "completions" || first == "--completions" {
            let shell = args.get(1).map(String::as_str).unwrap_or("zsh");
            print!("{}", completions::script(shell));
            return Ok(());
        }
        if first == "plugin" {
            return plugin::run(&args[1..]);
        }
        if first == "sync" {
            return sync::run(&args[1..]).await;
        }
        if first == "--help" || first == "-h" {
            print_help();
            return Ok(());
        }
    }

    let (pattern, pick) = parse_args_from(args.into_iter());
    let query = Query::new(&pattern);

    let index = Arc::new(Index::open(index_path()?)?);
    let registry = build_registry(Arc::clone(&index)).await;
    let scored = orchestrate(&query, &registry, &index).await?;

    if scored.is_empty() {
        println!("(no matches for {pattern:?} - {} providers queried)", registry.len());
        return Ok(());
    }

    println!("query: {pattern:?}  ({} results)", scored.len());
    println!("──────────────────────────────────────────");
    for (i, sc) in scored.iter().take(10).enumerate() {
        let subtitle = sc.candidate.subtitle.as_deref().unwrap_or("");
        println!(
            "{i:>2}  {:>8}  {:<28}  {subtitle}",
            sc.score, sc.candidate.title
        );
    }

    if let Some(idx) = pick {
        let Some(sc) = scored.get(idx) else {
            eprintln!("pick index {idx} out of range");
            return Ok(());
        };
        let now = now_secs();
        index.record_visit(&sc.candidate.id, now)?;
        let effect = registry.dispatch(&sc.candidate.id, "default").await?;
        println!();
        println!("recorded visit: {}", sc.candidate.title);
        match effect {
            Effect::OpenPath(p) => println!("effect: OpenPath({})", p.display()),
            Effect::CopyToClipboard(s) => println!("effect: CopyToClipboard({s:?})"),
            other => println!("effect: {other:?}"),
        }
    }

    Ok(())
}

/// Build the full production provider set. The common 45-provider
/// catalog lives in `gyors_providers::add_core_providers` - both this
/// binary and menu-bar app's `GyorsBridge` call it. Panel-only
/// providers (AiTransforms / Jwt / InputAutocomplete / PluginsUI /
/// Scratchpad) are added by IPC only; the CLI doesn't render their
/// supporting UI
async fn build_registry(index: Arc<Index>) -> ProviderRegistry {
    let gate = new_disabled_set();
    ConfigProvider::load_disabled_into(&gate);
    let core_ctx = gyors_providers::CoreProviderContext {
        index: Arc::clone(&index),
        disabled: gate.clone(),
        apps: AppsProvider::new().await.expect("apps init"),
        git_repos: GitReposProvider::new(),
        shortcuts: ShortcutsProvider::new(),
        notes: NotesProvider::new().await,
        snippets: SnippetsProvider::new().await,
        ssh: SshProvider::new().await,
        currency: CurrencyProvider::new().await,
    };
    let builder = ProviderRegistry::builder_with_gate(gate.clone());
    let mut registry = gyors_providers::add_core_providers(builder, core_ctx).build();
    let provider_ids: Vec<String> =
        registry.ids().into_iter().map(|s| s.to_string()).collect();
    registry.add_provider(ConfigProvider::new(gate, provider_ids));
    registry
}

async fn orchestrate(
    query: &Query,
    registry: &ProviderRegistry,
    index: &Index,
) -> Result<Vec<ScoredCandidate>> {
    let (pattern, mode) = parse_mode(query.raw.as_str());
    let effective = Query::new(pattern);

    if mode == QueryMode::Clipboard {
        let items = registry.query_one("clip", &effective).await;
        return Ok(items
            .into_iter()
            .enumerate()
            .map(|(i, c)| ScoredCandidate {
                candidate: c,
                score: BYPASS_RANK_SCORE - i as i64,
            })
            .collect());
    }
    if mode == QueryMode::Shell {
        let items = registry.query_one("shell", &effective).await;
        return Ok(items
            .into_iter()
            .map(|c| ScoredCandidate { candidate: c, score: BYPASS_RANK_SCORE })
            .collect());
    }

    let mut all = registry.query_all_except(&["files", "shell"], &effective).await;
    if mode == QueryMode::IncludeFiles {
        all.extend(registry.query_one("files", &effective).await);
    }

    let (bypass, normal): (Vec<_>, Vec<_>) = all.into_iter().partition(|c| c.bypass_rank);
    let mut scored: Vec<ScoredCandidate> = bypass
        .into_iter()
        .map(|c| ScoredCandidate { candidate: c, score: BYPASS_RANK_SCORE })
        .collect();
    scored.extend(NucleoRanker::new().rank(pattern, normal));

    let now = now_secs();
    let ids: Vec<&str> = scored.iter().map(|sc| sc.candidate.id.as_str()).collect();
    let boosts = index.frecency_scores_bulk(&ids, now, 20)?;
    for sc in &mut scored {
        let tier = precision_bonus(pattern, &sc.candidate.title);
        if sc.score == BYPASS_RANK_SCORE {
            sc.score = sc.score.saturating_add(tier);
        }
        if let Some(&b) = boosts.get(&sc.candidate.id) {
            let capped = (b as i64).min(MAX_FRECENCY_BOOST);
            sc.score = sc.score.saturating_add(capped);
        }
    }
    scored.sort_by_key(|s| std::cmp::Reverse(s.score));
    Ok(scored)
}

fn print_help() {
    println!("gyors <query>     (CLI for the Gyors launcher)");
    println!();
    println!("  --pick <n>          activate the nth result");
    println!("  plugin <subcmd>     plugin author tools (scaffold/validate/test)");
    println!("  completions <shell> print shell completions (zsh/bash/fish)");
    println!("  --help              this help");
}

/// Static shell-completion scripts. Keeps completions tiny and offline-
/// friendly: we dont auto-generate from the live provider list (those
/// vary by user state), we curate the high-signal keywords
mod completions {
    const KEYWORDS: &[&str] = &[
        "note", "notes", "n", "newnote", "nn",
        "snip", "snippet",
        "shortcut", "sc", "shortcuts",
        "upper", "uppercase", "lower", "lowercase", "rev", "reverse", "count",
        "json", "jq", "json2yaml", "yaml2json", "json2toml", "toml2json",
        "yaml2toml", "toml2yaml",
        "re", "regex",
        "ssh", "tab", "tabs", "recent", "rec",
        "tz", "timer", "timers",
        "calc", "uuid", "uuid7", "passw", "password", "lorem",
        "b64", "b64d", "url", "urld", "md5", "sha1", "sha256", "sha3", "blake3",
        "color", "hex", "rgb", "hsl", "hsv",
        "g", "ddg", "gh", "so", "yt", "npm", "w", "docs",
        "def", "emoji", "qr", "ask",
    ];

    pub fn script(shell: &str) -> String {
        match shell {
            "zsh" => zsh(),
            "bash" => bash(),
            "fish" => fish(),
            _ => format!("# Unsupported shell: {shell}. Supported: zsh, bash, fish.\n"),
        }
    }

    fn zsh() -> String {
        let mut out = String::new();
        out.push_str("#compdef gyors\n_gyors() {\n  local -a keywords\n  keywords=(\n");
        for kw in KEYWORDS {
            out.push_str(&format!("    '{kw}'\n"));
        }
        out.push_str("  )\n  _arguments '*:keyword:(${keywords[@]})'\n}\n_gyors \"$@\"\n");
        out
    }

    fn bash() -> String {
        let kws = KEYWORDS.join(" ");
        format!(
            r#"_gyors()
{{
  local cur="${{COMP_WORDS[COMP_CWORD]}}"
  COMPREPLY=( $(compgen -W "{kws}" -- "$cur") )
  return 0
}}
complete -F _gyors gyors
"#
        )
    }

    fn fish() -> String {
        let mut out = String::new();
        for kw in KEYWORDS {
            out.push_str(&format!("complete -c gyors -f -a '{kw}'\n"));
        }
        out
    }
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> (String, Option<usize>) {
    let mut pick = None;
    let mut parts: Vec<String> = Vec::new();
    let mut iter = args.into_iter();
    while let Some(a) = iter.next() {
        if a == "--pick" {
            pick = iter.next().and_then(|v| v.parse().ok());
        } else {
            parts.push(a);
        }
    }
    (parts.join(" "), pick)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn index_path() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("no local data dir"))?
        .join("Gyors");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("gyors.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_plain_query() {
        let (q, p) = parse_args_from(args(&["hello", "world"]));
        assert_eq!(q, "hello world");
        assert_eq!(p, None);
    }

    #[test]
    fn parse_empty() {
        let (q, p) = parse_args_from(args(&[]));
        assert_eq!(q, "");
        assert_eq!(p, None);
    }

    #[test]
    fn parse_pick_at_end() {
        let (q, p) = parse_args_from(args(&["saf", "--pick", "1"]));
        assert_eq!(q, "saf");
        assert_eq!(p, Some(1));
    }

    #[test]
    fn parse_pick_at_start() {
        let (q, p) = parse_args_from(args(&["--pick", "2", "foo", "bar"]));
        assert_eq!(q, "foo bar");
        assert_eq!(p, Some(2));
    }

    #[test]
    fn parse_pick_in_middle() {
        let (q, p) = parse_args_from(args(&["foo", "--pick", "0", "bar"]));
        assert_eq!(q, "foo bar");
        assert_eq!(p, Some(0));
    }

    #[test]
    fn parse_pick_without_value_is_none() {
        let (q, p) = parse_args_from(args(&["--pick"]));
        assert_eq!(q, "");
        assert_eq!(p, None);
    }

    #[test]
    fn parse_pick_non_numeric_is_none() {
        let (q, p) = parse_args_from(args(&["--pick", "abc"]));
        assert_eq!(q, "");
        assert_eq!(p, None);
    }
}
