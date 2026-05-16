//! Timezone / world clock. Keywords: `tz <city>`, `time in <city>`
//!
//! Ships with a curated city -> IANA-tz map so users can type familiar
//! names ("tokyo", "new york") instead of "Asia/Tokyo". Matching is
//! case-insensitive substring on either the city alias or the tz id, so
//! `tz paris` and `tz europe/paris` both work. Activating a row copies
//! formatted local time

use async_trait::async_trait;
use chrono::Utc;
use chrono_tz::Tz;
use gyors_core::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon, Provider, Query};

pub struct TimezoneProvider;

const RESULT_LIMIT: usize = 10;

struct CityEntry {
    aliases: &'static [&'static str],
    tz: &'static str,
}

const CITIES: &[CityEntry] = &[
    CityEntry { aliases: &["tokyo"], tz: "Asia/Tokyo" },
    CityEntry { aliases: &["london"], tz: "Europe/London" },
    CityEntry { aliases: &["new york", "nyc"], tz: "America/New_York" },
    CityEntry { aliases: &["los angeles", "la"], tz: "America/Los_Angeles" },
    CityEntry { aliases: &["san francisco", "sf"], tz: "America/Los_Angeles" },
    CityEntry { aliases: &["chicago"], tz: "America/Chicago" },
    CityEntry { aliases: &["toronto"], tz: "America/Toronto" },
    CityEntry { aliases: &["mexico city"], tz: "America/Mexico_City" },
    CityEntry { aliases: &["são paulo", "sao paulo"], tz: "America/Sao_Paulo" },
    CityEntry { aliases: &["buenos aires"], tz: "America/Argentina/Buenos_Aires" },
    CityEntry { aliases: &["santiago"], tz: "America/Santiago" },
    CityEntry { aliases: &["bogota"], tz: "America/Bogota" },
    CityEntry { aliases: &["lima"], tz: "America/Lima" },
    CityEntry { aliases: &["paris"], tz: "Europe/Paris" },
    CityEntry { aliases: &["berlin"], tz: "Europe/Berlin" },
    CityEntry { aliases: &["madrid"], tz: "Europe/Madrid" },
    CityEntry { aliases: &["rome"], tz: "Europe/Rome" },
    CityEntry { aliases: &["amsterdam"], tz: "Europe/Amsterdam" },
    CityEntry { aliases: &["vienna"], tz: "Europe/Vienna" },
    CityEntry { aliases: &["zurich"], tz: "Europe/Zurich" },
    CityEntry { aliases: &["stockholm"], tz: "Europe/Stockholm" },
    CityEntry { aliases: &["oslo"], tz: "Europe/Oslo" },
    CityEntry { aliases: &["helsinki"], tz: "Europe/Helsinki" },
    CityEntry { aliases: &["copenhagen"], tz: "Europe/Copenhagen" },
    CityEntry { aliases: &["warsaw"], tz: "Europe/Warsaw" },
    CityEntry { aliases: &["prague"], tz: "Europe/Prague" },
    CityEntry { aliases: &["athens"], tz: "Europe/Athens" },
    CityEntry { aliases: &["budapest"], tz: "Europe/Budapest" },
    CityEntry { aliases: &["dublin"], tz: "Europe/Dublin" },
    CityEntry { aliases: &["istanbul"], tz: "Europe/Istanbul" },
    CityEntry { aliases: &["moscow"], tz: "Europe/Moscow" },
    CityEntry { aliases: &["lisbon"], tz: "Europe/Lisbon" },
    CityEntry { aliases: &["dubai"], tz: "Asia/Dubai" },
    CityEntry { aliases: &["riyadh"], tz: "Asia/Riyadh" },
    CityEntry { aliases: &["tel aviv", "jerusalem"], tz: "Asia/Jerusalem" },
    CityEntry { aliases: &["mumbai", "bombay"], tz: "Asia/Kolkata" },
    CityEntry { aliases: &["delhi", "new delhi"], tz: "Asia/Kolkata" },
    CityEntry { aliases: &["bangkok"], tz: "Asia/Bangkok" },
    CityEntry { aliases: &["singapore"], tz: "Asia/Singapore" },
    CityEntry { aliases: &["hong kong", "hk"], tz: "Asia/Hong_Kong" },
    CityEntry { aliases: &["beijing", "shanghai"], tz: "Asia/Shanghai" },
    CityEntry { aliases: &["seoul"], tz: "Asia/Seoul" },
    CityEntry { aliases: &["taipei"], tz: "Asia/Taipei" },
    CityEntry { aliases: &["jakarta"], tz: "Asia/Jakarta" },
    CityEntry { aliases: &["manila"], tz: "Asia/Manila" },
    CityEntry { aliases: &["ho chi minh", "saigon"], tz: "Asia/Ho_Chi_Minh" },
    CityEntry { aliases: &["sydney"], tz: "Australia/Sydney" },
    CityEntry { aliases: &["melbourne"], tz: "Australia/Melbourne" },
    CityEntry { aliases: &["brisbane"], tz: "Australia/Brisbane" },
    CityEntry { aliases: &["perth"], tz: "Australia/Perth" },
    CityEntry { aliases: &["auckland"], tz: "Pacific/Auckland" },
    CityEntry { aliases: &["cairo"], tz: "Africa/Cairo" },
    CityEntry { aliases: &["nairobi"], tz: "Africa/Nairobi" },
    CityEntry { aliases: &["lagos"], tz: "Africa/Lagos" },
    CityEntry { aliases: &["johannesburg"], tz: "Africa/Johannesburg" },
    CityEntry { aliases: &["casablanca"], tz: "Africa/Casablanca" },
    CityEntry { aliases: &["honolulu"], tz: "Pacific/Honolulu" },
    CityEntry { aliases: &["anchorage"], tz: "America/Anchorage" },
    CityEntry { aliases: &["vancouver"], tz: "America/Vancouver" },
];

#[async_trait]
impl Provider for TimezoneProvider {
    fn id(&self) -> &str {
        "tz"
    }

    async fn query(&self, query: &Query) -> Vec<Candidate> {
        let pattern = query.pattern();
        let Some(filter) = strip_keyword(pattern) else { return vec![]; };
        let filter = filter.trim().to_lowercase();
        if filter.is_empty() {
            return vec![];
        }

        let now = Utc::now();
        let mut out: Vec<Candidate> = Vec::new();
        for entry in CITIES.iter() {
            if !entry_matches(entry, &filter) { continue; }
            let tz: Tz = match entry.tz.parse() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let local = now.with_timezone(&tz);
            let time_str = local.format("%H:%M").to_string();
            let date_str = local.format("%a %b %e").to_string();
            let offset = local.format("%:z").to_string();
            let title = format!("{} · {}", entry.aliases[0], time_str);
            let subtitle = format!("{} · UTC{}", date_str, offset);
            out.push(Candidate {
                id: format!("tz::{}", entry.tz),
                title: capitalize_words(&title),
                subtitle: Some(subtitle),
                icon: Icon::SfSymbol("globe.europe.africa.fill".into()),
                kind: CandidateKind::Action,
                actions: vec![Action::primary("Copy Time")],
                search_text: format!(
                    "{} {}",
                    entry.aliases.join(" "),
                    entry.tz
                ),
                bypass_rank: true,
            });
            if out.len() >= RESULT_LIMIT { break; }
        }
        out
    }

    async fn activate(&self, id: &CandidateId, _action: &str) -> anyhow::Result<Effect> {
        let tz_id = id
            .strip_prefix("tz::")
            .ok_or_else(|| anyhow::anyhow!("invalid tz candidate id: {id}"))?;
        let tz: Tz = tz_id
            .parse()
            .map_err(|_| anyhow::anyhow!("unknown timezone: {tz_id}"))?;
        let now = Utc::now().with_timezone(&tz);
        let pretty = now.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        Ok(Effect::CopyToClipboard(pretty))
    }
}

fn strip_keyword(s: &str) -> Option<&str> {
    s.strip_prefix("tz ")
        .or_else(|| s.strip_prefix("time in "))
        .or_else(|| s.strip_prefix("timezone "))
}

fn entry_matches(entry: &CityEntry, filter_lower: &str) -> bool {
    if entry.tz.to_lowercase().contains(filter_lower) {
        return true;
    }
    entry
        .aliases
        .iter()
        .any(|a| a.to_lowercase().contains(filter_lower))
}

fn capitalize_words(s: &str) -> String {
    s.split_inclusive(char::is_whitespace)
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_match_without_keyword() {
        let p = TimezoneProvider;
        assert!(p.query(&Query::new("tokyo")).await.is_empty());
    }

    #[tokio::test]
    async fn bare_keyword_returns_empty() {
        let p = TimezoneProvider;
        assert!(p.query(&Query::new("tz ")).await.is_empty());
    }

    #[tokio::test]
    async fn city_lookup_tz_keyword() {
        let p = TimezoneProvider;
        let out = p.query(&Query::new("tz tokyo")).await;
        assert!(!out.is_empty());
        assert_eq!(out[0].id, "tz::Asia/Tokyo");
    }

    #[tokio::test]
    async fn time_in_keyword_works() {
        let p = TimezoneProvider;
        let out = p.query(&Query::new("time in london")).await;
        assert_eq!(out[0].id, "tz::Europe/London");
    }

    #[tokio::test]
    async fn timezone_alias_resolves_to_canonical_tz() {
        let p = TimezoneProvider;
        let la = p.query(&Query::new("tz la")).await;
        let sf = p.query(&Query::new("tz sf")).await;
        assert_eq!(la[0].id, "tz::America/Los_Angeles");
        assert_eq!(sf[0].id, "tz::America/Los_Angeles");
    }

    #[tokio::test]
    async fn iana_id_substring_also_matches() {
        let p = TimezoneProvider;
        let out = p.query(&Query::new("tz europe/")).await;
        assert!(out.iter().all(|c| c.id.contains("Europe/")));
        assert!(out.len() > 1);
    }

    #[tokio::test]
    async fn no_match_yields_empty() {
        let p = TimezoneProvider;
        assert!(p.query(&Query::new("tz xyznotacity")).await.is_empty());
    }

    #[tokio::test]
    async fn subtitle_contains_utc_offset() {
        let p = TimezoneProvider;
        let out = p.query(&Query::new("tz tokyo")).await;
        let sub = out[0].subtitle.as_deref().unwrap();
        assert!(sub.contains("UTC"), "got {sub:?}");
    }

    #[tokio::test]
    async fn activate_copies_formatted_time() {
        let p = TimezoneProvider;
        let out = p.query(&Query::new("tz tokyo")).await;
        let eff = p.activate(&out[0].id, "default").await.unwrap();
        match eff {
            Effect::CopyToClipboard(s) => {
                assert!(s.contains(":"), "looks like a time: {s:?}");
                // JST is the Tokyo abbrev - present via %Z
                assert!(s.contains("JST") || s.contains("+0900") || s.contains("+09"));
            }
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn activate_unknown_tz_errors() {
        let p = TimezoneProvider;
        assert!(p
            .activate(&"tz::Nowhere/Nowhere".to_string(), "default")
            .await
            .is_err());
    }

    #[test]
    fn capitalize_words_basic() {
        assert_eq!(capitalize_words("tokyo · 12:34"), "Tokyo · 12:34");
        assert_eq!(capitalize_words("new york · 08:00"), "New York · 08:00");
    }
}
