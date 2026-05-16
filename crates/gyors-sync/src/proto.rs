//! Wire types - Rust mirror of `gyors-cloud/src/lib/proto.ts`
//!
//! Names and shapes are 1:1 with the TypeScript so a breaking change
//! on either side surfaces in compile errors here. Requests are
//! *closed* shapes (server rejects unknown fields). Responses are
//! *open* - server may add optional fields without bumping
//! `PROTOCOL_VERSION`, so old clients stay compatible

use serde::{Deserialize, Serialize};

/// Bumped only on breaking changes; additive fields dont ratchet
pub const PROTOCOL_VERSION: u32 = 1;


/// Logical buckets the server treats as opaque. The client decides
/// how each blob ID maps onto its own local data structures
///
/// Keep this enum in sync with `proto.ts::NAMESPACES`. Adding a
/// variant is non-breaking; removing one is breaking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    Snippets,
    ClipboardHistory,
    Notes,
    Settings,
    Themes,
    PluginRegistry,
    Frecency,
    QueryHistory,
    Scratchpad,
}

impl Namespace {
    pub const fn as_str(self) -> &'static str {
        match self {
            Namespace::Snippets => "snippets",
            Namespace::ClipboardHistory => "clipboard_history",
            Namespace::Notes => "notes",
            Namespace::Settings => "settings",
            Namespace::Themes => "themes",
            Namespace::PluginRegistry => "plugin_registry",
            Namespace::Frecency => "frecency",
            Namespace::QueryHistory => "query_history",
            Namespace::Scratchpad => "scratchpad",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "snippets" => Some(Namespace::Snippets),
            "clipboard_history" => Some(Namespace::ClipboardHistory),
            "notes" => Some(Namespace::Notes),
            "settings" => Some(Namespace::Settings),
            "themes" => Some(Namespace::Themes),
            "plugin_registry" => Some(Namespace::PluginRegistry),
            "frecency" => Some(Namespace::Frecency),
            "query_history" => Some(Namespace::QueryHistory),
            "scratchpad" => Some(Namespace::Scratchpad),
            _ => None,
        }
    }

    /// Conflict policy for this namespace. The server is dumb -
    /// version-monotonic upserts are all it knows. The client is
    /// where merge-vs-LWW actually happens
    pub const fn conflict_policy(self) -> ConflictPolicy {
        match self {
            // Content-addressable: the id is a hash of bytes, so two
            // devices can't collide on a meaningful conflict - same id
            // means same content. On collision, older version wins
            // (it's same row anyway)
            Namespace::ClipboardHistory
            | Namespace::QueryHistory
            | Namespace::Frecency
            | Namespace::Notes => ConflictPolicy::Merge,
            // User-edited singletons - last write wins
            Namespace::Settings
            | Namespace::Themes
            | Namespace::PluginRegistry
            | Namespace::Snippets
            | Namespace::Scratchpad => ConflictPolicy::LastWriteWins,
        }
    }
}

impl Serialize for Namespace {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Namespace {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Namespace::from_str(&s).ok_or_else(|| serde::de::Error::custom("unknown namespace"))
    }
}

/// How the engine reconciles a push that the server rejects with
/// `conflict`. Drives what happens *after* we've adopted the server's
/// version - whether to drop our local change or attempt a merge
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Server's version replaces ours. Use for items where user's
    /// "intent" is the singular state (settings, themes)
    LastWriteWins,
    /// Both sides survive. Only sound when ids are content-addressable
    /// - two writers with different content produce different ids,
    ///   so an actual id-collision means equal content
    Merge,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountTier {
    Free,
    Plus,
    Pro,
}

// `AccountTier` carries no gating logic on the client. Every device
// pushes whatever it has queued; the server decides what to keep
// based on user's tier. Putting policy in one place (server
// only) means a modified client can't bypass the cap, and lets us
// retune tier sizes without shipping a new launcher build. Client
// just renders the tier label in the UI ("Plus", "Free", ...)


#[derive(Debug, Clone, Serialize)]
pub struct SignupRequest {
    pub email: String,
    /// Client-derived verifier (Argon2id over password + kdf_salt).
    /// See `auth::derive_verifier`
    pub auth_verifier: String,
    /// Salt for auth KDF - server stores so other devices re-derive
    pub kdf_salt: String,
    /// Salt for the *encryption* KDF - independent chain so a verifier
    /// leak doesn't open the encryption key
    pub encryption_salt: String,
    /// Per-user data-encryption-key, wrapped under the
    /// password-derived KEK. The server stores opaque bytes; only
    /// the client can unwrap. Generated once at signup and kept
    /// stable across password changes - rotating the password
    /// re-wraps same DEK under a new KEK
    pub wrapped_data_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SigninRequest {
    pub email: String,
    pub auth_verifier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthResponse {
    pub user_id: String,
    pub session_token: String,
    pub kdf_salt: String,
    pub encryption_salt: String,
    /// Server echoes user's wrapped DEK on every auth
    /// response (signup / signin / refresh) so a fresh client can
    /// rebuild the DEK after a cold start without needing to
    /// remember it separately
    pub wrapped_data_key: String,
    pub tier: AccountTier,
    pub expires_at: String,
}

/// Password change. Old verifier proves user knows the
/// existing password; the new fields replace stored material.
/// The DEK itself is unchanged - `new_wrapped_data_key` is just the
/// same key wrapped under the new KEK, so existing ciphertext stays
/// readable
#[derive(Debug, Clone, Serialize)]
pub struct ChangePasswordRequest {
    pub old_auth_verifier: String,
    pub new_auth_verifier: String,
    pub new_kdf_salt: String,
    pub new_encryption_salt: String,
    pub new_wrapped_data_key: String,
}


#[derive(Debug, Clone, Serialize)]
pub struct PushItem {
    pub id: String,
    pub namespace: Namespace,
    pub version: i64,
    /// Base64-encoded ciphertext. Server never inspects bytes
    pub ciphertext: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PushRequest {
    pub items: Vec<PushItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushStatus {
    Accepted,
    Conflict,
    Rejected,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PushOutcome {
    pub id: String,
    pub namespace: Namespace,
    pub status: PushStatus,
    #[serde(default)]
    pub server_version: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PushResponse {
    pub cursor: String,
    pub outcomes: Vec<PushOutcome>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullItem {
    pub id: String,
    pub namespace: Namespace,
    pub version: i64,
    pub ciphertext: String,
    pub deleted: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullResponse {
    pub items: Vec<PullItem>,
    pub cursor: String,
    pub has_more: bool,
}


#[derive(Debug, Clone, Deserialize)]
pub struct AccountInfo {
    pub user_id: String,
    pub email: String,
    pub tier: AccountTier,
    pub usage_bytes: i64,
    pub quota_bytes: i64,
    pub created_at: String,
}


#[derive(Debug, Clone, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    QuotaExceeded,
    ServerError,
}
