//! Ed25519 over payload bytes, in Sparkle's encoding: the public key is the
//! base64 of the 32 raw key bytes, the signature the base64 of the 64 raw
//! signature bytes (`sparkle:edSignature`). Sparkle's `sign_update` tool
//! produces the same bytes, so either tool can sign a release.
//!
//! Key file lines:
//!   `ed25519 <base64 32 bytes>`           public (committed, compiled in)
//!   `ed25519-private <base64 32 or 64>`   private seed (vault + CI only)

use anyhow::{bail, Context};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey);

pub struct PrivateKey(SigningKey);

impl PublicKey {
    /// Accepts `ed25519 <b64>` (with or without comment lines) or bare base64.
    pub fn parse_line(text: &str) -> anyhow::Result<PublicKey> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .context("no key line (expected `ed25519 <base64>`)")?;
        let b64 = line.strip_prefix("ed25519").map(str::trim).unwrap_or(line);
        let bytes = B64.decode(b64).context("public key is not base64")?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|v: Vec<u8>| anyhow::anyhow!("public key is {} bytes, want 32", v.len()))?;
        Ok(PublicKey(
            VerifyingKey::from_bytes(&bytes).context("invalid Ed25519 public key")?,
        ))
    }

    pub fn to_base64(&self) -> String {
        B64.encode(self.0.as_bytes())
    }

    pub fn to_line(&self) -> String {
        format!("ed25519 {}", self.to_base64())
    }

    /// Verify a Sparkle `edSignature` over `bytes`.
    pub fn verify(&self, bytes: &[u8], signature_b64: &str) -> anyhow::Result<()> {
        let sig = B64
            .decode(signature_b64.trim())
            .context("signature is not base64")?;
        let sig = Signature::from_slice(&sig).context("signature is not 64 bytes")?;
        self.0
            .verify(bytes, &sig)
            .map_err(|_| anyhow::anyhow!("Ed25519 signature does not match the payload"))
    }
}

impl PrivateKey {
    pub fn generate() -> PrivateKey {
        PrivateKey(SigningKey::generate(&mut rand::rngs::OsRng))
    }

    /// Accepts `ed25519-private <b64>` or bare base64 of the 32-byte seed, or
    /// of 64 bytes (seed followed by public key, as Sparkle stores it).
    pub fn parse(text: &str) -> anyhow::Result<PrivateKey> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .context("no private key line")?;
        let b64 = line
            .strip_prefix("ed25519-private")
            .map(str::trim)
            .unwrap_or(line);
        let bytes = B64.decode(b64).context("private key is not base64")?;
        let seed: [u8; 32] = match bytes.len() {
            32 | 64 | 96 => bytes[..32].try_into().expect("32 bytes"),
            n => bail!("private key is {n} bytes, want 32 (seed) or 64 (seed+public)"),
        };
        Ok(PrivateKey(SigningKey::from_bytes(&seed)))
    }

    pub fn to_line(&self) -> String {
        format!("ed25519-private {}", B64.encode(self.0.to_bytes()))
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }

    pub fn sign(&self, bytes: &[u8]) -> String {
        B64.encode(self.0.sign(bytes).to_bytes())
    }
}

/// Hex SHA-256 of `bytes`, written next to the signature for humans and for
/// the GitHub release notes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(bytes))
}
