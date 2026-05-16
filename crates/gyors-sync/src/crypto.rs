//! Envelope encryption - AES-256-GCM, per-payload random nonce
//!
//! Wire format for a sealed blob:
//!
//! ```text
//! +--------+------------------------+
//! | nonce  | ciphertext + auth tag  |
//! | 12 B   | N + 16 B               |
//! +--------+------------------------+
//! ```
//!
//! The whole thing gets base64-encoded for `PushItem.ciphertext`.
//! Server stores opaque bytes; never inspects
//!
//! ## Why a trait
//!
//! Tests get a [`NullCrypto`] that round-trips bytes without keying
//! material so we can exercise the engine + outbox without pulling
//! user's password through every helper
//!
//! ## Why per-payload nonces, not a sequence
//!
//! A per-account counter would force every device to coordinate. A
//! 12-byte random nonce gives ~2^48 messages before birthday-bound
//! collision becomes a concern; we'd hit quota long before that

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::RngCore;

use crate::auth::Secret;

const NONCE_LEN: usize = 12;

/// Encrypt / decrypt the payload bytes that travel as
/// `PushItem.ciphertext` / `PullItem.ciphertext`
///
/// Object-safe so engine can hold `Box<dyn Crypto>` and swap
/// between [`AesGcmCrypto`] (real) and [`NullCrypto`] (tests)
pub trait Crypto: Send + Sync {
    /// Seal `plaintext`. Returns wire-encoded ciphertext (base64
    /// of nonce || tag-and-body)
    fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<String>;

    /// Reverse of `seal`. `aad` must match exactly or decryption
    /// fails (auth tag mismatch)
    fn open(&self, aad: &[u8], ciphertext_b64: &str) -> Result<Vec<u8>>;
}

/// Real AES-256-GCM. Holds the encryption key derived from
/// `password + encryption_salt` via Argon2id (see `auth.rs`)
pub struct AesGcmCrypto {
    cipher: Aes256Gcm,
}

impl AesGcmCrypto {
    pub fn new(key: &Secret) -> Self {
        // `Aes256Gcm::new` takes a `&Key<Aes256Gcm>` which is just a
        // `GenericArray<u8, U32>` - we own bytes so we copy in
        let cipher = Aes256Gcm::new(key.as_bytes().into());
        Self { cipher }
    }
}

impl Crypto for AesGcmCrypto {
    fn seal(&self, aad: &[u8], plaintext: &[u8]) -> Result<String> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let body = self
            .cipher
            .encrypt(
                nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|e| anyhow!("aes-gcm seal failed: {e}"))?;
        let mut out = Vec::with_capacity(NONCE_LEN + body.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&body);
        Ok(B64.encode(out))
    }

    fn open(&self, aad: &[u8], ciphertext_b64: &str) -> Result<Vec<u8>> {
        let raw = B64
            .decode(ciphertext_b64)
            .map_err(|e| anyhow!("ciphertext not base64: {e}"))?;
        if raw.len() < NONCE_LEN + 16 {
            return Err(anyhow!("ciphertext too short"));
        }
        let (nonce_bytes, body) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, Payload { msg: body, aad })
            .map_err(|e| anyhow!("aes-gcm open failed: {e}"))
    }
}

/// AAD used when wrapping the per-user DEK under the KEK.
/// Distinct from any blob AAD so a wrapped-key blob can never be
/// substituted for a payload blob (and vice versa). String is
/// also part of wire contract - if it ever changes, every
/// existing wrapped key becomes unreadable
const DEK_WRAP_AAD: &[u8] = b"gyors:dek:v1";

/// Wrap a freshly-generated DEK under the password-derived
/// KEK. Returns the base64-encoded wire form (nonce || ciphertext ||
/// tag) suitable for `SignupRequest.wrapped_data_key` and
/// `ChangePasswordRequest.new_wrapped_data_key`
///
/// `kek` is what `auth::derive_encryption_key(password, salt)`
/// produces. `dek` is `auth::fresh_dek()` at signup time, or the
/// already-unwrapped DEK at password-change time
pub fn wrap_dek(kek: &Secret, dek: &Secret) -> Result<String> {
    AesGcmCrypto::new(kek).seal(DEK_WRAP_AAD, dek.as_bytes())
}

/// Reverse of [`wrap_dek`]. Pulls wire-encoded wrapped
/// DEK off an `AuthResponse` and re-derives bytes the launcher
/// needs to encrypt sync blobs. Errors when the KEK is wrong (auth
/// tag mismatch) or the payload isn't a 32-byte secret
pub fn unwrap_dek(kek: &Secret, wrapped_b64: &str) -> Result<Secret> {
    let bytes = AesGcmCrypto::new(kek).open(DEK_WRAP_AAD, wrapped_b64)?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("unwrapped DEK wrong length: {}", bytes.len()))?;
    Ok(Secret::from_bytes(arr))
}

/// Identity crypto - base64-rounds bytes without encrypting.
/// Tests use this so they dont have to spin up a key. Production
/// must NEVER instantiate this
pub struct NullCrypto;

impl Crypto for NullCrypto {
    fn seal(&self, _aad: &[u8], plaintext: &[u8]) -> Result<String> {
        Ok(B64.encode(plaintext))
    }

    fn open(&self, _aad: &[u8], ciphertext_b64: &str) -> Result<Vec<u8>> {
        B64.decode(ciphertext_b64)
            .map_err(|e| anyhow!("null crypto: bad base64: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::derive_encryption_key;

    fn key() -> Secret {
        derive_encryption_key("hunter2", "AAAAAAAAAAAAAAAAAAAAAA==").unwrap()
    }

    #[test]
    fn roundtrip() {
        let c = AesGcmCrypto::new(&key());
        let aad = b"clipboard_history|abc123|7";
        let body = b"the quick brown fox";
        let sealed = c.seal(aad, body).unwrap();
        let opened = c.open(aad, &sealed).unwrap();
        assert_eq!(&opened, body);
    }

    #[test]
    fn aad_mismatch_fails() {
        let c = AesGcmCrypto::new(&key());
        let sealed = c.seal(b"good-aad", b"payload").unwrap();
        assert!(c.open(b"different-aad", &sealed).is_err());
    }

    #[test]
    fn different_keys_dont_open_each_other() {
        let a = AesGcmCrypto::new(&derive_encryption_key("a", "AAAAAAAAAAAAAAAAAAAAAA==").unwrap());
        let b = AesGcmCrypto::new(&derive_encryption_key("b", "AAAAAAAAAAAAAAAAAAAAAA==").unwrap());
        let sealed = a.seal(b"aad", b"secret").unwrap();
        assert!(b.open(b"aad", &sealed).is_err());
    }

    #[test]
    fn nonces_are_unique_per_seal() {
        // Same plaintext sealed twice must produce different
        // ciphertexts - that's the guarantee a fresh nonce buys
        let c = AesGcmCrypto::new(&key());
        let one = c.seal(b"aad", b"hello").unwrap();
        let two = c.seal(b"aad", b"hello").unwrap();
        assert_ne!(one, two);
    }

    #[test]
    fn null_crypto_roundtrips() {
        let c = NullCrypto;
        let body = b"plaintext-please";
        let sealed = c.seal(b"aad-ignored", body).unwrap();
        let opened = c.open(b"aad-ignored-different", &sealed).unwrap();
        assert_eq!(&opened, body);
    }

    // Wrap/unwrap regressions

    #[test]
    fn dek_wrap_unwrap_roundtrips() {
        let kek = key();
        let dek = crate::auth::fresh_dek();
        let wrapped = wrap_dek(&kek, &dek).unwrap();
        let back = unwrap_dek(&kek, &wrapped).unwrap();
        assert_eq!(dek.as_bytes(), back.as_bytes());
    }

    #[test]
    fn dek_unwrap_fails_with_wrong_kek() {
        let dek = crate::auth::fresh_dek();
        let good_kek = key();
        let wrong_kek = crate::auth::derive_encryption_key(
            "different",
            "AAAAAAAAAAAAAAAAAAAAAA==",
        )
        .unwrap();
        let wrapped = wrap_dek(&good_kek, &dek).unwrap();
        assert!(unwrap_dek(&wrong_kek, &wrapped).is_err());
    }

    #[test]
    fn dek_wrap_output_is_within_server_validator_window() {
        // Server's wrapped_data_key regex is 40-256 chars of
        // base64. A 32-byte secret wrapped with AES-GCM is
        // 12 nonce + 32 ct + 16 tag = 60 bytes, base64'd = 80
        // chars. We want this comfortably inside that range so
        // a tweak to either side surfaces as a test failure
        let kek = key();
        let dek = crate::auth::fresh_dek();
        let wrapped = wrap_dek(&kek, &dek).unwrap();
        assert!(wrapped.len() >= 40, "wrapped too short: {}", wrapped.len());
        assert!(wrapped.len() <= 256, "wrapped too long: {}", wrapped.len());
    }

    #[test]
    fn dek_wrap_each_call_uses_fresh_nonce() {
        // Same KEK + DEK wrapped twice should produce different
        // wire bytes because the nonce is per-seal random
        let kek = key();
        let dek = crate::auth::fresh_dek();
        let a = wrap_dek(&kek, &dek).unwrap();
        let b = wrap_dek(&kek, &dek).unwrap();
        assert_ne!(a, b);
        // Both still unwrap to same DEK
        assert_eq!(
            unwrap_dek(&kek, &a).unwrap().as_bytes(),
            unwrap_dek(&kek, &b).unwrap().as_bytes(),
        );
    }
}
