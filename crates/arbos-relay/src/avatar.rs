//! Project avatars: a slug from the tree manifest the apps ship. New projects get a
//! deterministic default so every client and the relay agree without a round trip:
//! `slugs[fnv1a64(project_id) % slugs.len()]`, over the manifest in its listed order.
//!
//! The manifest reaches the relay through `ARBOS_AVATAR_MANIFEST` (a JSON file: `["oak", …]`,
//! `{"slugs": […]}`, or `{"avatars": [{"slug": …}, …]}`) or `ARBOS_AVATAR_SLUGS` (comma list);
//! without either, the built-in list below is used; it equals the apps' manifest (2026-10-15),
//! in manifest order.

use serde_json::Value;

pub const DEFAULT_SLUGS: &[&str] = &[
    "oak", "pine", "lollipop", "fractal", "mushroom", "cypress", "willow", "palm", "bonsai",
    "birch", "baobab", "sapling", "maple", "acacia", "spruce", "cherry", "cactus", "sequoia",
    "olive", "ginkgo",
];

/// FNV-1a 64 over the UTF-8 bytes (the same arithmetic is one line in Swift).
pub fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn default_for(project_id: &str, slugs: &[String]) -> String {
    if slugs.is_empty() {
        return DEFAULT_SLUGS[(fnv1a64(project_id) % DEFAULT_SLUGS.len() as u64) as usize]
            .to_owned();
    }
    slugs[(fnv1a64(project_id) % slugs.len() as u64) as usize].clone()
}

/// The default for a new project of a user: start at `default_for(project_id)` and probe forward
/// (wrapping) to the first slug none of the user's existing projects uses; when every slug is
/// taken, keep the hash pick. Stored on the row at creation, so apps never have to guess.
pub fn default_unused(project_id: &str, slugs: &[String], taken: &[String]) -> String {
    let list: Vec<String> = if slugs.is_empty() {
        DEFAULT_SLUGS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        slugs.to_vec()
    };
    let start = (fnv1a64(project_id) % list.len() as u64) as usize;
    for i in 0..list.len() {
        let candidate = &list[(start + i) % list.len()];
        if !taken.iter().any(|t| t == candidate) {
            return candidate.clone();
        }
    }
    list[start].clone()
}

/// Slug list from the env (file or list), else the built-in default.
pub fn slugs_from_env() -> Vec<String> {
    if let Ok(path) = std::env::var("ARBOS_AVATAR_MANIFEST") {
        if let Ok(text) = std::fs::read_to_string(path.trim()) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                let list = match &v {
                    Value::Array(a) => a.clone(),
                    Value::Object(o) => o
                        .get("slugs")
                        .or_else(|| o.get("avatars"))
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                let slugs: Vec<String> = list
                    .iter()
                    .filter_map(|e| {
                        e.as_str()
                            .map(str::to_owned)
                            .or_else(|| e.get("slug").and_then(Value::as_str).map(str::to_owned))
                    })
                    .collect();
                if !slugs.is_empty() {
                    return slugs;
                }
            }
        }
    }
    if let Ok(list) = std::env::var("ARBOS_AVATAR_SLUGS") {
        let slugs: Vec<String> = list
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if !slugs.is_empty() {
            return slugs;
        }
    }
    DEFAULT_SLUGS.iter().map(|s| (*s).to_owned()).collect()
}

/// A slug a client may set: short, lower-case, letters/digits/hyphens.
pub fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_unused_probes_forward_and_wraps() {
        let slugs: Vec<String> = DEFAULT_SLUGS.iter().map(|s| (*s).to_owned()).collect();
        let first = default_for("P1", &slugs);
        assert_eq!(default_unused("P1", &slugs, &[]), first);
        let start = slugs.iter().position(|s| *s == first).unwrap();
        let next = slugs[(start + 1) % slugs.len()].clone();
        assert_eq!(default_unused("P1", &slugs, &[first.clone()]), next);
        // Every slug taken: the hash pick stays.
        assert_eq!(default_unused("P1", &slugs, &slugs), first);
    }

    #[test]
    fn default_is_deterministic_and_in_range() {
        let slugs: Vec<String> = DEFAULT_SLUGS.iter().map(|s| (*s).to_owned()).collect();
        let a = default_for("01HPROJECT", &slugs);
        assert_eq!(a, default_for("01HPROJECT", &slugs));
        assert!(slugs.contains(&a));
        // Reference values for the apps' implementation of the same rule (FNV-1a 64).
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
