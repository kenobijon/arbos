//! Encryption at rest for connector tokens: AES-256-GCM under one relay key.
//! The key comes from `ARBOS_RELAY_KEY` (any string; hashed to 32 bytes) or is
//! generated once and kept in the `meta` table (`relay_key`).

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::db::Db;

const META_KEY: &str = "relay_key";

#[derive(Clone)]
pub struct RelayKey([u8; 32]);

impl std::fmt::Debug for RelayKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RelayKey(…)")
    }
}

impl RelayKey {
    /// `ARBOS_RELAY_KEY` when set, else the stored key, else a fresh random one stored in `meta`.
    pub fn load(db: &Db) -> Result<Self> {
        if let Some(k) = std::env::var("ARBOS_RELAY_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(Self::derive(k.trim()));
        }
        if let Some(stored) = db.meta_get(META_KEY)? {
            return Ok(Self::derive(&stored));
        }
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let encoded = hex::encode(bytes);
        db.meta_set(META_KEY, &encoded)?;
        tracing::info!("generated a relay key for connector tokens (stored in meta; set ARBOS_RELAY_KEY to pin one)");
        Ok(Self::derive(&encoded))
    }

    /// Hashing lets the operator pass any passphrase while the cipher always gets 32 bytes.
    pub fn derive(secret: &str) -> Self {
        let mut out = [0u8; 32];
        out.copy_from_slice(&Sha256::digest(
            format!("arbos-relay-key:{secret}").as_bytes(),
        ));
        Self(out)
    }

    /// A 32-byte key for another purpose (JWT signing), derived so the two never share bytes.
    pub fn subkey(&self, purpose: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"arbos-relay-subkey:");
        h.update(purpose.as_bytes());
        h.update(b":");
        h.update(self.0);
        let mut out = [0u8; 32];
        out.copy_from_slice(&h.finalize());
        out
    }

    /// `base64(nonce || ciphertext)`.
    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|_| anyhow!("encrypt"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        Ok(B64.encode(out))
    }

    pub fn decrypt(&self, encoded: &str) -> Result<String> {
        let raw = B64.decode(encoded.trim())?;
        if raw.len() < 12 {
            return Err(anyhow!("ciphertext too short"));
        }
        let (nonce, ct) = raw.split_at(12);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let pt = cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| anyhow!("decrypt: wrong relay key or corrupt row"))?;
        Ok(String::from_utf8(pt)?)
    }

    /// True when the key came from `ARBOS_RELAY_KEY` (env or `op run`), not from the `meta` table
    /// next to the ciphertext. Production should log loudly when this is false (D83 decision 9).
    pub fn from_env() -> bool {
        std::env::var("ARBOS_RELAY_KEY").is_ok_and(|s| !s.trim().is_empty())
    }

    /// Wrap a per-user data-encryption key under this key's `secrets-dek-wrap` subkey.
    pub fn wrap_dek(&self, dek: &Dek) -> Result<String> {
        let kek = Dek(self.subkey("secrets-dek-wrap"));
        kek.encrypt(b"dek", &hex::encode(dek.0))
    }

    pub fn unwrap_dek(&self, wrapped: &str) -> Result<Dek> {
        let kek = Dek(self.subkey("secrets-dek-wrap"));
        let hex_key = kek.decrypt(b"dek", wrapped)?;
        let bytes = hex::decode(hex_key.trim())?;
        let mut out = [0u8; 32];
        if bytes.len() != 32 {
            return Err(anyhow!("wrapped dek has the wrong length"));
        }
        out.copy_from_slice(&bytes);
        Ok(Dek(out))
    }
}

/// A per-user data-encryption key (D83 §7.2): secret values and vault tokens are AES-256-GCM
/// under it with additional authenticated data binding each ciphertext to its row, so a
/// ciphertext cannot be moved to another secret or version. Deleting the wrapped key deletes
/// every value of that user (crypto-shredding).
#[derive(Clone)]
pub struct Dek([u8; 32]);

impl std::fmt::Debug for Dek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Dek(…)")
    }
}

impl Dek {
    pub fn random() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// `base64(nonce || ciphertext)` with `aad` authenticated but not stored.
    pub fn encrypt(&self, aad: &[u8], plaintext: &str) -> Result<String> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(
                &nonce,
                aes_gcm::aead::Payload {
                    msg: plaintext.as_bytes(),
                    aad,
                },
            )
            .map_err(|_| anyhow!("encrypt"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        Ok(B64.encode(out))
    }

    pub fn decrypt(&self, aad: &[u8], encoded: &str) -> Result<String> {
        let raw = B64.decode(encoded.trim())?;
        if raw.len() < 12 {
            return Err(anyhow!("ciphertext too short"));
        }
        let (nonce, ct) = raw.split_at(12);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let pt = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                aes_gcm::aead::Payload { msg: ct, aad },
            )
            .map_err(|_| anyhow!("decrypt: wrong key, wrong row, or corrupt ciphertext"))?;
        Ok(String::from_utf8(pt)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ciphertext is bound to its row: the same bytes under another AAD do not decrypt, and a
    /// DEK wrapped by one relay key does not unwrap under another.
    #[test]
    fn aad_binds_and_wrap_round_trips() {
        let dek = Dek::random();
        let ct = dek.encrypt(b"secret-1|1", "sk-live-123").unwrap();
        assert_eq!(dek.decrypt(b"secret-1|1", &ct).unwrap(), "sk-live-123");
        assert!(dek.decrypt(b"secret-1|2", &ct).is_err());
        assert!(dek.decrypt(b"secret-2|1", &ct).is_err());

        let kek = RelayKey::derive("relay-a");
        let wrapped = kek.wrap_dek(&dek).unwrap();
        assert!(!wrapped.contains(&hex::encode(dek.0)));
        let back = kek.unwrap_dek(&wrapped).unwrap();
        assert_eq!(back.decrypt(b"secret-1|1", &ct).unwrap(), "sk-live-123");
        assert!(RelayKey::derive("relay-b").unwrap_dek(&wrapped).is_err());
    }
}
