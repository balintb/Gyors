//! Generic E2EE sync. One outbox, one transport, many namespaces
//!
//! ## Why a generic engine
//!
//! Sync isn't clipboard-specific. Snippets, themes, query history,
//! frecency - they all want same primitives: local change
//! capture, encrypted upload, conflict-resolved pull, key
//! management. Building a per-namespace stack would force all of
//! that to drift across N implementations. So [`SyncResource`] is
//! trait every namespace plugs into; [`SyncEngine`] reads from a
//! single [`gyors_index::Index`] outbox and dispatches per namespace
//!
//! ## Server protocol
//!
//! Wire types are a 1:1 mirror of `gyors-cloud/src/lib/proto.ts` -
//! see [`proto`]. The server (Cloudflare Workers + D1, deployed at
//! `api.gyo.rs`) treats every blob as opaque ciphertext and enforces
//! version monotonicity per `(user, namespace, item_id)`. Conflict
//! resolution lives on the client, per-namespace, via
//! [`ConflictPolicy`](crate::proto::ConflictPolicy)
//!
//! ## Phasing
//!
//! - Phase A (today): real Argon2id + AES-256-GCM, [`NullTransport`]
//!   so we can exercise local plumbing without a network. Clipboard
//!   is the first registered namespace.
//! - Phase B: HTTP transport against `api.gyo.rs/v1/sync/*`,
//!   sign-in UI, Keychain-resident encryption key, paid-tier gating
//!
//! The seam between phases is [`Transport`] - swap `NullTransport`
//! for `HttpTransport` and the engine is unchanged

use std::sync::Arc;

pub mod auth;
pub mod clipboard;
pub mod crypto;
pub mod engine;
pub mod http;
#[cfg(target_os = "macos")]
pub mod keychain;
pub mod proto;
pub mod resource;
pub mod session;
pub mod settings;
pub mod transport;

pub use clipboard::ClipboardResource;
pub use crypto::{unwrap_dek, wrap_dek, AesGcmCrypto, Crypto, NullCrypto};
pub use engine::{SyncEngine, SyncEngineConfig, TickReport};
pub use http::{
    base_url, change_password, delete_account, refresh_session, signin, signup, HttpTransport,
};
pub use proto::{
    AccountTier, AuthResponse, ChangePasswordRequest, ConflictPolicy, Namespace, PushItem,
    PushOutcome, PushRequest, PushResponse, PushStatus, SigninRequest, SignupRequest,
    PROTOCOL_VERSION,
};
pub use resource::{LocalChange, RemoteChange, SyncOp, SyncResource};
pub use session::{default_session_path, Session};
pub use settings::SettingsResource;
pub use transport::{NullTransport, Transport, TransportError};

/// Re-export for downstream callers wiring sync at app startup
pub type SharedIndex = Arc<gyors_index::Index>;
