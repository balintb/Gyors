use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_VISITS_PER_ENTRY: usize = 20;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Frecency {
    entries: HashMap<String, Entry>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Entry {
    visits: Vec<i64>,
}

impl Frecency {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, id: &str) {
        self.record_at(id, now_secs());
    }

    pub fn record_at(&mut self, id: &str, ts: i64) {
        let entry = self.entries.entry(id.to_string()).or_default();
        entry.visits.push(ts);
        if entry.visits.len() > MAX_VISITS_PER_ENTRY {
            let drop = entry.visits.len() - MAX_VISITS_PER_ENTRY;
            entry.visits.drain(..drop);
        }
    }

    pub fn score(&self, id: &str) -> f64 {
        self.score_at(id, now_secs())
    }

    pub fn score_at(&self, id: &str, now: i64) -> f64 {
        let Some(entry) = self.entries.get(id) else { return 0.0; };
        entry.visits.iter().map(|&ts| weight(now - ts)).sum()
    }
}

fn weight(age_secs: i64) -> f64 {
    let days = age_secs as f64 / 86_400.0;
    match days {
        d if d < 4.0 => 100.0,
        d if d < 14.0 => 70.0,
        d if d < 31.0 => 50.0,
        d if d < 90.0 => 30.0,
        _ => 10.0,
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

    #[test]
    fn empty_score_is_zero() {
        let f = Frecency::new();
        assert_eq!(f.score("nope"), 0.0);
    }

    #[test]
    fn fresh_visit_scores_100() {
        let mut f = Frecency::new();
        let now = 1_700_000_000;
        f.record_at("safari", now);
        assert_eq!(f.score_at("safari", now), 100.0);
    }

    #[test]
    fn decay_buckets() {
        let mut f = Frecency::new();
        let now = 1_700_000_000;
        let day = 86_400;
        f.record_at("a", now - 3 * day);
        f.record_at("a", now - 10 * day);
        f.record_at("a", now - 20 * day);
        f.record_at("a", now - 60 * day);
        f.record_at("a", now - 120 * day);
        assert_eq!(f.score_at("a", now), 100.0 + 70.0 + 50.0 + 30.0 + 10.0);
    }

    #[test]
    fn visits_are_capped() {
        let mut f = Frecency::new();
        for i in 0..50 {
            f.record_at("a", i);
        }
        // All visits ancient relative to far-future now -> each weighted 10.0
        assert_eq!(f.score_at("a", 10_000_000_000), 20.0 * 10.0);
    }
}
