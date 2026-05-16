//! [`SyncResource`] for `clipboard_history`
//!
//! ## Id derivation
//!
//! Local clipboard rows have an autoincrement id, but on wire we
//! key by `blake3(content || ts)[..16]` so two devices copying the
//! same bytes generate same id. That makes the
//! [`ConflictPolicy::Merge`](crate::proto::ConflictPolicy::Merge)
//! policy correct by construction: an id collision means the content
//! is identical, so accepting either side's row is fine
//!
//! ## Payload shape
//!
//! Today: JSON `{ "content": "...", "ts": <epoch_s> }`. JSON because
//! it survives schema additions cheaply (server doesn't see it; only
//! the client serializes/deserializes). When images land, we'll add
//! `kind: "text" | "image"` and a `blob_ref` for image bytes that
//! live in object storage; the launcher can ignore unknown kinds

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::proto::Namespace;
use crate::resource::{LocalChange, RemoteChange, SyncOp, SyncResource};
use crate::SharedIndex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardPayload {
    pub content: String,
    pub ts: i64,
}

pub struct ClipboardResource {
    index: SharedIndex,
}

impl ClipboardResource {
    pub fn new(index: SharedIndex) -> Self {
        Self { index }
    }

    /// Build a [`LocalChange`] for a freshly-recorded clipboard row.
    /// The launcher's pasteboard watcher calls this then enqueues
    /// result via the engine
    pub fn change_for_upsert(content: &str, ts: i64) -> Result<LocalChange> {
        let id = sync_id(content, ts);
        let payload = serde_json::to_vec(&ClipboardPayload {
            content: content.to_string(),
            ts,
        })
        .context("serialize clipboard payload")?;
        Ok(LocalChange {
            id,
            op: SyncOp::Upsert,
            payload,
            ts_local: ts,
        })
    }

    /// Tombstone a row by its content-addressed id. Used when the
    /// user explicitly deletes a clipboard entry from the launcher
    pub fn change_for_delete(content: &str, ts: i64) -> LocalChange {
        LocalChange {
            id: sync_id(content, ts),
            op: SyncOp::Delete,
            payload: Vec::new(),
            ts_local: ts,
        }
    }
}

impl SyncResource for ClipboardResource {
    fn namespace(&self) -> Namespace {
        Namespace::ClipboardHistory
    }

    fn apply(&self, change: &RemoteChange) -> Result<()> {
        match change.op {
            SyncOp::Upsert => {
                let payload: ClipboardPayload =
                    serde_json::from_slice(&change.payload).context("decode clipboard payload")?;
                // Index dedupes by content+ts in `record_clipboard`,
                // so calling it with the remote bytes is idempotent -
                // a duplicate just no-ops at the SQL layer
                self.index
                    .record_clipboard(&payload.content, payload.ts)
                    .context("apply remote clipboard upsert")?;
            }
            SyncOp::Delete => {
                // The server's tombstone carries the content-addressed
                // sync_id, not the local autoincrement id. The index
                // populates `clipboard_items.sync_id` on every insert
                // (and backfills existing rows on `open`), so the
                // lookup is direct
                //
                // Idempotent at the SQL layer - a duplicate tombstone
                // returns `false` for "row not found" rather than
                // erroring, so engine doesn't dead-letter a
                // tombstone whose effect was already realised
                let removed = self
                    .index
                    .delete_clipboard_by_sync_id(&change.id)
                    .context("apply remote clipboard delete")?;
                if !removed {
                    tracing::debug!(
                        sync_id = %change.id,
                        "clipboard delete: no local row (already gone or never seen)"
                    );
                }
            }
        }
        Ok(())
    }

    fn validate_payload(&self, payload: &[u8]) -> Result<Vec<u8>> {
        // Round-trip through typed shape so a corrupt outbox row
        // fails before we ship it. JSON re-serialization can change
        // key ordering, but the server treats bytes as opaque so
        // that's fine
        let parsed: ClipboardPayload =
            serde_json::from_slice(payload).context("validate clipboard payload")?;
        Ok(serde_json::to_vec(&parsed)?)
    }
}

/// Stable id for a (content, ts) pair. Delegates to
/// `gyors_index::clipboard_sync_id` - the formula lives there because
/// index needs to populate the column on insert without taking a
/// dep on this crate. Keeping a single source of truth means the
/// `apply(Delete)` lookup can never silently mismatch a row that was
/// recorded with a different hash
pub fn sync_id(content: &str, ts: i64) -> String {
    gyors_index::clipboard_sync_id(content, ts)
}

/// Convenience: enqueue a clipboard upsert directly to the outbox.
/// Used by callers that dont yet have a `SyncEngine` wired up - the
/// outbox row outlives the absence of an engine, so writes dont
/// silently drop while sync is being plumbed in
pub fn enqueue_upsert(index: &Arc<gyors_index::Index>, content: &str, ts: i64) -> Result<()> {
    let change = ClipboardResource::change_for_upsert(content, ts)?;
    index
        .outbox_enqueue(
            Namespace::ClipboardHistory.as_str(),
            &change.id,
            change.op.as_str(),
            &change.payload,
            change.ts_local,
        )
        .context("enqueue clipboard upsert")?;
    Ok(())
}
