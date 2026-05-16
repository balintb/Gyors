//! [`SyncResource`] - per-namespace plug-in. Each namespace in the
//! [`Namespace`](crate::proto::Namespace) enum gets one impl. The
//! engine handles encryption + transport + version bookkeeping; the
//! resource owns serialization and the apply-to-local path
//!
//! Object-safe by design - no associated types, no generics. The
//! engine stores `Box<dyn SyncResource>` keyed by `Namespace` and
//! dispatches via a hash-map lookup

use anyhow::Result;

use crate::proto::Namespace;

/// What a [`SyncResource`] hands to the engine when something
/// changed locally and needs to ship to the server. Built by the
/// resource itself (it owns serialization), then enqueued in the
/// outbox until the engine drains it
#[derive(Debug, Clone)]
pub struct LocalChange {
    /// Stable, content-addressable id where possible. For
    /// `clipboard_history` this is `blake3(content || ts)[..16]` so
    /// two devices copying same bytes converge naturally; for
    /// settings it might be a fixed string like `"theme"`
    pub id: String,
    pub op: SyncOp,
    /// Already-serialised body. Crypto + base64 happen later, in the
    /// engine, just before push
    pub payload: Vec<u8>,
    pub ts_local: i64,
}

/// What the engine hands to a resource after a successful pull. The
/// payload has been decrypted; bytes belong to the resource and the
/// engine doesn't peek inside
#[derive(Debug, Clone)]
pub struct RemoteChange {
    pub id: String,
    pub op: SyncOp,
    pub version: i64,
    /// ISO-8601 from the server. Useful for resources that want to
    /// surface "last synced from device X at ..." in diagnostics
    pub updated_at: String,
    pub payload: Vec<u8>,
}

/// Upsert vs tombstone. The wire format folds delete into a boolean
/// flag on `PushItem`/`PullItem`; this enum is the in-process shape
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOp {
    Upsert,
    Delete,
}

impl SyncOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            SyncOp::Upsert => "upsert",
            SyncOp::Delete => "delete",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "upsert" => Some(SyncOp::Upsert),
            "delete" => Some(SyncOp::Delete),
            _ => None,
        }
    }
}

pub trait SyncResource: Send + Sync {
    /// Which namespace this implements. Used to route remote changes
    /// to the right impl on pull, and to scope outbox queries
    fn namespace(&self) -> Namespace;

    /// Apply a remote change locally. Implementations MUST be
    /// idempotent - same `RemoteChange` can arrive twice if the
    /// engine retries a pull after a crash. Typical shape: "if local
    /// row with `id` exists at version >= `change.version`, skip;
    /// else upsert (or tombstone)."
    fn apply(&self, change: &RemoteChange) -> Result<()>;

    /// Round-trip a payload through deserialize + reserialize.
    /// Engine calls this against outbox rows just before encrypting,
    /// so a corrupt payload fails loudly at the seam instead of
    /// silently uploading garbage. Default trusts bytes
    fn validate_payload(&self, payload: &[u8]) -> Result<Vec<u8>> {
        Ok(payload.to_vec())
    }
}
