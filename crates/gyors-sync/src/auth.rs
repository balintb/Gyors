//! Password-derived material. Two independent KDF chains feed two
//! distinct secrets:
//!
//! 1. Auth verifier - Argon2id over `password + kdf_salt`.
//!    Sent to the server, which re-hashes with PBKDF2 before storing
//!    (`gyors-cloud/lib/crypto.ts::pbkdf2VerifierHash`). The server
//!    never sees the password itself
//!
//! 2. Encryption key - Argon2id over `password + encryption_salt`.
//!    Stays on the device. Drives AES-256-GCM. The server never sees
//!    this either, and it's derived from a *different* salt so a
//!    verifier leak doesn't reveal the encryption key (and vice
//!    versa)
//!
//! Salts are 16 bytes of OS RNG, base64-encoded for transport. They
//! must be stable across a user's lifetime - if the salt changes, the
//! key changes, and existing ciphertext becomes unreadable

use anyhow::{anyhow, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::RngCore;

/// 32-byte secret derived from password material
///
/// Wrapped so it doesn't accidentally end up in `Debug` output or
/// serialized somewhere it shouldn't. Use `as_bytes()` only at the
/// crypto boundary
pub struct Secret([u8; 32]);

impl Secret {
    /// Wrap raw 32-byte material as a `Secret`. Used by [`fresh_dek`]
    /// and by the unwrap path (the bytes coming out of
    /// AES-GCM-decrypt aren't base64). Direct constructor isn't
    /// exposed via `pub fn new` because only legitimate sources
    /// of these bytes are KDF output, random generation, or a
    /// just-unwrapped wire-shaped DEK
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Secret(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Hex encoding - wire form for `auth_verifier` (server expects
    /// the verifier as a printable string, not raw bytes)
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Base64 encoding - used by Keychain-resident session
    /// blob to round-trip the AES key. Shorter than hex by a third
    /// and keeps the storage shape consistent with `kdf_salt` /
    /// `encryption_salt`, which are also base64 in protocol
    pub fn to_b64(&self) -> String {
        B64.encode(self.0)
    }

    /// Reverse of [`to_b64`]. Errors on bad encoding or
    /// wrong-length payloads - if either happens session blob
    /// is corrupt and caller should treat it as "force
    /// re-sign-in" rather than trusting bytes
    pub fn from_b64(s: &str) -> Result<Self> {
        let bytes = B64
            .decode(s)
            .map_err(|e| anyhow!("encryption key not base64: {e}"))?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("encryption key wrong length: {}", bytes.len()))?;
        Ok(Secret(arr))
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Argon2id parameters tuned for an interactive sign-in flow on a
/// modern Mac. ~50ms on M-series silicon at the time of writing -
/// fast enough that user doesn't see it as a stall, slow enough
/// that an offline brute-force is meaningfully expensive even with
/// a leaked verifier
fn argon2() -> Argon2<'static> {
    // 64 MiB memory, 3 iterations, 1 lane. Output length is fixed at
    // 32 bytes by caller via `hash_password_into`
    let params = Params::new(64 * 1024, 3, 1, Some(32))
        .expect("argon2 params are statically valid");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Argon2id over `(password, salt)`. Used for both auth verifier
/// (with `kdf_salt`) and the encryption key (with `encryption_salt`).
/// Caller is responsible for picking the right salt - mixing them up
/// silently produces unrelated bytes
fn derive(password: &[u8], salt_b64: &str) -> Result<Secret> {
    let salt = B64
        .decode(salt_b64)
        .map_err(|e| anyhow!("invalid salt base64: {e}"))?;
    if salt.is_empty() {
        return Err(anyhow!("salt must not be empty"));
    }
    let mut out = [0u8; 32];
    argon2()
        .hash_password_into(password, &salt, &mut out)
        .map_err(|e| anyhow!("argon2 derive failed: {e}"))?;
    Ok(Secret(out))
}

/// Compute `auth_verifier` for sign-up / sign-in. The server will
/// re-hash with PBKDF2 before storing as `users.auth_hash`
pub fn derive_verifier(password: &str, kdf_salt_b64: &str) -> Result<Secret> {
    derive(password.as_bytes(), kdf_salt_b64)
}

/// Derive the AES-256-GCM key used to seal payloads. Lives only on
/// the device; never leaves it
pub fn derive_encryption_key(password: &str, encryption_salt_b64: &str) -> Result<Secret> {
    derive(password.as_bytes(), encryption_salt_b64)
}

/// Fresh 16-byte salt, base64-encoded for wire. Caller persists
/// result so future sign-ins on same account derive the same
/// secrets
pub fn fresh_salt() -> String {
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    B64.encode(buf)
}

/// Fresh 32-byte data-encryption key. Generated once at
/// signup, wrapped under the password-derived KEK, and persisted on
/// the server in opaque form. Stays stable across password changes -
/// rotating the password rewraps this same DEK under a new KEK, so
/// existing ciphertext keeps decrypting
pub fn fresh_dek() -> Secret {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    Secret(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_password_same_salt_same_secret() {
        let salt = fresh_salt();
        let a = derive_verifier("hunter2", &salt).unwrap();
        let b = derive_verifier("hunter2", &salt).unwrap();
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn different_salt_different_secret() {
        let s1 = fresh_salt();
        let s2 = fresh_salt();
        let a = derive_verifier("hunter2", &s1).unwrap();
        let b = derive_verifier("hunter2", &s2).unwrap();
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn verifier_and_encryption_key_diverge() {
        // Same password but the two chains use independent salts -
        // by construction the two outputs must not match
        let kdf_salt = fresh_salt();
        let enc_salt = fresh_salt();
        let v = derive_verifier("hunter2", &kdf_salt).unwrap();
        let k = derive_encryption_key("hunter2", &enc_salt).unwrap();
        assert_ne!(v.as_bytes(), k.as_bytes());
    }

    #[test]
    fn empty_salt_rejected() {
        assert!(derive_verifier("hunter2", "").is_err());
    }

    #[test]
    fn secret_b64_roundtrip() {
        let s = derive_verifier("hunter2", &fresh_salt()).unwrap();
        let encoded = s.to_b64();
        let back = Secret::from_b64(&encoded).unwrap();
        assert_eq!(s.as_bytes(), back.as_bytes());
    }

    #[test]
    fn secret_from_b64_rejects_garbage() {
        assert!(Secret::from_b64("not-base64!!!").is_err());
    }

    #[test]
    fn secret_from_b64_rejects_wrong_length() {
        let short = B64.encode([0u8; 16]); // 16 bytes, not 32
        assert!(Secret::from_b64(&short).is_err());
        let long = B64.encode([0u8; 64]);
        assert!(Secret::from_b64(&long).is_err());
    }
}
