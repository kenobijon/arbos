//! Remote feature flags the apps read from `GET /me` → `features` (one object, one place).
//! Each flag is an env switch on the relay, flipped with a redeploy and no client release.
//!
//! | flag | env | default |
//! | --- | --- | --- |
//! | `vault_connect` (#1265: the Connect 1Password flow) | `ARBOS_FEATURE_VAULT_CONNECT` (`vaults.rs`) | off |
//! | `leaf_avatars` (D92: agents without a photo draw a leaf, not initials) | `ARBOS_FEATURE_LEAF_AVATARS` | on; `0` / `false` / `off` / `no` is the kill switch |
//! | `voice_mode` (spoken replies, voice picker, reply playback; 2026-10-22 Jacob: hidden until it works end to end) | `ARBOS_FEATURE_VOICE_MODE` | off; the relay also refuses `/proxy/v1/audio/speech` while off, so no client can spend on TTS |
//!
//! `GET /secrets` serves the same object (`vaults::features()` delegates here).

use arbos_core::api::MeFeaturesDto;
use serde_json::{json, Value};

/// `1|true|on|yes` → on, `0|false|off|no` → off, anything else → `default`.
pub fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => true,
            "0" | "false" | "off" | "no" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

/// D92 leaf avatars for agents. Default on.
pub fn leaf_avatars() -> bool {
    env_flag("ARBOS_FEATURE_LEAF_AVATARS", true)
}

/// Voice mode (spoken replies, voice picker, playback). Default off; `ARBOS_FEATURE_VOICE_MODE=1`
/// brings it back without a client release.
pub fn voice_mode() -> bool {
    env_flag("ARBOS_FEATURE_VOICE_MODE", false)
}

/// The `features` object on `GET /me` and `GET /secrets`. Key names are a contract with the
/// shipped apps (Mac v0.6.104 / TestFlight read `vault_connect`); only add, never rename.
pub fn dto() -> MeFeaturesDto {
    MeFeaturesDto {
        vault_connect: Some(crate::vaults::vault_connect()),
        leaf_avatars: Some(leaf_avatars()),
        voice_mode: Some(voice_mode()),
    }
}

/// `dto()` as JSON (for `GET /secrets`, whose body is hand-built).
pub fn features() -> Value {
    serde_json::to_value(dto()).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_parsing() {
        std::env::remove_var("ARBOS_FEATURE_TEST_FLAG");
        assert!(env_flag("ARBOS_FEATURE_TEST_FLAG", true));
        assert!(!env_flag("ARBOS_FEATURE_TEST_FLAG", false));
        for (v, want) in [
            ("0", false),
            ("off", false),
            ("NO", false),
            ("1", true),
            ("on", true),
            ("maybe", true),
        ] {
            std::env::set_var("ARBOS_FEATURE_TEST_FLAG", v);
            assert_eq!(env_flag("ARBOS_FEATURE_TEST_FLAG", true), want, "{v}");
        }
        std::env::remove_var("ARBOS_FEATURE_TEST_FLAG");
    }

    /// Voice mode is off unless the relay is started with the flag; the object always carries
    /// the key, so a client never has to guess. (The flag itself is not flipped here: other
    /// tests snapshot `features()` concurrently.)
    #[test]
    fn voice_mode_is_off_by_default_and_always_sent() {
        if std::env::var_os("ARBOS_FEATURE_VOICE_MODE").is_none() {
            assert!(!voice_mode());
            assert_eq!(features()["voice_mode"], serde_json::json!(false));
        }
        assert!(features().get("voice_mode").is_some());
    }
}
