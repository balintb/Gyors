//! Persistent storage for Gyors: SQLite-backed visit log + clipboard history

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, RwLock};

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS visits (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    candidate_id  TEXT    NOT NULL,
    ts            INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_visits_candidate_ts
    ON visits(candidate_id, ts DESC);

CREATE TABLE IF NOT EXISTS clipboard_items (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    content  TEXT    NOT NULL,
    ts       INTEGER NOT NULL,
    -- Content-addressed sync identifier: blake3(content || ts)[..16],
    -- in hex. Lets `apply(Delete)` find the local row from the
    -- server's tombstone without round-tripping through content+ts.
    -- Nullable for the migration window: existing rows get backfilled
    -- on `open` via `ensure_clipboard_sync_id`. New rows always
    -- populate it.
    sync_id  TEXT
);
CREATE INDEX IF NOT EXISTS idx_clipboard_ts ON clipboard_items(ts DESC);
-- The `idx_clipboard_sync_id` index gets created inside
-- `ensure_clipboard_sync_id`, AFTER the ALTER-TABLE migration runs.
-- An existing DB that predates the column wouldn't let us create
-- the index here - the SCHEMA `CREATE TABLE IF NOT EXISTS` no-ops
-- when the table already exists, so the column doesn't appear until
-- the ALTER runs.

CREATE TABLE IF NOT EXISTS query_history (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    pattern  TEXT    NOT NULL,
    ts       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_query_history_ts ON query_history(ts DESC);

-- Sync outbox: every local change that needs to ship to the sync
-- server lands here first. ONE generic table for every resource
-- kind (clipboard, snippet, theme, …) so the sync worker reads
-- from a single queue rather than per-kind feeds. `payload` is the
-- *unencrypted* serialised body - `gyors-sync` encrypts in-flight
-- so the Keychain-resident master key never round-trips through
-- SQL. `attempts` + `last_error` drive exponential backoff on
-- flaky networks. `op` distinguishes upserts from deletions
-- (tombstones); the resource-kind impl decides what `payload`
-- means for each.
CREATE TABLE IF NOT EXISTS sync_outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    resource_kind   TEXT    NOT NULL,
    resource_id     TEXT    NOT NULL,
    op              TEXT    NOT NULL,
    payload         BLOB    NOT NULL,
    ts_local        INTEGER NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT,
    -- Exponential-backoff gate: a row whose `next_attempt_at` is in
    -- the future is skipped by `outbox_drain`. Defaults to 0 so a
    -- fresh row drains on the next tick. `outbox_fail` recomputes
    -- this after every failure (`attempts^2 * 60s`).
    next_attempt_at INTEGER NOT NULL DEFAULT 0,
    -- Dead-letter marker. Set by `outbox_abandon` when a row has
    -- exhausted its retry budget. `outbox_drain` ignores abandoned
    -- rows so they don't replay forever, but we keep them in-table
    -- (rather than dropping) so diagnostics can show what got stuck.
    abandoned_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_sync_outbox_kind_ts
    ON sync_outbox(resource_kind, ts_local);

-- Per-(namespace, item) version counter. The server enforces strict
-- monotonicity on push, so we track the last version we *successfully
-- shipped* here and bump on every fresh upsert. Conflict outcomes
-- update this row to the server's reported version, so the next push
-- attempt rides on top of whatever the other device wrote. Acts as the
-- pull high-water-mark too - `cursor_b64` holds the opaque cursor the
-- server returned on the last `/v1/sync/pull` call, scoped to a single
-- row keyed by `('__cursor__', '')`. Special-cased in code, ugly in
-- SQL; the alternative is a second table for one row.
CREATE TABLE IF NOT EXISTS sync_versions (
    namespace  TEXT    NOT NULL,
    item_id    TEXT    NOT NULL,
    version    INTEGER NOT NULL,
    cursor_b64 TEXT,
    PRIMARY KEY (namespace, item_id)
);
"#;

/// Default local clipboard retention when user hasn't set
/// `clipboard_max_items` in config. Plenty of headroom for normal
/// use; users with privacy concerns can dial it down explicitly
pub const CLIPBOARD_RETENTION_DEFAULT: usize = 500;

/// Cap on persisted query-history rows. Old entries roll off the back
pub const QUERY_HISTORY_RETENTION: usize = 500;

/// Cap on the per-id visit history we keep in memory. The frecency
/// formula only consults the most recent `max_visits` anyway (default
/// 20), and the weight of a 6-month-old visit is ~0, so theres no
/// point paying the memory or recompute cost for ancient history
const VISIT_MEMORY_CAP: usize = 64;
/// Hard cap on the on-disk `visits` table per candidate id. Bigger
/// than the in-memory cache so SQL fallback in `frecency_score`
/// has room to breathe, but still bounded - without this, every
/// activation forever appended to the table. 256 newest visits per
/// id is far more frecency signal than the decay-weighted score
/// can usefully consume (visits older than ~120 days weight to
/// zero in `weight()`). Public so IPC soak test can assert
/// against the production constant without redefining it
pub const VISIT_DB_CAP: usize = 256;

pub struct Index {
    conn: Mutex<Connection>,
    /// In-memory mirror of the `visits` table, keyed by candidate id.
    /// Populated at `open()` time by `load_visits_into_cache`, then
    /// appended-to on every `record_visit`. The hot path
    /// (`frecency_scores_bulk`) reads this map - zero SQL per
    /// keystroke, which is a 5-10x speed-up on apps-heavy queries
    /// with history. SQLite is the persistence substrate; this cache
    /// is the read accelerator
    visits: RwLock<HashMap<String, Vec<i64>>>,
    /// Local clipboard retention cap. Driven by the
    /// `clipboard_max_items` config field; tier does not influence
    /// this - sync gating happens at the engine layer instead.
    /// Reads happen on every `record_clipboard` call so we keep
    /// this lock-free via `AtomicUsize`
    clipboard_cap: AtomicUsize,
}

/// One queued sync change. Body is the resource-kind's
/// already-serialised payload - the outbox doesn't peek inside
#[derive(Debug, Clone)]
pub struct OutboxEntry {
    pub id: i64,
    pub resource_kind: String,
    pub resource_id: String,
    pub op: String,
    pub payload: Vec<u8>,
    pub ts_local: i64,
    pub attempts: i64,
}

#[derive(Debug, Clone)]
pub struct ClipboardItem {
    pub id: i64,
    pub content: String,
    pub ts: i64,
}

impl Index {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref()).context("open sqlite db")?;
        conn.execute_batch(SCHEMA)?;
        ensure_outbox_columns(&conn)?;
        ensure_clipboard_sync_id(&conn)?;
        let visits = load_visits_into_cache(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            visits: RwLock::new(visits),
            clipboard_cap: AtomicUsize::new(CLIPBOARD_RETENTION_DEFAULT),
        })
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        ensure_outbox_columns(&conn)?;
        ensure_clipboard_sync_id(&conn)?;
        let visits = load_visits_into_cache(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            visits: RwLock::new(visits),
            clipboard_cap: AtomicUsize::new(CLIPBOARD_RETENTION_DEFAULT),
        })
    }


    /// Total row count in the `visits` table. Public so soak tests
    /// in dependent crates can assert bounded growth without
    /// reaching into the private SQLite handle
    pub fn visits_total_rows(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM visits", [], |r| r.get(0))
            .map_err(Into::into)
    }

    /// Max row count for any single `candidate_id` in the visits
    /// table. Used by soak tests to verify the per-id cap holds.
    /// Returns 0 when the table is empty
    pub fn visits_max_rows_per_id(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(MAX(c), 0) FROM \
             (SELECT COUNT(*) AS c FROM visits GROUP BY candidate_id)",
            [],
            |r| r.get(0),
        )
        .map_err(Into::into)
    }

    pub fn record_visit(&self, candidate_id: &str, ts: i64) -> Result<()> {
        // Persist first - if SQLite errors we dont want the in-memory
        // cache to diverge from what we'll load next launch
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO visits (candidate_id, ts) VALUES (?1, ?2)",
            params![candidate_id, ts],
        )?;
        // Per-id table cap - without this, every activation appends
        // forever and the visits table grows unbounded. The
        // in-memory cache caps at VISIT_MEMORY_CAP, but the SQLite
        // table fed `load_visits_into_cache` at startup AND the
        // `frecency_score` SQL fallback both read the table directly.
        // Mirror clipboard / query history pattern: keep the
        // newest VISIT_DB_CAP rows per id, drop the rest
        conn.execute(
            "DELETE FROM visits
             WHERE candidate_id = ?1
               AND ts NOT IN (
                 SELECT ts FROM visits
                 WHERE candidate_id = ?1
                 ORDER BY ts DESC
                 LIMIT ?2
               )",
            params![candidate_id, VISIT_DB_CAP as i64],
        )?;
        drop(conn); // release before we take the visits write lock
        let mut guard = self.visits.write().unwrap();
        let entry = guard.entry(candidate_id.to_string()).or_default();
        entry.insert(0, ts); // newest first
        if entry.len() > VISIT_MEMORY_CAP {
            entry.truncate(VISIT_MEMORY_CAP);
        }
        Ok(())
    }

    /// Mozilla-style frecency: decay-weighted sum over the last `max_visits` visits
    pub fn frecency_score(&self, candidate_id: &str, now: i64, max_visits: usize) -> Result<f64> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT ts FROM visits
             WHERE candidate_id = ?1
             ORDER BY ts DESC
             LIMIT ?2",
        )?;
        let visits: Vec<i64> = stmt
            .query_map(params![candidate_id, max_visits as i64], |r| {
                r.get::<_, i64>(0)
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(visits.iter().map(|&ts| weight(now - ts)).sum())
    }

    /// Bulk frecency - zero-SQL hot path. Reads the in-memory visits
    /// map; a normal keystroke now runs in microseconds regardless of
    /// history size. Persistence (SQLite) only ticks on actual visits
    /// (Enter / activate), not on every keystroke
    ///
    /// Returns a score of 0.0 for any id that has never been visited,
    /// so callers can look up each id without a `contains_key` dance
    pub fn frecency_scores_bulk(
        &self,
        candidate_ids: &[&str],
        now: i64,
        max_visits: usize,
    ) -> Result<HashMap<String, f64>> {
        if candidate_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let guard = self.visits.read().unwrap();
        let mut out = HashMap::with_capacity(candidate_ids.len());
        for &id in candidate_ids {
            if let Some(visits) = guard.get(id) {
                let score: f64 = visits
                    .iter()
                    .take(max_visits)
                    .map(|&ts| weight(now - ts))
                    .sum();
                if score > 0.0 {
                    out.insert(id.to_string(), score);
                }
            }
        }
        Ok(out)
    }


    /// Record a new clipboard item. Empty or whitespace-only content is
    /// ignored. Consecutive duplicates (same content as the most recent
    /// row) are also ignored. Prunes the table to CLIPBOARD_RETENTION
    ///
    /// Populates `sync_id` with the content-addressed identifier used
    /// by the cloud-sync engine so server-side delete tombstones can
    /// find the local row via `delete_by_sync_id`
    pub fn record_clipboard(&self, content: &str, ts: i64) -> Result<bool> {
        if content.trim().is_empty() {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let last: Option<String> = conn
            .query_row(
                "SELECT content FROM clipboard_items ORDER BY ts DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        if last.as_deref() == Some(content) {
            return Ok(false);
        }
        let sid = clipboard_sync_id(content, ts);
        conn.execute(
            "INSERT INTO clipboard_items (content, ts, sync_id) VALUES (?1, ?2, ?3)",
            params![content, ts, sid],
        )?;
        let cap = self.clipboard_cap.load(Ordering::Relaxed);
        conn.execute(
            "DELETE FROM clipboard_items
             WHERE id NOT IN (
                 SELECT id FROM clipboard_items ORDER BY ts DESC LIMIT ?1
             )",
            params![cap as i64],
        )?;
        Ok(true)
    }

    /// Remove clipboard row whose `sync_id` matches. Used by
    /// `ClipboardResource::apply(Delete)` when a tombstone arrives
    /// from another device. Returns `true` if a row was removed,
    /// `false` if no local row matched (the tombstone applies to
    /// something we never had, or already evicted from retention)
    ///
    /// Idempotent: a duplicate tombstone is a no-op rather than an
    /// error - the engine retries on flaky networks and we dont
    /// want a spurious failure to dead-letter a tombstone whose
    /// effect was already realised
    pub fn delete_clipboard_by_sync_id(&self, sync_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute(
            "DELETE FROM clipboard_items WHERE sync_id = ?1",
            params![sync_id],
        )?;
        Ok(rows > 0)
    }

    /// Set the cap and prune anything beyond it. Called on init
    /// (reads `clipboard_max_items` from config) and after user
    /// edits that field. The prune is part of the setter so dialing
    /// value DOWN takes immediate effect - user shouldn't
    /// keep seeing 500 items after they set it to 50
    pub fn set_clipboard_cap(&self, cap: usize) -> Result<()> {
        let cap = cap.max(1);
        self.clipboard_cap.store(cap, Ordering::Relaxed);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM clipboard_items
             WHERE id NOT IN (
                 SELECT id FROM clipboard_items ORDER BY ts DESC LIMIT ?1
             )",
            params![cap as i64],
        )?;
        Ok(())
    }

    pub fn clipboard_cap(&self) -> usize {
        self.clipboard_cap.load(Ordering::Relaxed)
    }

    /// Fuzzy-ish filter via SQL `LIKE`. Returns most recent matches first
    pub fn clipboard_search(&self, filter: &str, limit: usize) -> Result<Vec<ClipboardItem>> {
        let conn = self.conn.lock().unwrap();
        let like = format!("%{}%", escape_like(filter));
        let mut stmt = conn.prepare(
            "SELECT id, content, ts FROM clipboard_items
             WHERE content LIKE ?1 ESCAPE '\\'
             ORDER BY ts DESC
             LIMIT ?2",
        )?;
        let items = stmt
            .query_map(params![like, limit as i64], row_to_item)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(items)
    }

    pub fn clipboard_recent(&self, limit: usize) -> Result<Vec<ClipboardItem>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, content, ts FROM clipboard_items
             ORDER BY ts DESC
             LIMIT ?1",
        )?;
        let items = stmt
            .query_map(params![limit as i64], row_to_item)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(items)
    }

    pub fn clipboard_get(&self, id: i64) -> Result<Option<ClipboardItem>> {
        let conn = self.conn.lock().unwrap();
        let item = conn
            .query_row(
                "SELECT id, content, ts FROM clipboard_items WHERE id = ?1",
                params![id],
                row_to_item,
            )
            .ok();
        Ok(item)
    }

    pub fn clipboard_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM clipboard_items", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    /// Delete all clipboard history rows
    pub fn clear_clipboard_history(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute("DELETE FROM clipboard_items", [])?;
        Ok(rows)
    }

    //
    // Generic queue for the sync engine. Resource-kind-agnostic - the
    // engine reads rows, hands them off to matching `SyncResource`
    // impl for serialisation/encryption, ships them, and records the
    // outcome (delete on success, increment attempts on failure)

    /// Enqueue a change. `payload` is the resource-kind's already-
    /// serialised body (the kind owns schema); the outbox doesn't
    /// peek inside it. Returns the new row id
    pub fn outbox_enqueue(
        &self,
        resource_kind: &str,
        resource_id: &str,
        op: &str,
        payload: &[u8],
        ts_local: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sync_outbox \
                 (resource_kind, resource_id, op, payload, ts_local) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![resource_kind, resource_id, op, payload, ts_local],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Pop next `limit` queued entries, oldest first. Used by the
    /// sync worker each tick. Caller is responsible for marking each
    /// row complete via `outbox_ack` or `outbox_fail` afterwards -
    /// rows stay in the table until acked, so a worker crash mid-flight
    /// re-tries on next start
    ///
    /// Skips rows that are either backoff-gated (`next_attempt_at` in
    /// the future) or dead-lettered (`abandoned_at IS NOT NULL`), so
    /// a flaky network doesn't burn CPU re-encrypting + re-pushing
    /// same row every tick
    pub fn outbox_drain(&self, kind: Option<&str>, limit: usize) -> Result<Vec<OutboxEntry>> {
        let conn = self.conn.lock().unwrap();
        let now = now_secs();
        let mut entries = Vec::new();
        match kind {
            Some(k) => {
                let mut stmt = conn.prepare(
                    "SELECT id, resource_kind, resource_id, op, payload, ts_local, attempts \
                     FROM sync_outbox \
                     WHERE resource_kind = ?1 \
                       AND abandoned_at IS NULL \
                       AND next_attempt_at <= ?2 \
                     ORDER BY ts_local ASC \
                     LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![k, now, limit as i64], outbox_row)?;
                for row in rows {
                    entries.push(row?);
                }
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, resource_kind, resource_id, op, payload, ts_local, attempts \
                     FROM sync_outbox \
                     WHERE abandoned_at IS NULL \
                       AND next_attempt_at <= ?1 \
                     ORDER BY ts_local ASC \
                     LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![now, limit as i64], outbox_row)?;
                for row in rows {
                    entries.push(row?);
                }
            }
        }
        Ok(entries)
    }

    /// Mark a row as successfully shipped - drops it from the queue
    pub fn outbox_ack(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sync_outbox WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Mark a row as failed. Increments the attempt count, records
    /// error, and schedules next attempt with quadratic
    /// backoff (`attempts^2 * 60s`). Doesn't delete - the engine
    /// decides via `outbox_abandon` whether to dead-letter after N
    /// attempts
    ///
    /// Capped at one hour between attempts so a flaky-but-not-broken
    /// link still gets retried at a useful cadence
    pub fn outbox_fail(&self, id: i64, error: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = now_secs();
        // `attempts` is incremented in same statement; we compute
        // `(attempts+1)^2 * 60` against the post-bump count so the
        // first failure waits 60s, second 240s, third 540s, ...
        // Capped at 1h. The cap keeps the worker awake on long
        // outages without DOS'ing the server when it recovers
        let backoff_sql =
            "MIN(3600, (attempts + 1) * (attempts + 1) * 60)";
        let sql = format!(
            "UPDATE sync_outbox \
             SET attempts = attempts + 1, \
                 last_error = ?2, \
                 next_attempt_at = ?3 + {backoff_sql} \
             WHERE id = ?1",
        );
        conn.execute(&sql, params![id, error, now])?;
        Ok(())
    }

    /// Dead-letter a row: stops further retries while keeping row
    /// in-table so diagnostics can show what got stuck. The engine
    /// calls this once a row has hit the per-row retry cap; users
    /// see "N items stuck" in the sync panel + can read the last
    /// error string off row
    pub fn outbox_abandon(&self, id: i64, reason: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = now_secs();
        conn.execute(
            "UPDATE sync_outbox \
             SET abandoned_at = ?2, last_error = ?3 \
             WHERE id = ?1",
            params![id, now, reason],
        )?;
        Ok(())
    }

    /// Pending row count for a resource kind. Excludes abandoned
    /// (dead-lettered) and backoff-gated rows so diagnostics
    /// surface matches what `outbox_drain` would actually return
    pub fn outbox_pending(&self, kind: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = now_secs();
        conn.query_row(
            "SELECT COUNT(*) FROM sync_outbox \
              WHERE resource_kind = ?1 \
                AND abandoned_at IS NULL \
                AND next_attempt_at <= ?2",
            params![kind, now],
            |r| r.get(0),
        )
        .map_err(Into::into)
    }

    /// Dead-letter count - rows the engine has given up on. Surfaced
    /// in the sync diagnostics panel so user knows theres
    /// something stuck that won't fix itself
    pub fn outbox_dead_count(&self, kind: Option<&str>) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let res = match kind {
            Some(k) => conn.query_row(
                "SELECT COUNT(*) FROM sync_outbox \
                  WHERE resource_kind = ?1 AND abandoned_at IS NOT NULL",
                params![k],
                |r| r.get(0),
            ),
            None => conn.query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE abandoned_at IS NOT NULL",
                params![],
                |r| r.get(0),
            ),
        };
        res.map_err(Into::into)
    }


    /// Last server-confirmed version for `(namespace, item_id)`, or
    /// `None` if we've never shipped this item. Engine reads this
    /// before pushing to compute `version = (last ?? 0) + 1`
    pub fn sync_version_get(&self, namespace: &str, item_id: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let v = conn
            .query_row(
                "SELECT version FROM sync_versions \
                 WHERE namespace = ?1 AND item_id = ?2",
                params![namespace, item_id],
                |r| r.get::<_, i64>(0),
            )
            .ok();
        Ok(v)
    }

    /// Record a server-acked version for `(namespace, item_id)`. Used
    /// after a successful push and after a `conflict` outcome (where
    /// we adopt the server's version)
    pub fn sync_version_set(&self, namespace: &str, item_id: &str, version: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sync_versions (namespace, item_id, version) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(namespace, item_id) DO UPDATE SET version = excluded.version",
            params![namespace, item_id, version],
        )?;
        Ok(())
    }

    /// Pull cursor for entire account. We store it in the same
    /// table on a sentinel row keyed by `('__cursor__', '')` to keep
    /// schema small. Opaque base64 round-tripped from the server
    pub fn sync_pull_cursor_get(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let v = conn
            .query_row(
                "SELECT cursor_b64 FROM sync_versions \
                 WHERE namespace = '__cursor__' AND item_id = ''",
                [],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        Ok(v)
    }

    pub fn sync_pull_cursor_set(&self, cursor: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sync_versions (namespace, item_id, version, cursor_b64) \
             VALUES ('__cursor__', '', 0, ?1) \
             ON CONFLICT(namespace, item_id) DO UPDATE SET cursor_b64 = excluded.cursor_b64",
            params![cursor],
        )?;
        Ok(())
    }


    /// Record a completed query. Empty / whitespace-only patterns are
    /// ignored, as are consecutive duplicates of the most recent entry -
    /// hitting Enter twice on same query shouldn't fill the ring.
    /// Prunes the table to `QUERY_HISTORY_RETENTION`
    pub fn record_query(&self, pattern: &str, ts: i64) -> Result<bool> {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let last: Option<String> = conn
            .query_row(
                "SELECT pattern FROM query_history ORDER BY ts DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        if last.as_deref() == Some(trimmed) {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO query_history (pattern, ts) VALUES (?1, ?2)",
            params![trimmed, ts],
        )?;
        conn.execute(
            "DELETE FROM query_history
             WHERE id NOT IN (
                 SELECT id FROM query_history ORDER BY ts DESC LIMIT ?1
             )",
            params![QUERY_HISTORY_RETENTION as i64],
        )?;
        Ok(true)
    }

    /// Most recent unique patterns, newest first. Duplicates within the
    /// ring collapse to the first (most recent) occurrence so up-arrow
    /// navigation never shows same query twice in a row
    pub fn recent_queries(&self, limit: usize) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let conn = self.conn.lock().unwrap();
        // `SELECT DISTINCT` would drop the ordering guarantee, so we
        // walk rows newest-first and dedupe in Rust. Cheap: limit is
        // tiny (tens), not the full 500-row retention
        let mut stmt = conn.prepare(
            "SELECT pattern FROM query_history
             ORDER BY ts DESC
             LIMIT ?1",
        )?;
        let scan_limit = (limit * 4).max(32) as i64;
        let rows = stmt.query_map(params![scan_limit], |r| r.get::<_, String>(0))?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::with_capacity(limit);
        for row in rows {
            let p = row?;
            if seen.insert(p.clone()) {
                out.push(p);
                if out.len() == limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Most recent query - what `!!` expands to. `None` if history is empty
    pub fn last_query(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT pattern FROM query_history ORDER BY ts DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .ok())
    }

    pub fn query_history_count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM query_history", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    /// Wipe persisted query history - menu action when user wants
    /// a clean slate without nuking clipboard or frecency alongside it
    pub fn clear_query_history(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let rows = conn.execute("DELETE FROM query_history", [])?;
        Ok(rows)
    }
}

fn row_to_item(r: &rusqlite::Row) -> rusqlite::Result<ClipboardItem> {
    Ok(ClipboardItem {
        id: r.get(0)?,
        content: r.get(1)?,
        ts: r.get(2)?,
    })
}

/// Unix seconds. Centralised so backoff scheduling +
/// abandoned-marker code all read from same clock
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn outbox_row(r: &rusqlite::Row) -> rusqlite::Result<OutboxEntry> {
    Ok(OutboxEntry {
        id: r.get(0)?,
        resource_kind: r.get(1)?,
        resource_id: r.get(2)?,
        op: r.get(3)?,
        payload: r.get(4)?,
        ts_local: r.get(5)?,
        attempts: r.get(6)?,
    })
}

/// Lazy migration for outbox columns added after the launcher first
/// shipped. `CREATE TABLE IF NOT EXISTS` won't add new columns to an
/// existing table; SQLite has no `ADD COLUMN IF NOT EXISTS`, so the
/// idiom is "try ADD COLUMN, swallow the duplicate-column error."
///
/// Idempotent: a fresh DB already has these columns from `SCHEMA` so
/// both adds error harmlessly. An upgraded DB picks them up here
fn ensure_outbox_columns(conn: &Connection) -> Result<()> {
    for stmt in [
        "ALTER TABLE sync_outbox ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE sync_outbox ADD COLUMN abandoned_at INTEGER",
    ] {
        match conn.execute(stmt, []) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
                if msg.contains("duplicate column name") => {}
            Err(e) => return Err(e).context("alter sync_outbox"),
        }
    }
    Ok(())
}

/// Lazy migration for `clipboard_items.sync_id`. Same shape as
/// `ensure_outbox_columns`: try to ADD COLUMN, swallow the
/// duplicate-column error, then backfill any pre-existing rows
/// whose `sync_id` is still NULL by computing it from `(content, ts)`
///
/// Backfill matters: without it, a launcher that records a clip,
/// then upgrades, then receives a tombstone from another device,
/// would fail to find the local row by sync_id and the delete would
/// silently drop. With backfill, migration completes the
/// content-addressing for every existing row
fn ensure_clipboard_sync_id(conn: &Connection) -> Result<()> {
    let add_col = conn.execute(
        "ALTER TABLE clipboard_items ADD COLUMN sync_id TEXT",
        [],
    );
    match add_col {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
            if msg.contains("duplicate column name") => {}
        Err(e) => return Err(e).context("alter clipboard_items"),
    }
    // The index is created via SCHEMA's `CREATE INDEX IF NOT EXISTS`
    // but a pre-existing DB without the column won't have it yet.
    // Running it here is idempotent
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_clipboard_sync_id ON clipboard_items(sync_id)",
        [],
    )
    .context("create idx_clipboard_sync_id")?;

    // Backfill NULL sync_ids. Read first, then update - SQLite would
    // let us write a `UPDATE ... SET sync_id = blake3(...)`-style
    // statement if blake3 were a SQLite function, but it isn't, so
    // we compute in Rust
    let mut stmt = conn.prepare(
        "SELECT id, content, ts FROM clipboard_items WHERE sync_id IS NULL",
    )?;
    let rows: Vec<(i64, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (id, content, ts) in rows {
        let sid = clipboard_sync_id(&content, ts);
        conn.execute(
            "UPDATE clipboard_items SET sync_id = ?1 WHERE id = ?2",
            params![sid, id],
        )?;
    }
    Ok(())
}

/// Content-addressed sync id for a clipboard row
///
/// `blake3(content || little_endian(ts))[..16]`, hex-encoded. The
/// formula is also implemented identically in
/// `gyors-sync/src/clipboard.rs::sync_id` - that file delegates here
/// so two never drift. Living in `gyors-index` means index
/// can populate the column without taking a dep on the sync crate
pub fn clipboard_sync_id(content: &str, ts: i64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(content.as_bytes());
    hasher.update(&ts.to_le_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest.as_bytes()[..16])
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Load entire visits table into the in-memory cache at startup.
/// Each id's vec is capped at `VISIT_MEMORY_CAP` newest-first timestamps
/// - older rows stay on disk but never load into RAM, since their
///   frecency contribution is negligible
fn load_visits_into_cache(conn: &Connection) -> Result<HashMap<String, Vec<i64>>> {
    let mut stmt =
        conn.prepare("SELECT candidate_id, ts FROM visits ORDER BY candidate_id, ts DESC")?;
    let mut out: HashMap<String, Vec<i64>> = HashMap::new();
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (id, ts) = row?;
        let entry = out.entry(id).or_default();
        if entry.len() < VISIT_MEMORY_CAP {
            entry.push(ts);
        }
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn unknown_candidate_scores_zero() {
        let idx = Index::in_memory().unwrap();
        assert_eq!(idx.frecency_score("nope", 1, 20).unwrap(), 0.0);
    }

    #[test]
    fn single_fresh_visit() {
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        idx.record_visit("safari", now).unwrap();
        assert_eq!(idx.frecency_score("safari", now, 20).unwrap(), 100.0);
    }

    #[test]
    fn decay_buckets() {
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        let day = 86_400;
        idx.record_visit("a", now - 3 * day).unwrap();
        idx.record_visit("a", now - 10 * day).unwrap();
        idx.record_visit("a", now - 60 * day).unwrap();
        idx.record_visit("a", now - 120 * day).unwrap();
        assert_eq!(
            idx.frecency_score("a", now, 20).unwrap(),
            100.0 + 70.0 + 30.0 + 10.0
        );
    }

    #[test]
    fn respects_max_visits_cap() {
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        for i in 0..50 {
            idx.record_visit("a", now - i).unwrap();
        }
        assert_eq!(idx.frecency_score("a", now, 5).unwrap(), 500.0);
    }

    #[test]
    fn visits_table_caps_per_id_at_db_cap() {
        // REGRESSION (2026-04-29): every activation appended to the
        // `visits` table without any pruning. After heavy use the
        // SQLite store grew unbounded - disk + a slow LIMIT scan
        // on every frecency query. Verify each new row triggers a
        // DELETE that keeps only the newest VISIT_DB_CAP rows
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        // Insert 3x the cap. Each insert should purge older rows
        // beyond the cap so table never exceeds it
        for i in 0..(VISIT_DB_CAP * 3) {
            idx.record_visit("noisy", now - i as i64).unwrap();
        }
        let row_count: i64 = idx
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM visits WHERE candidate_id = ?1",
                params!["noisy"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            row_count as usize, VISIT_DB_CAP,
            "table caps at VISIT_DB_CAP per id; got {row_count}"
        );
    }

    #[test]
    #[ignore = "long-running soak - invoke explicitly via `cargo test --release -- --ignored`"]
    fn soak_visits_table_stays_bounded() {
        // Soak: 10K record_visit calls spread across 50 unique
        // candidate ids. The per-id cap (`VISIT_DB_CAP=256`) means
        // each id can have at most 256 rows on disk; total must
        // stay at unique_ids x cap regardless of insert volume.
        // Without the cap, this test's table would balloon to 10K
        // rows
        //
        // Lives here (gyors-index tests) rather than in gyors-ipc
        // because the IPC layer's BRIDGE OnceLock leaks state
        // between tests in same process - soaking 10K writes
        // against shared state collides with whatever the regular
        // test suite already inserted. A fresh `Index::in_memory`
        // here gives the soak a clean slate
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        const ACTIVATIONS: usize = 10_000;
        const UNIQUE_IDS: usize = 50;

        for i in 0..ACTIVATIONS {
            let id = format!("soak-id-{}", i % UNIQUE_IDS);
            // Ts must increase so ORDER BY ts DESC LIMIT N in
            // the prune query keeps a deterministic newest-N set.
            // Without monotonic ts, repeated inserts at same
            // timestamp tie-break randomly and test becomes
            // flaky
            idx.record_visit(&id, now + i as i64).unwrap();
        }

        let total = idx.visits_total_rows().unwrap();
        let max_per_id = idx.visits_max_rows_per_id().unwrap();
        let cap = VISIT_DB_CAP as i64;
        assert!(
            max_per_id <= cap,
            "visits per id exceeded cap: max={max_per_id}, cap={cap}"
        );
        assert!(
            total <= cap * UNIQUE_IDS as i64,
            "total visits exceeded {}×{} cap, got {total}",
            UNIQUE_IDS,
            cap
        );
        // 10K / 50 = 200 visits per id, which is under the 256
        // cap, so total is exactly 10K. Pin that to confirm the
        // prune isn't dropping rows we should have kept
        assert_eq!(
            total, ACTIVATIONS as i64,
            "expected {ACTIVATIONS} rows total - too few means \
             the cap is over-pruning, too many means it's under-pruning"
        );
    }

    #[test]
    #[ignore = "long-running soak - invoke explicitly via `cargo test --release -- --ignored`"]
    fn soak_visits_per_id_caps_exactly_at_db_cap() {
        // Counter-test to the ratio above: drive a SINGLE id past
        // the cap to verify the prune actually kicks in. Without
        // pruning this would yield 1000 rows; with pruning it
        // stays at exactly VISIT_DB_CAP
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        for i in 0..1_000 {
            idx.record_visit("hot-id", now + i as i64).unwrap();
        }
        let max_per_id = idx.visits_max_rows_per_id().unwrap();
        assert_eq!(
            max_per_id, VISIT_DB_CAP as i64,
            "single id should cap exactly at VISIT_DB_CAP after \
             repeated inserts; got {max_per_id}"
        );
        let total = idx.visits_total_rows().unwrap();
        assert_eq!(total, VISIT_DB_CAP as i64, "no other rows should leak in");
    }

    #[test]
    fn visits_table_cap_isolates_by_candidate_id() {
        // The DELETE keeps the newest N FOR THIS ID - must not
        // touch other ids. Bug guard against an over-broad DELETE
        // clause hitting unrelated rows
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        idx.record_visit("rare", now).unwrap();
        for i in 0..(VISIT_DB_CAP + 50) {
            idx.record_visit("noisy", now - i as i64).unwrap();
        }
        // `rare` had a single insert and shouldn't have been
        // disturbed when `noisy` got pruned
        let rare_count: i64 = idx
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM visits WHERE candidate_id = ?1",
                params!["rare"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rare_count, 1, "rare id row was wrongly pruned");
    }


    #[test]
    fn bulk_score_matches_single_score() {
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        idx.record_visit("a", now).unwrap();
        idx.record_visit("a", now - 100_000).unwrap();
        idx.record_visit("b", now - 3_000_000).unwrap();

        let bulk = idx.frecency_scores_bulk(&["a", "b", "c"], now, 20).unwrap();
        assert_eq!(
            bulk.get("a"),
            Some(&idx.frecency_score("a", now, 20).unwrap())
        );
        assert_eq!(
            bulk.get("b"),
            Some(&idx.frecency_score("b", now, 20).unwrap())
        );
        // Never-visited ids are absent, not zero - callers treat the
        // absence as 0
        assert!(!bulk.contains_key("c"));
    }

    #[test]
    fn bulk_score_empty_input_returns_empty() {
        let idx = Index::in_memory().unwrap();
        let out = idx.frecency_scores_bulk(&[], 0, 20).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn in_memory_cache_survives_reopen_via_sqlite() {
        // Visits persisted to SQLite must load back into the in-memory
        // map on next `Index::open(...)`. Critical for the "saved
        // on every app start" guarantee
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let now = 1_700_000_000;
        {
            let idx = Index::open(&path).unwrap();
            idx.record_visit("apps::Safari", now).unwrap();
            idx.record_visit("apps::Safari", now - 60).unwrap();
            idx.record_visit("note::foo", now - 10).unwrap();
        }
        // Reopen - cache rebuilt from SQLite
        let idx = Index::open(&path).unwrap();
        let out = idx
            .frecency_scores_bulk(&["apps::Safari", "note::foo", "unknown"], now, 20)
            .unwrap();
        assert!(out.get("apps::Safari").copied().unwrap_or(0.0) > 0.0);
        assert!(out.get("note::foo").copied().unwrap_or(0.0) > 0.0);
        assert!(!out.contains_key("unknown"));
    }

    #[test]
    fn record_visit_updates_cache_for_immediate_reads() {
        // The "no SQL on hot path" design requires record_visit to
        // update cache synchronously - otherwise a just-activated
        // candidate wouldn't bump its frecency until app restart
        let idx = Index::in_memory().unwrap();
        let now = 1_700_000_000;
        idx.record_visit("fresh", now).unwrap();
        let out = idx.frecency_scores_bulk(&["fresh"], now, 20).unwrap();
        assert!(out.get("fresh").copied().unwrap_or(0.0) > 0.0);
    }


    #[test]
    fn clipboard_recent_empty() {
        let idx = Index::in_memory().unwrap();
        assert!(idx.clipboard_recent(10).unwrap().is_empty());
        assert_eq!(idx.clipboard_count().unwrap(), 0);
    }

    #[test]
    fn clipboard_records_and_returns_recent() {
        let idx = Index::in_memory().unwrap();
        idx.record_clipboard("hello", 100).unwrap();
        idx.record_clipboard("world", 200).unwrap();
        let items = idx.clipboard_recent(10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].content, "world"); // newest first
        assert_eq!(items[1].content, "hello");
    }

    #[test]
    fn clipboard_isnt_interested_in_empty_strings() {
        let idx = Index::in_memory().unwrap();
        assert!(!idx.record_clipboard("", 1).unwrap());
        assert!(!idx.record_clipboard("   ", 2).unwrap());
        assert!(!idx.record_clipboard("\n\t", 3).unwrap());
        assert_eq!(idx.clipboard_count().unwrap(), 0);
    }

    #[test]
    fn clipboard_doesnt_record_echo() {
        let idx = Index::in_memory().unwrap();
        assert!(idx.record_clipboard("same", 1).unwrap());
        assert!(!idx.record_clipboard("same", 2).unwrap());
        assert!(!idx.record_clipboard("same", 3).unwrap());
        assert_eq!(idx.clipboard_count().unwrap(), 1);
    }

    #[test]
    fn clipboard_allows_duplicate_after_different() {
        let idx = Index::in_memory().unwrap();
        idx.record_clipboard("a", 1).unwrap();
        idx.record_clipboard("b", 2).unwrap();
        idx.record_clipboard("a", 3).unwrap();
        assert_eq!(idx.clipboard_count().unwrap(), 3);
    }

    #[test]
    fn clipboard_default_cap_is_full_local_history() {
        // Free-tier signed-out is NOT capped at 5 locally - the
        // default cap is same generous 500 the launcher always
        // shipped. Cap-at-5 only applies to what gets *synced*
        let idx = Index::in_memory().unwrap();
        assert_eq!(idx.clipboard_cap(), CLIPBOARD_RETENTION_DEFAULT);
        // Smoke-test: insert plenty more than 5; everything sticks
        for i in 0..50 {
            idx.record_clipboard(&format!("item-{i}"), i + 1).unwrap();
        }
        assert_eq!(idx.clipboard_count().unwrap(), 50);
    }

    #[test]
    fn user_can_dial_cap_below_default() {
        // The `clipboard_max_items` config field hooks in here. Free
        // *or* paid, user-chosen value wins
        let idx = Index::in_memory().unwrap();
        idx.set_clipboard_cap(3).unwrap();
        for i in 0..10 {
            idx.record_clipboard(&format!("item-{i}"), i + 1).unwrap();
        }
        assert_eq!(idx.clipboard_count().unwrap(), 3);
    }

    #[test]
    fn shrinking_cap_immediately_prunes() {
        let idx = Index::in_memory().unwrap();
        for i in 0..20 {
            idx.record_clipboard(&format!("item-{i}"), i + 1).unwrap();
        }
        assert_eq!(idx.clipboard_count().unwrap(), 20);
        idx.set_clipboard_cap(5).unwrap();
        assert_eq!(idx.clipboard_count().unwrap(), 5);
    }

    #[test]
    fn clipboard_search_matches_by_substring() {
        let idx = Index::in_memory().unwrap();
        idx.record_clipboard("hello world", 1).unwrap();
        idx.record_clipboard("foo bar baz", 2).unwrap();
        idx.record_clipboard("say hello", 3).unwrap();

        let results = idx.clipboard_search("hello", 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content, "say hello"); // newest first
        assert_eq!(results[1].content, "hello world");
    }

    #[test]
    fn clipboard_search_respects_limit() {
        let idx = Index::in_memory().unwrap();
        for i in 0..20 {
            idx.record_clipboard(&format!("foo-{i}"), i).unwrap();
        }
        let results = idx.clipboard_search("foo", 5).unwrap();
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn clipboard_search_doesnt_let_sql_wildcards_sneak_in() {
        let idx = Index::in_memory().unwrap();
        idx.record_clipboard("percent%sign", 1).unwrap();
        idx.record_clipboard("under_score", 2).unwrap();
        idx.record_clipboard("plain text", 3).unwrap();

        assert_eq!(idx.clipboard_search("%", 10).unwrap().len(), 1);
        assert_eq!(idx.clipboard_search("_", 10).unwrap().len(), 1);
    }

    #[test]
    fn clipboard_get_returns_item() {
        let idx = Index::in_memory().unwrap();
        idx.record_clipboard("hello", 1).unwrap();
        let items = idx.clipboard_recent(1).unwrap();
        let id = items[0].id;
        let fetched = idx.clipboard_get(id).unwrap().unwrap();
        assert_eq!(fetched.content, "hello");
        assert_eq!(fetched.ts, 1);
    }

    #[test]
    fn clipboard_get_missing_is_none() {
        let idx = Index::in_memory().unwrap();
        assert!(idx.clipboard_get(999).unwrap().is_none());
    }

    #[test]
    fn clipboard_recent_respects_limit() {
        let idx = Index::in_memory().unwrap();
        for i in 0..20 {
            idx.record_clipboard(&format!("item-{i}"), i).unwrap();
        }
        assert_eq!(idx.clipboard_recent(5).unwrap().len(), 5);
    }

    #[test]
    fn clear_clipboard_history_empties_table() {
        let idx = Index::in_memory().unwrap();
        idx.record_clipboard("a", 1).unwrap();
        idx.record_clipboard("b", 2).unwrap();
        let deleted = idx.clear_clipboard_history().unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(idx.clipboard_count().unwrap(), 0);
    }

    #[test]
    fn clearing_empty_clipboard_non_event() {
        let idx = Index::in_memory().unwrap();
        let deleted = idx.clear_clipboard_history().unwrap();
        assert_eq!(deleted, 0);
    }


    #[test]
    fn query_history_starts_empty() {
        let idx = Index::in_memory().unwrap();
        assert!(idx.recent_queries(10).unwrap().is_empty());
        assert!(idx.last_query().unwrap().is_none());
        assert_eq!(idx.query_history_count().unwrap(), 0);
    }

    #[test]
    fn record_query_keeps_order_newest_first() {
        let idx = Index::in_memory().unwrap();
        idx.record_query("safari", 1).unwrap();
        idx.record_query("note foo", 2).unwrap();
        idx.record_query("calc 2+2", 3).unwrap();
        let recent = idx.recent_queries(10).unwrap();
        assert_eq!(recent, vec!["calc 2+2", "note foo", "safari"]);
        assert_eq!(idx.last_query().unwrap().as_deref(), Some("calc 2+2"));
    }

    #[test]
    fn query_history_skips_blank() {
        let idx = Index::in_memory().unwrap();
        assert!(!idx.record_query("", 1).unwrap());
        assert!(!idx.record_query("   ", 2).unwrap());
        assert!(!idx.record_query("\n\t", 3).unwrap());
        assert_eq!(idx.query_history_count().unwrap(), 0);
    }

    #[test]
    fn query_history_skips_back_to_back_echo() {
        let idx = Index::in_memory().unwrap();
        assert!(idx.record_query("same", 1).unwrap());
        assert!(!idx.record_query("same", 2).unwrap());
        assert!(!idx.record_query(" same ", 3).unwrap()); // trim-aware
        assert!(idx.record_query("different", 4).unwrap());
        assert!(idx.record_query("same", 5).unwrap()); // allowed after break
        assert_eq!(idx.query_history_count().unwrap(), 3);
    }

    #[test]
    fn recent_queries_dedupes_older_entries() {
        let idx = Index::in_memory().unwrap();
        idx.record_query("a", 1).unwrap();
        idx.record_query("b", 2).unwrap();
        idx.record_query("a", 3).unwrap(); // new "a" after "b"
        idx.record_query("c", 4).unwrap();
        // All three unique patterns present, `a` surfaces at its latest slot
        let recent = idx.recent_queries(10).unwrap();
        assert_eq!(recent, vec!["c", "a", "b"]);
    }

    #[test]
    fn recent_queries_respects_limit() {
        let idx = Index::in_memory().unwrap();
        for i in 0..20 {
            idx.record_query(&format!("q{i}"), i).unwrap();
        }
        assert_eq!(idx.recent_queries(5).unwrap().len(), 5);
    }

    #[test]
    fn recent_queries_limit_zero_is_quick_no_op() {
        let idx = Index::in_memory().unwrap();
        idx.record_query("anything", 1).unwrap();
        assert!(idx.recent_queries(0).unwrap().is_empty());
    }

    #[test]
    fn query_history_prunes_to_retention() {
        let idx = Index::in_memory().unwrap();
        for i in 0..(QUERY_HISTORY_RETENTION as i64 + 10) {
            idx.record_query(&format!("q-{i}"), i + 1).unwrap();
        }
        assert_eq!(idx.query_history_count().unwrap(), QUERY_HISTORY_RETENTION);
    }

    #[test]
    fn clear_query_history_wipes_rows() {
        let idx = Index::in_memory().unwrap();
        idx.record_query("a", 1).unwrap();
        idx.record_query("b", 2).unwrap();
        assert_eq!(idx.clear_query_history().unwrap(), 2);
        assert!(idx.last_query().unwrap().is_none());
    }

    #[test]
    fn query_history_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        {
            let idx = Index::open(&path).unwrap();
            idx.record_query("remember me", 1).unwrap();
        }
        let idx = Index::open(&path).unwrap();
        assert_eq!(
            idx.last_query().unwrap().as_deref(),
            Some("remember me"),
        );
    }
}
