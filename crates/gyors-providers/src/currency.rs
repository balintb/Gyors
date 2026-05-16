//! Currency conversion via `open.er-api.com` (free, no auth, ECB/central-
//! bank sourced, refreshed daily). Pattern-triggered alongside the unit
//! converter - `100 usd to eur` fires this
//!
//! Rates are cached to
//! `~/Library/Application Support/Gyors/rates.json` with a fetched-at
//! timestamp. On startup we load cache synchronously and kick off an
//! async refresh if older than 24h. A failed fetch keeps the stale rates
//! around - a day-old EUR/USD is far better than nothing
//!
//! Conversions are done against USD as the pivot currency: result =
//! amount x (rate_to / rate_from)

use anyhow::Result;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Max cache age before we trigger a background refresh. 24h matches
/// the upstream update cadence - refreshing sooner is just noise
const CACHE_MAX_AGE_SECS: i64 = 24 * 3600;
/// Abs cap - if cache is this stale, stop serving it so user
/// sees "rates unavailable" instead of a number that may be weeks out
const CACHE_HARD_MAX_SECS: i64 = 7 * 24 * 3600;

pub struct CurrencyProvider {
    rates: Arc<ArcSwap<Option<CurrencyRates>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrencyRates {
    /// Base currency the `rates` values are expressed against
    pub base: String,
    /// Map of currency code -> rate per 1 unit of `base`.
    /// E.g. `{"EUR": 0.92, "GBP": 0.79, "USD": 1.0}` when base == "USD"
    pub rates: HashMap<String, f64>,
    /// Unix-seconds timestamp of the fetch. Used for staleness checks
    pub fetched_at: i64,
}

impl CurrencyProvider {
    pub async fn new() -> Self {
        let rates = Arc::new(ArcSwap::from(Arc::new(load_cached())));
        let mine = Self { rates: Arc::clone(&rates) };

        // Schedule a background refresh if cache is missing or stale.
        // A failed fetch keeps the old cache; we never wipe it on error
        let now = now_secs();
        let needs_refresh = match rates.load().as_ref().as_ref() {
            Some(c) => now - c.fetched_at > CACHE_MAX_AGE_SECS,
            None => true,
        };
        if needs_refresh {
            tokio::spawn(async move {
                if let Ok(Some(fresh)) = tokio::task::spawn_blocking(fetch_rates).await {
                    let _ = save_cache(&fresh);
                    rates.store(Arc::new(Some(fresh)));
                }
            });
        }

        mine
    }

    pub fn len(&self) -> usize {
        self.rates.load().as_ref().as_ref().map(|c| c.rates.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn has_code(&self, code: &str) -> bool {
        let upper = code.to_uppercase();
        self.rates
            .load()
            .as_ref()
            .as_ref()
            .map(|c| c.rates.contains_key(&upper))
            .unwrap_or(false)
    }
}

#[async_trait]
impl Provider for CurrencyProvider {
    fn id(&self) -> &str {
        "ccy"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let Some((amount, from, to)) = parse_conversion(query.pattern()) else { return vec![]; };
        let cache_guard = self.rates.load();
        let Some(cache) = cache_guard.as_ref().as_ref() else { return vec![]; };
        if now_secs() - cache.fetched_at > CACHE_HARD_MAX_SECS {
            // Too stale to trust; dont mislead user
            return vec![];
        }
        let from_upper = from.to_uppercase();
        let to_upper = to.to_uppercase();
        // Only fire when *both* codes look like real currencies - the
        // unit converter covers everything else
        let (Some(&rate_from), Some(&rate_to)) =
            (cache.rates.get(&from_upper), cache.rates.get(&to_upper))
        else {
            return vec![];
        };
        let result = amount * rate_to / rate_from;
        vec![candidate(amount, &from_upper, &to_upper, result, cache.fetched_at)]
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> Result<Effect> {
        let value = id
            .strip_prefix("ccy::")
            .ok_or_else(|| anyhow::anyhow!("invalid ccy candidate id: {id}"))?;
        Ok(Effect::CopyToClipboard(value.to_string()))
    }
}

/// Re-use the unit converter's separator logic: `<num> <a> to <b>`
/// (`in`, `as` also accepted). Returns None if the shape doesn't fit,
/// regardless of whether the tokens are currencies
pub fn parse_conversion(s: &str) -> Option<(f64, String, String)> {
    // Strip trailing punctuation users casually add to questions -
    // `?`, `.`, `!`, `,`, `;`. Currency codes and names never
    // include these, so they're unambiguously noise. Without this
    // strip `to` field on `"10 usd in huf?"` ends up as
    // `"huf?"` and the rate lookup fails silently - user sees
    // no currency row, falls through to AI free-form, gets a
    // generic "you'd need an exchange rate" non-answer
    let trimmed = s
        .trim()
        .trim_end_matches(['?', '.', '!', ',', ';'])
        .trim();
    let lower = trimmed.to_lowercase();
    let mut parts = lower.splitn(2, char::is_whitespace);
    let num_str = parts.next()?;
    let value: f64 = num_str.parse().ok()?;
    let rest = parts.next()?.trim();
    let (idx, sep_len) = [" to ", " in ", " as "]
        .iter()
        .filter_map(|sep| rest.find(sep).map(|i| (i, sep.len())))
        .min_by_key(|&(i, _)| i)?;
    let (from, after) = rest.split_at(idx);
    let to = &after[sep_len..];
    let from = from.trim();
    let to = to.trim();
    if from.is_empty() || to.is_empty() {
        return None;
    }
    Some((value, from.into(), to.into()))
}

fn candidate(amount: f64, from: &str, to: &str, result: f64, fetched_at: i64) -> Candidate {
    let display = format_money(result);
    let age = format_age(now_secs() - fetched_at);
    Candidate {
        id: format!("ccy::{display}"),
        title: format!("{display} {to}"),
        subtitle: Some(format!("{} {from} → {to}  ·  rates {age}", format_money(amount))),
        icon: Icon::SfSymbol("dollarsign.circle.fill".into()),
        kind: CandidateKind::Action,
        actions: vec![Action::primary("Copy")],
        search_text: String::new(),
        bypass_rank: true,
    }
}

fn format_money(v: f64) -> String {
    // 2-4 decimal places depending on magnitude; strip trailing zeros
    let precision = if v.abs() >= 1.0 { 2 } else { 4 };
    let s = format!("{v:.*}", precision);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

fn format_age(age_secs: i64) -> &'static str {
    match age_secs {
        ..=60 => "updated now",
        61..=3_600 => "updated within the hour",
        3_601..=86_400 => "updated today",
        86_401..=172_800 => "updated yesterday",
        _ => "updated this week",
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn cache_path() -> PathBuf {
    dirs::data_local_dir()
        .map(|b| b.join("Gyors"))
        .unwrap_or_else(|| PathBuf::from(".gyors"))
        .join("rates.json")
}

fn load_cached() -> Option<CurrencyRates> {
    let path = cache_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_cache(rates: &CurrencyRates) -> Result<()> {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string(rates)?)?;
    Ok(())
}

/// Fetch the latest rates from open.er-api.com using system curl. Kept
/// as a plain blocking fn so it slots into `spawn_blocking` and
/// avoids a new dependency (reqwest, ureq, etc.)
pub fn fetch_rates() -> Option<CurrencyRates> {
    let out = Command::new("/usr/bin/curl")
        .args([
            "-s",
            "--max-time",
            "8",
            "-A",
            "gyors/0.1",
            "https://open.er-api.com/v6/latest/USD",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&out.stdout);
    parse_er_api_response(&body)
}

/// Parse the open.er-api.com JSON payload into our cache shape.
/// Exposed for tests so we can exercise the mapping logic without a
/// live network call
pub fn parse_er_api_response(body: &str) -> Option<CurrencyRates> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if v.get("result")?.as_str()? != "success" {
        return None;
    }
    let base = v.get("base_code")?.as_str()?.to_string();
    let rates_obj = v.get("rates")?.as_object()?;
    let rates: HashMap<String, f64> = rates_obj
        .iter()
        .filter_map(|(k, val)| val.as_f64().map(|f| (k.clone(), f)))
        .collect();
    if rates.is_empty() {
        return None;
    }
    Some(CurrencyRates { base, rates, fetched_at: now_secs() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(rates: &[(&str, f64)]) -> CurrencyProvider {
        let mut map = HashMap::new();
        for (k, v) in rates {
            map.insert((*k).to_string(), *v);
        }
        let cache = CurrencyRates { base: "USD".into(), rates: map, fetched_at: now_secs() };
        CurrencyProvider {
            rates: Arc::new(ArcSwap::from(Arc::new(Some(cache)))),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn parse_basic_forms() {
        assert_eq!(
            parse_conversion("100 usd to eur"),
            Some((100.0, "usd".into(), "eur".into()))
        );
        assert_eq!(
            parse_conversion("50 EUR in GBP"),
            Some((50.0, "eur".into(), "gbp".into()))
        );
    }

    #[test]
    fn parse_rejects_bad_shape() {
        assert!(parse_conversion("foo bar baz").is_none());
        assert!(parse_conversion("100").is_none());
    }

    #[test]
    fn parse_strips_trailing_question_mark() {
        // REGRESSION (2026-04-28): user typed "010 usd in huf?" and
        // got an AI free-form "you'd need an exchange rate" answer
        // instead of a currency conversion. The trailing `?` made
        // `to` end up as `"huf?"` which has no rate entry
        assert_eq!(
            parse_conversion("010 usd in huf?"),
            Some((10.0, "usd".into(), "huf".into()))
        );
        assert_eq!(
            parse_conversion("100 usd in eur."),
            Some((100.0, "usd".into(), "eur".into()))
        );
        assert_eq!(
            parse_conversion("50 EUR in GBP!"),
            Some((50.0, "eur".into(), "gbp".into()))
        );
        assert_eq!(
            parse_conversion("75 jpy to krw,"),
            Some((75.0, "jpy".into(), "krw".into()))
        );
    }

    #[test]
    fn parse_handles_leading_zero_in_amount() {
        // F64 already accepts leading zeros, but pin the behaviour
        // so a future "strict integer" parser change doesn't
        // silently drop user's `010 usd ...` query
        assert_eq!(
            parse_conversion("010 usd to eur"),
            Some((10.0, "usd".into(), "eur".into()))
        );
        assert_eq!(
            parse_conversion("0.5 usd in eur"),
            Some((0.5, "usd".into(), "eur".into()))
        );
    }

    #[test]
    fn parse_er_api_response_happy_path() {
        let body = r#"{
            "result":"success",
            "base_code":"USD",
            "rates":{"EUR":0.92,"GBP":0.79,"USD":1.0}
        }"#;
        let rates = parse_er_api_response(body).unwrap();
        assert_eq!(rates.base, "USD");
        assert!(close(*rates.rates.get("EUR").unwrap(), 0.92));
        assert!(close(*rates.rates.get("USD").unwrap(), 1.0));
    }

    #[test]
    fn parse_er_api_response_rejects_error_payload() {
        let body = r#"{"result":"error","error-type":"invalid-key"}"#;
        assert!(parse_er_api_response(body).is_none());
    }

    #[test]
    fn parse_er_api_response_rejects_malformed() {
        assert!(parse_er_api_response("not json").is_none());
        assert!(parse_er_api_response("{}").is_none());
    }

    #[tokio::test]
    async fn query_converts_usd_to_eur_via_pivot() {
        let p = mk(&[("USD", 1.0), ("EUR", 0.92), ("GBP", 0.79)]);
        let out = p.query(&Query::new("100 usd to eur")).await;
        assert_eq!(out.len(), 1);
        // 100 USD x (0.92 / 1.0) = 92 EUR
        assert!(out[0].title.contains("92"));
    }

    #[tokio::test]
    async fn query_converts_eur_to_gbp_cross_rate() {
        let p = mk(&[("USD", 1.0), ("EUR", 0.92), ("GBP", 0.79)]);
        let out = p.query(&Query::new("100 eur to gbp")).await;
        assert_eq!(out.len(), 1);
        // 100 EUR x (0.79 / 0.92) ~ 85.87 GBP
        assert!(out[0].title.contains("85"), "got {:?}", out[0].title);
    }

    #[tokio::test]
    async fn query_case_insensitive_codes() {
        let p = mk(&[("USD", 1.0), ("EUR", 0.92)]);
        let lc = p.query(&Query::new("50 usd to eur")).await;
        let uc = p.query(&Query::new("50 USD to EUR")).await;
        assert_eq!(lc[0].title, uc[0].title);
    }

    #[tokio::test]
    async fn query_empty_when_code_unknown() {
        let p = mk(&[("USD", 1.0), ("EUR", 0.92)]);
        assert!(p.query(&Query::new("100 usd to xyz")).await.is_empty());
    }

    #[tokio::test]
    async fn query_empty_when_no_cache() {
        let p = CurrencyProvider {
            rates: Arc::new(ArcSwap::from(Arc::new(None))),
        };
        assert!(p.query(&Query::new("100 usd to eur")).await.is_empty());
    }

    #[tokio::test]
    async fn query_empty_when_cache_hard_stale() {
        let mut map = HashMap::new();
        map.insert("USD".into(), 1.0);
        map.insert("EUR".into(), 0.92);
        let cache = CurrencyRates {
            base: "USD".into(),
            rates: map,
            fetched_at: now_secs() - CACHE_HARD_MAX_SECS - 1,
        };
        let p = CurrencyProvider {
            rates: Arc::new(ArcSwap::from(Arc::new(Some(cache)))),
        };
        assert!(p.query(&Query::new("100 usd to eur")).await.is_empty());
    }

    #[tokio::test]
    async fn activate_copies_value() {
        let p = mk(&[("USD", 1.0), ("EUR", 0.92)]);
        let out = p.query(&Query::new("100 usd to eur")).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => assert_eq!(s, "92"),
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_foreign_prefix_errors() {
        let p = mk(&[]);
        assert!(p.activate(&"apps::x".to_string(), "default").await.is_err());
    }

    #[test]
    fn format_money_rounds_nicely() {
        assert_eq!(format_money(1.0), "1");
        assert_eq!(format_money(1.5), "1.5");
        assert_eq!(format_money(1.234), "1.23");
        // Sub-dollar values get more precision
        assert_eq!(format_money(0.00123), "0.0012");
    }
}
