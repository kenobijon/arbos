//! Token minting and hashing. Tokens are random 32-byte strings; only their
//! SHA-256 hash is stored (design §5 `*_token_hash`).

use rand::RngCore;
use sha2::{Digest, Sha256};

/// Token prefixes make a leaked string identifiable in logs and greps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Device,
    Box,
    Project,
    Mcp,
}

impl TokenKind {
    pub fn prefix(self) -> &'static str {
        match self {
            TokenKind::Device => "adv_",
            TokenKind::Box => "abx_",
            TokenKind::Project => "apr_",
            TokenKind::Mcp => "amc_",
        }
    }
}

/// Mint a new random token with the kind's prefix.
pub fn mint(kind: TokenKind) -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{}{}", kind.prefix(), hex::encode(bytes))
}

/// SHA-256 hex of a token; the only form that is ever persisted.
pub fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Six-digit one-time pairing code (D35).
pub fn pairing_code() -> String {
    let n = rand::thread_rng().next_u32() % 1_000_000;
    format!("{n:06}")
}
