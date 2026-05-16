//! [`SyncEngine`] - the loop that drains the outbox, encrypts items,
//! pushes them, pulls deltas, and applies them through the right
//! [`SyncResource`]
//!
//! Phase A scope: a `tick()` method that does one push + one pull
//! pass. The launcher will eventually drive `tick` from a tokio task
//! on a 60s cadence (or wake-on-outbox-non-empty); for now only
//! caller is tests

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;

use crate::crypto::Crypto;
use crate::proto::{
    AccountTier, ConflictPolicy, Namespace, PushItem, PushRequest, PushStatus,
};
use crate::resource::{RemoteChange, SyncOp, SyncResource};
use crate::transport::Transport;
use crate::SharedIndex;

/// Hard caps so a runaway local outbox doesn't trip the server's
/// per-request limits (`MAX_PUSH_ITEMS = 200` in `gyors-cloud`)
const PUSH_BATCH: usize = 100;

/// Per-row retry budget before we dead-letter. 12 attempts with
/// quadratic backoff covers ~10 hours of outage (1 + 4 + 9 + 16 + ...
/// Minutes, capped at 60 each) before giving up - a one-day flaky
/// network heals long before this, but a permanently-broken row
/// (corrupt payload, server-side bug) gets out of the queue
const MAX_OUTBOX_ATTEMPTS: i64 = 12;

#[derive(Debug, Clone)]
pub struct SyncEngineConfig {
    pub tier: AccountTier,
}

pub struct SyncEngine {
    index: SharedIndex,
    crypto: Arc<dyn Crypto>,
    transport: Arc<dyn Transport>,
    resources: HashMap<Namespace, Box<dyn SyncResource>>,
    #[allow(dead_code)]
    config: SyncEngineConfig,
}

impl SyncEngine {
    pub fn new(
        index: SharedIndex,
        crypto: Arc<dyn Crypto>,
        transport: Arc<dyn Transport>,
        config: SyncEngineConfig,
    ) -> Self {
        Self {
            index,
            crypto,
            transport,
            resources: HashMap::new(),
            config,
        }
    }

    /// Wire up a per-namespace impl. Engine ignores namespaces with
    /// no registered resource - this is how we keep the server's
    /// fixed namespace list extensible without forcing the client to
    /// support all of them at once
    pub fn register(&mut self, resource: Box<dyn SyncResource>) {
        self.resources.insert(resource.namespace(), resource);
    }

    /// One push + one pull pass. The launcher's background task
    /// calls this on a cadence; tests call it directly
    ///
    /// Two specific transport failures are surfaced via flags rather
    /// than as `Err`:
    /// - `Unauthenticated` (401): the bearer is dead. Swift UI
    ///   needs to prompt re-signin AND the background loop must
    ///   stop spamming. Both happen off `session_expired` in the
    ///   FFI layer.
    /// - `QuotaExceeded` (507): not actionable by retry; show the
    ///   "storage full" banner. Outbox stays put so freeing bytes
    ///   resumes from where we left off
    ///
    /// All other transport errors propagate as `Err` so caller
    /// can log + back off
    pub async fn tick(&self) -> Result<TickReport> {
        let mut report = TickReport::default();
        match self.push_pass().await {
            Ok(n) => report.pushed = n,
            Err(e) => {
                if classify_into_report(&e, &mut report) {
                    return Ok(report);
                }
                return Err(e);
            }
        }
        match self.pull_pass().await {
            Ok(n) => report.pulled = n,
            Err(e) => {
                if classify_into_report(&e, &mut report) {
                    return Ok(report);
                }
                return Err(e);
            }
        }
        Ok(report)
    }

    /// Drain the outbox, encrypt, and push in one batch
    ///
    /// Tier policy is server-enforced - the client pushes everything
    /// it has queued, regardless of tier. The server is responsible
    /// for accepting / dropping / capping per user's plan. This
    /// matters for two reasons: a tampered launcher can't bypass
    /// the cap, and we can retune tier sizes without shipping a new
    /// build
    async fn push_pass(&self) -> Result<usize> {
        let drained = self
            .index
            .outbox_drain(None, PUSH_BATCH)
            .context("drain outbox")?;
        if drained.is_empty() {
            return Ok(0);
        }

        let mut items: Vec<PushItem> = Vec::with_capacity(drained.len());
        let mut id_to_outbox: Vec<(i64, Namespace, String)> = Vec::with_capacity(drained.len());
        let mut unknown_ns_acks: Vec<i64> = Vec::new();
        for entry in &drained {
            let Some(ns) = Namespace::from_str(&entry.resource_kind) else {
                // Outbox row for a namespace we dont even know
                // about (e.g. left over from an older build). Drop
                // it so it doesn't replay forever
                unknown_ns_acks.push(entry.id);
                continue;
            };
            let Some(resource) = self.resources.get(&ns) else {
                continue;
            };

            let payload = resource
                .validate_payload(&entry.payload)
                .with_context(|| format!("validate {}/{}", ns.as_str(), entry.resource_id))?;

            let next_version = self
                .index
                .sync_version_get(ns.as_str(), &entry.resource_id)?
                .unwrap_or(0)
                + 1;

            // AAD binds the ciphertext to its (namespace, id, version)
            // - a server that re-routes blobs under different ids
            // can't trick a client into decrypting the wrong row
            let aad = aad_for(ns, &entry.resource_id, next_version);
            let ciphertext = self
                .crypto
                .seal(&aad, &payload)
                .context("seal payload")?;

            let deleted = entry.op == SyncOp::Delete.as_str();
            items.push(PushItem {
                id: entry.resource_id.clone(),
                namespace: ns,
                version: next_version,
                ciphertext,
                deleted: if deleted { Some(true) } else { None },
            });
            id_to_outbox.push((entry.id, ns, entry.resource_id.clone()));
        }

        for id in unknown_ns_acks {
            self.index.outbox_ack(id)?;
        }
        if items.is_empty() {
            return Ok(0);
        }

        let request_count = items.len();
        let response = self
            .transport
            .push(PushRequest { items })
            .await
            .context("push to transport")?;

        // Map outcomes back to outbox rows by (namespace, id). The
        // server returns one outcome per accepted item plus one per
        // rejected/conflicted item
        let mut outcome_lookup: HashMap<(Namespace, String), &crate::proto::PushOutcome> =
            HashMap::with_capacity(response.outcomes.len());
        for outcome in &response.outcomes {
            outcome_lookup.insert((outcome.namespace, outcome.id.clone()), outcome);
        }

        for (outbox_id, ns, item_id) in id_to_outbox {
            let outcome = outcome_lookup.get(&(ns, item_id.clone()));
            match outcome.map(|o| o.status) {
                Some(PushStatus::Accepted) => {
                    let v = outcome.and_then(|o| o.server_version).unwrap_or(0);
                    if v > 0 {
                        self.index.sync_version_set(ns.as_str(), &item_id, v)?;
                    }
                    self.index.outbox_ack(outbox_id)?;
                }
                Some(PushStatus::Conflict) => {
                    // Server says someone else's version is newer.
                    // Adopt their version; whether to retry our local
                    // change depends on the namespace's policy
                    if let Some(server_v) = outcome.and_then(|o| o.server_version) {
                        self.index
                            .sync_version_set(ns.as_str(), &item_id, server_v)?;
                    }
                    match ns.conflict_policy() {
                        ConflictPolicy::LastWriteWins => {
                            // Drop our change - the server's row wins
                            // and we'll learn its bytes on next pull
                            self.index.outbox_ack(outbox_id)?;
                        }
                        ConflictPolicy::Merge => {
                            // Same id from two writers can only happen
                            // for content-addressable rows, where same
                            // id == same content. Adopting the server's
                            // version is enough; drop our duplicate
                            self.index.outbox_ack(outbox_id)?;
                        }
                    }
                }
                Some(PushStatus::Rejected) => {
                    let reason = outcome
                        .and_then(|o| o.reason.clone())
                        .unwrap_or_else(|| "rejected".to_string());
                    // Quota rejections are user-fixable (delete blobs
                    // / upgrade tier) - keep retrying with backoff.
                    // Everything else is "the server actively said no"
                    // (bad shape, unknown namespace, ...) - those won't
                    // get better, so dead-letter immediately rather
                    // than waste retries
                    if reason == "quota_exceeded" {
                        self.index.outbox_fail(outbox_id, &reason)?;
                    } else {
                        self.index.outbox_abandon(outbox_id, &reason)?;
                    }
                }
                None => {
                    self.index
                        .outbox_fail(outbox_id, "missing outcome from server")?;
                }
            }
        }

        // After every push pass, dead-letter rows that have hit the
        // per-row retry cap. `outbox_fail` keeps incrementing
        // `attempts` forever otherwise - which means a permanently-
        // failing row burns one outbox slot + one HTTP call per
        // tick forever. The dead-letter row stays in-table so the
        // sync panel can surface "N items couldn't sync" + the
        // last error
        self.abandon_exhausted()?;

        Ok(request_count)
    }

    /// Walk the just-drained rows; any with `attempts >= MAX_OUTBOX_ATTEMPTS`
    /// gets dead-lettered. We scan after every push so a flaky row
    /// gets marked the moment it exceeds budget rather than waiting
    /// for next drain
    fn abandon_exhausted(&self) -> Result<()> {
        // Cheaper than re-querying: peek at next drain page and
        // pick out the ones over budget. The drain itself filters
        // them next tick, but we want them gone NOW so they dont
        // count toward `outbox_pending`
        let candidates = self
            .index
            .outbox_drain(None, PUSH_BATCH)
            .context("drain for abandon check")?;
        for entry in &candidates {
            if entry.attempts >= MAX_OUTBOX_ATTEMPTS {
                self.index.outbox_abandon(
                    entry.id,
                    "exhausted retries (>= MAX_OUTBOX_ATTEMPTS)",
                )?;
            }
        }
        Ok(())
    }

    /// Pull deltas the server has for us, decrypt, dispatch to
    /// resource impls. Loops while the server reports `has_more` so
    /// a fresh device with N>page_size server items catches up in
    /// one tick instead of N/page_size ticks. Cursor is opaque -
    /// we round-trip it without inspecting
    ///
    /// Bounded by `MAX_PULL_ITEMS_PER_TICK` so a runaway server (or
    /// a buggy cursor that never advances) can't loop forever. The
    /// cursor is persisted on every page so a partial tick still
    /// makes forward progress on next call
    async fn pull_pass(&self) -> Result<usize> {
        let mut applied = 0usize;
        let mut pages = 0usize;
        let mut cursor = self.index.sync_pull_cursor_get()?;

        loop {
            let response = self
                .transport
                .pull(cursor.as_deref())
                .await
                .context("pull from transport")?;

            for item in &response.items {
                let Some(resource) = self.resources.get(&item.namespace) else {
                    continue;
                };

                let aad = aad_for(item.namespace, &item.id, item.version);
                let plaintext = match self.crypto.open(&aad, &item.ciphertext) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            namespace = %item.namespace.as_str(),
                            id = %item.id,
                            "sync pull: decrypt failed: {e}"
                        );
                        continue;
                    }
                };

                let change = RemoteChange {
                    id: item.id.clone(),
                    op: if item.deleted {
                        SyncOp::Delete
                    } else {
                        SyncOp::Upsert
                    },
                    version: item.version,
                    updated_at: item.updated_at.clone(),
                    payload: plaintext,
                };
                if let Err(e) = resource.apply(&change) {
                    tracing::warn!(
                        namespace = %item.namespace.as_str(),
                        id = %item.id,
                        "sync apply failed: {e}"
                    );
                    continue;
                }
                self.index
                    .sync_version_set(item.namespace.as_str(), &item.id, item.version)?;
                applied += 1;
            }

            // Persist cursor on every page so a crash mid-loop
            // doesn't replay everything we already applied
            self.index.sync_pull_cursor_set(&response.cursor)?;
            cursor = Some(response.cursor);
            pages += 1;

            if !response.has_more {
                break;
            }
            if applied >= MAX_PULL_ITEMS_PER_TICK {
                tracing::info!(
                    "sync pull: hit per-tick cap ({applied} items, {pages} pages); \
                     remainder picks up next tick"
                );
                break;
            }
            if pages >= MAX_PULL_PAGES_PER_TICK {
                // If the server reports has_more forever
                // (e.g. cursor regression bug), dont spin
                tracing::warn!(
                    "sync pull: hit page cap ({pages} pages, {applied} items); \
                     bailing - server may be advertising has_more without advancing"
                );
                break;
            }
        }

        Ok(applied)
    }
}

/// Soft cap on how many items a single tick will pull. Sized to
/// catch up a long-offline device in a couple of ticks without
/// monopolising runtime - 5k clipboard items at ~200 B each
/// is ~1 MB of decrypt + insert work, fits comfortably in <1s
const MAX_PULL_ITEMS_PER_TICK: usize = 5000;

/// Hard cap on pages-per-tick. Default server page is 500;
/// `5000 / 500 = 10` so this cap is mostly belt-and-braces against a
/// pathological server that keeps emitting empty `has_more: true`
const MAX_PULL_PAGES_PER_TICK: usize = 32;

/// Map a transport error into the TickReport's flag-shaped surface,
/// returning `true` if error was "soft" enough to swallow (we
/// reported it via the flag) or `false` if caller should keep
/// propagating it as `Err`
///
/// Error type matters here, but error chains can wrap the
/// transport error a few layers deep (we go through `anyhow::Context`).
/// We use `downcast_ref` over the chain so classification works
/// regardless of how many `.context()` layers call site added
fn classify_into_report(err: &anyhow::Error, report: &mut TickReport) -> bool {
    for cause in err.chain() {
        if let Some(t) = cause.downcast_ref::<crate::transport::TransportError>() {
            match t {
                crate::transport::TransportError::Unauthenticated
                | crate::transport::TransportError::SessionExpired => {
                    report.session_expired = true;
                    return true;
                }
                crate::transport::TransportError::QuotaExceeded => {
                    report.quota_exceeded = true;
                    return true;
                }
                _ => return false,
            }
        }
    }
    false
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TickReport {
    pub pushed: usize,
    pub pulled: usize,
    /// Server rejected the bearer (401). The FFI layer surfaces this
    /// to Swift so user gets a "session expired - sign in again"
    /// prompt + the background tick auto-pauses until they do
    pub session_expired: bool,
    /// Server reported insufficient storage (507). User sees a
    /// "sync paused, storage full" banner; the outbox stays put so
    /// freeing bytes (or upgrading) resumes from where we left off
    pub quota_exceeded: bool,
}

/// Authenticated-data binding for a payload. `(namespace, id, version)`
/// uniquely identifies the slot the ciphertext belongs to; if the
/// server (or an attacker) swaps blobs across slots, the AES-GCM auth
/// tag fails and we skip row instead of decrypting nonsense
fn aad_for(ns: Namespace, id: &str, version: i64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(ns.as_str().as_bytes());
    buf.push(b'|');
    buf.extend_from_slice(id.as_bytes());
    buf.push(b'|');
    buf.extend_from_slice(version.to_string().as_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{ClipboardResource, enqueue_upsert};
    use crate::crypto::NullCrypto;
    use crate::proto::{PullItem, PullResponse};
    use crate::transport::NullTransport;
    use std::sync::Arc;

    fn open_index() -> SharedIndex {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idx.db");
        let idx = gyors_index::Index::open(&path).unwrap();
        // Leak the tempdir so file outlives test; tests are
        // short-lived and the OS reaps the tmp tree on shutdown
        std::mem::forget(dir);
        Arc::new(idx)
    }

    fn engine(index: SharedIndex, transport: Arc<NullTransport>) -> SyncEngine {
        let mut e = SyncEngine::new(
            index.clone(),
            Arc::new(NullCrypto),
            transport,
            SyncEngineConfig {
                tier: AccountTier::Plus,
            },
        );
        e.register(Box::new(ClipboardResource::new(index)));
        e
    }

    #[tokio::test]
    async fn push_drains_outbox_and_records_version() {
        let index = open_index();
        enqueue_upsert(&index, "hello", 1234).unwrap();
        let transport = Arc::new(NullTransport::new());
        let e = engine(index.clone(), transport.clone());

        let report = e.tick().await.unwrap();
        assert_eq!(report.pushed, 1);

        // Outbox is now empty (the null transport accepts everything)
        assert_eq!(
            index.outbox_pending(Namespace::ClipboardHistory.as_str()).unwrap(),
            0
        );

        // Version was recorded for the synthetic id
        let pushed = transport.pushed();
        assert_eq!(pushed.len(), 1);
        let item = &pushed[0].items[0];
        assert_eq!(item.namespace, Namespace::ClipboardHistory);
        assert_eq!(item.version, 1);
        let stored = index
            .sync_version_get(Namespace::ClipboardHistory.as_str(), &item.id)
            .unwrap();
        assert_eq!(stored, Some(1));
    }

    fn engine_for_tier(
        index: SharedIndex,
        transport: Arc<NullTransport>,
        tier: AccountTier,
    ) -> SyncEngine {
        let mut e = SyncEngine::new(
            index.clone(),
            Arc::new(NullCrypto),
            transport,
            SyncEngineConfig { tier },
        );
        e.register(Box::new(ClipboardResource::new(index)));
        e
    }

    /// Cap-at-N is a server-side policy. The client must push
    /// everything it has, regardless of tier, so server has the
    /// data it needs to enforce. These tests pin that - if anyone
    /// reintroduces client-side gating, these break loudly
    #[tokio::test]
    async fn client_pushes_everything_regardless_of_tier() {
        for tier in [AccountTier::Free, AccountTier::Plus, AccountTier::Pro] {
            let index = open_index();
            for i in 0..8 {
                enqueue_upsert(&index, &format!("item-{i}-{tier:?}"), (i + 1) as i64).unwrap();
            }
            let transport = Arc::new(NullTransport::new());
            let e = engine_for_tier(index.clone(), transport.clone(), tier);

            let report = e.tick().await.unwrap();
            assert_eq!(
                report.pushed, 8,
                "tier {tier:?} must push all 8 - cap policy lives on the server"
            );
            assert_eq!(transport.pushed()[0].items.len(), 8);
            assert_eq!(
                index.outbox_pending(Namespace::ClipboardHistory.as_str()).unwrap(),
                0,
                "tier {tier:?} should fully drain the outbox after a push tick"
            );
        }
    }

    #[tokio::test]
    async fn client_does_not_filter_namespaces_by_tier() {
        // Free-tier clipboard settings push too - server decides
        // what to keep. We register only ClipboardResource here, so
        // a settings outbox row goes through the "no registered
        // resource" path (still doesn't ship - that's a separate
        // skip, not tier gating)
        let index = open_index();
        enqueue_upsert(&index, "clip-content", 100).unwrap();
        let transport = Arc::new(NullTransport::new());
        let e = engine_for_tier(index.clone(), transport.clone(), AccountTier::Free);
        let report = e.tick().await.unwrap();
        assert_eq!(report.pushed, 1);
        // Confirm wire payload was actually a clipboard item,
        // not a settings one - separates "tier said no" (no longer
        // a thing) from "no resource registered" (still a thing)
        assert_eq!(
            transport.pushed()[0].items[0].namespace,
            Namespace::ClipboardHistory
        );
    }

    /// A row whose `resource_kind` doesn't decode to any known
    /// `Namespace` (e.g. a leftover from a previous build that
    /// supported more kinds) should be acked-and-forgotten rather
    /// than replaying on every tick
    #[tokio::test]
    async fn unknown_namespace_outbox_row_is_dropped() {
        let index = open_index();
        index
            .outbox_enqueue("brand_new_kind_we_dont_know", "id-1", "upsert", b"{}", 1)
            .unwrap();
        let transport = Arc::new(NullTransport::new());
        let e = engine_for_tier(index.clone(), transport.clone(), AccountTier::Plus);
        let report = e.tick().await.unwrap();
        assert_eq!(report.pushed, 0);
        assert_eq!(transport.pushed().len(), 0);
        let pending: i64 = index
            .outbox_pending("brand_new_kind_we_dont_know")
            .unwrap();
        assert_eq!(pending, 0, "unknown-ns rows must be acked, not retried");
    }

    #[tokio::test]
    async fn pull_applies_remote_clipboard_and_records_cursor() {
        let index = open_index();
        let transport = Arc::new(NullTransport::new());
        let crypto = NullCrypto;
        let id = crate::clipboard::sync_id("from-other-device", 9999);
        let aad = aad_for(Namespace::ClipboardHistory, &id, 1);
        let payload = serde_json::to_vec(&crate::clipboard::ClipboardPayload {
            content: "from-other-device".to_string(),
            ts: 9999,
        })
        .unwrap();
        let ciphertext = crypto.seal(&aad, &payload).unwrap();
        transport.queue_pull(PullResponse {
            items: vec![PullItem {
                id: id.clone(),
                namespace: Namespace::ClipboardHistory,
                version: 1,
                ciphertext,
                deleted: false,
                updated_at: "2026-05-07T10:00:00Z".to_string(),
            }],
            cursor: "v1:42".to_string(),
            has_more: false,
        });

        let e = engine(index.clone(), transport);
        e.tick().await.unwrap();

        let cursor = index.sync_pull_cursor_get().unwrap();
        assert_eq!(cursor.as_deref(), Some("v1:42"));
        let items = index.clipboard_recent(10).unwrap();
        assert!(
            items.iter().any(|c| c.content == "from-other-device"),
            "remote clipboard upsert did not land in local index"
        );
    }

    /// REGRESSION: server-side clipboard deletes used to silently
    /// no-op on the local index because the apply(Delete) path
    /// couldn't map the content-addressed sync_id to a local row.
    /// Pins wire-up between `record_clipboard` populating
    /// `sync_id` and `apply(Delete)` looking it up
    #[tokio::test]
    async fn pull_applies_remote_clipboard_delete_tombstone() {
        let index = open_index();
        // Plant a local row that the tombstone will target. The
        // `record_clipboard` call computes + stores same sync_id
        // the server-side tombstone will reference
        let content = "to-be-deleted";
        let ts = 12345;
        index.record_clipboard(content, ts).unwrap();
        let count_before = index.clipboard_count().unwrap();
        assert!(count_before >= 1);
        let id = crate::clipboard::sync_id(content, ts);

        // Server-side delete arrives via pull. `ciphertext` is empty
        // for a tombstone but the engine still calls `crypto.open`
        // with the appropriate AAD; NullCrypto base64-roundtrips, so
        // an empty-base64 payload decodes to an empty Vec which the
        // resource's Delete branch ignores
        let transport = Arc::new(NullTransport::new());
        let aad = aad_for(Namespace::ClipboardHistory, &id, 2);
        let ciphertext = NullCrypto.seal(&aad, &[]).unwrap();
        transport.queue_pull(PullResponse {
            items: vec![PullItem {
                id: id.clone(),
                namespace: Namespace::ClipboardHistory,
                version: 2,
                ciphertext,
                deleted: true,
                updated_at: "2026-05-13T10:00:00Z".to_string(),
            }],
            cursor: "v1:99".to_string(),
            has_more: false,
        });

        let e = engine(index.clone(), transport);
        e.tick().await.unwrap();

        let remaining: Vec<_> = index
            .clipboard_recent(10)
            .unwrap()
            .into_iter()
            .filter(|c| c.content == content)
            .collect();
        assert!(
            remaining.is_empty(),
            "remote tombstone should have evicted the local row, still found: {remaining:?}"
        );
    }

    /// Tombstone for a sync_id the local index never had (or already
    /// pruned via retention) must NOT error - the engine retries
    /// failed applies, so a hard error on an unknown id would
    /// dead-letter a tombstone whose effect is already realised
    #[tokio::test]
    async fn pull_applies_remote_delete_for_unknown_id_is_noop() {
        let index = open_index();
        let transport = Arc::new(NullTransport::new());
        let id = crate::clipboard::sync_id("never-seen", 5);
        let aad = aad_for(Namespace::ClipboardHistory, &id, 1);
        let ciphertext = NullCrypto.seal(&aad, &[]).unwrap();
        transport.queue_pull(PullResponse {
            items: vec![PullItem {
                id: id.clone(),
                namespace: Namespace::ClipboardHistory,
                version: 1,
                ciphertext,
                deleted: true,
                updated_at: "2026-05-13T10:00:00Z".to_string(),
            }],
            cursor: "v1:1".to_string(),
            has_more: false,
        });

        let e = engine(index.clone(), transport);
        // Doesn't panic, doesn't error
        e.tick().await.unwrap();
    }
}
