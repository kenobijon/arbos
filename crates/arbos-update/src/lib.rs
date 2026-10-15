//! Arbos self-update (`arbos-design.md` D42).
//!
//! The feed is a Sparkle 2 appcast (RSS + the `sparkle:` namespace) and every
//! enclosure carries `sparkle:edSignature`, an Ed25519 signature over the
//! payload bytes. The public half of the key is compiled in from
//! `update-key.pub`; the private half lives only in the 1Password vault and in
//! CI. A payload installs only when its bytes verify against that key, so a
//! compromised download host alone cannot ship code to a Mac.
//!
//! Why a small Rust updater and not the Sparkle framework: the app is a GPUI
//! binary with no Xcode project, so Sparkle would need an Objective-C bridge,
//! an embedded framework plus two XPC services to sign, and its own alert UI
//! in front of the rail button. This crate does the four things Sparkle would
//! do for us (poll, download, verify, swap + relaunch) in ~600 lines that also
//! run on Linux, where the end-to-end flow is tested. The appcast is kept in
//! Sparkle's format so the framework can replace this crate without changing
//! the release pipeline.

pub mod appcast;
pub mod client;
pub mod sign;
pub mod version;

/// `ed25519 <base64 public key>`: the key every payload must verify against.
pub const PUBLIC_KEY_LINE: &str = include_str!("../update-key.pub");

/// The compiled-in public key, or `None` while the key file holds no key yet
/// (a fresh fork). Without a key every update is refused.
pub fn public_key() -> Option<sign::PublicKey> {
    sign::PublicKey::parse_line(PUBLIC_KEY_LINE).ok()
}

/// The default feed: the `appcast.xml` asset of the newest GitHub release.
/// `releases/latest/download/<asset>` is a stable URL that follows the
/// release marked latest, so the app never needs to learn a new address.
pub const DEFAULT_FEED_URL: &str =
    "https://github.com/unarbos/arbos/releases/latest/download/appcast.xml";

/// The `sparkle:os` value for the platform this binary runs on.
pub const fn current_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}
