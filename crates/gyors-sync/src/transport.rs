//! Transport - the seam between gyors-sync and gyors-cloud
//!
//! Phase A (today) ships a [`NullTransport`] whose `push`/`pull`
//! succeed without doing anything. The engine drains the outbox into
//! it so we can exercise the local plumbing - encryption, version
//! tracking, conflict handling - before the real HTTP client lands
//!
//! Phase B replaces the null impl with `HttpTransport` against
//! `api.gyo.rs` (Hono routes in `gyors-cloud/src/routes/sync.ts`):
//!
//! - `POST /v1/sync/push`  ->  `PushRequest`  ->  `PushResponse`
//! - `GET  /v1/sync/pull?since=<cursor>`  ->  `PullResponse`
//!
//! Trait below is the minimum surface either backend has to honor

use async_trait::async_trait;
use std::sync::Mutex;
use thiserror::Error;

use crate::proto::{PullResponse, PushRequest, PushResponse};

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("not signed in")]
    Unauthenticated,
    #[error("session expired - re-auth required")]
    SessionExpired,
    #[error("over quota")]
    QuotaExceeded,
    #[error("rate limited; retry after {retry_after_s}s")]
    RateLimited { retry_after_s: u64 },
    #[error("network: {0}")]
    Network(String),
    #[error("server error: {0}")]
    Server(String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

#[async_trait]
pub trait Transport: Send + Sync {
    /// Ship a batch of encrypted items. Server returns one outcome
    /// per item - `accepted | conflict | rejected`. Caller persists
    /// the `cursor` (server's tip seq after the write) so a fresh
    /// pull starts from the right place
    async fn push(&self, req: PushRequest) -> Result<PushResponse, TransportError>;

    /// Pull deltas strictly newer than `since`. `None` means pull
    /// from the beginning (first sign-in on this device)
    async fn pull(&self, since: Option<&str>) -> Result<PullResponse, TransportError>;
}

/// Tests-and-skeleton transport. Records what was pushed so tests
/// can assert against it; `pull` returns nothing by default but can
/// be primed via [`NullTransport::queue_pull`]
pub struct NullTransport {
    state: Mutex<NullState>,
}

#[derive(Default)]
struct NullState {
    pushed: Vec<PushRequest>,
    pull_queue: Vec<PullResponse>,
}

impl NullTransport {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(NullState::default()),
        }
    }

    pub fn pushed(&self) -> Vec<PushRequest> {
        self.state.lock().unwrap().pushed.clone()
    }

    pub fn queue_pull(&self, resp: PullResponse) {
        self.state.lock().unwrap().pull_queue.push(resp);
    }
}

impl Default for NullTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for NullTransport {
    async fn push(&self, req: PushRequest) -> Result<PushResponse, TransportError> {
        let outcomes = req
            .items
            .iter()
            .map(|it| crate::proto::PushOutcome {
                id: it.id.clone(),
                namespace: it.namespace,
                status: crate::proto::PushStatus::Accepted,
                server_version: Some(it.version),
                reason: None,
            })
            .collect();
        self.state.lock().unwrap().pushed.push(req);
        Ok(PushResponse {
            // Sentinel cursor; engine round-trips it without inspecting
            cursor: "v1:null".to_string(),
            outcomes,
        })
    }

    async fn pull(&self, _since: Option<&str>) -> Result<PullResponse, TransportError> {
        let mut s = self.state.lock().unwrap();
        if let Some(next) = s.pull_queue.pop() {
            Ok(next)
        } else {
            Ok(PullResponse {
                items: vec![],
                cursor: "v1:null".to_string(),
                has_more: false,
            })
        }
    }
}
