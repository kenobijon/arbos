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
}
