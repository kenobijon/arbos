//! Remote feature flags the apps read from `GET /me` → `features` (one object, one place).
//! Each flag is an env switch on the relay, flipped with a redeploy and no client release.
//!
//! | flag | env | default |
//! | --- | --- | --- |
//! | `leaf_avatars` (D91: agents without a photo draw a leaf, not initials) | `ARBOS_FEATURE_LEAF_AVATARS` | on; `0` / `false` / `off` / `no` is the kill switch |
//!
//! `vault_connect` (`ARBOS_FEATURE_VAULT_CONNECT`, default off, #1265) is added here by the
//! secrets module once it lands; until then it rides on `GET /secrets`.

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

/// D91 leaf avatars for agents. Default on.
pub fn leaf_avatars() -> bool {
    env_flag("ARBOS_FEATURE_LEAF_AVATARS", true)
}

/// The `features` object on `GET /me`.
pub fn features() -> Value {
    json!({ "leaf_avatars": leaf_avatars() })
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
}
