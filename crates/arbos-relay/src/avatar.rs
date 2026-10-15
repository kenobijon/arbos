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

/// Avatar colour palette (manifest `colors[]`), in order.
pub const DEFAULT_COLORS: &[&str] = &[
    "mono", "red", "orange", "amber", "green", "teal", "cyan", "blue", "indigo", "violet", "pink",
    "rose", "slate",
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

/// Default colour for a new project: `colors[fnv1a64(project_id + ":color") % n]`.
pub fn default_color_for(project_id: &str, colors: &[String]) -> String {
    let list: Vec<String> = if colors.is_empty() {
        DEFAULT_COLORS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        colors.to_vec()
    };
    list[(fnv1a64(&format!("{project_id}:color")) % list.len() as u64) as usize].clone()
}

/// Default `(tree, color)` for a new project, deduped against the user's existing pairs: start
/// at the hash picks; probe forward on the colour first (same tree, next colour), then on the
/// tree, until the pair is unused; when every pair is taken, keep the hash picks. Two agents may
/// share a tree only when their colours differ.
pub fn default_pair(
    project_id: &str,
    slugs: &[String],
    colors: &[String],
    taken: &[(String, String)],
) -> (String, String) {
    let trees: Vec<String> = if slugs.is_empty() {
        DEFAULT_SLUGS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        slugs.to_vec()
    };
    let palette: Vec<String> = if colors.is_empty() {
        DEFAULT_COLORS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        colors.to_vec()
    };
    let t0 = (fnv1a64(project_id) % trees.len() as u64) as usize;
    let c0 = (fnv1a64(&format!("{project_id}:color")) % palette.len() as u64) as usize;
    for ti in 0..trees.len() {
        let tree = &trees[(t0 + ti) % trees.len()];
        for ci in 0..palette.len() {
            let color = &palette[(c0 + ci) % palette.len()];
            if !taken.iter().any(|(t, c)| t == tree && c == color) {
                return (tree.clone(), color.clone());
            }
        }
    }
    (trees[t0].clone(), palette[c0].clone())
}

/// Colour list from the manifest file (`colors[]`), `ARBOS_AVATAR_COLORS`, else the default.
pub fn colors_from_env() -> Vec<String> {
    if let Ok(path) = std::env::var("ARBOS_AVATAR_MANIFEST") {
        if let Ok(text) = std::fs::read_to_string(path.trim()) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                let colors: Vec<String> = v
                    .get("colors")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| {
                                e.as_str().map(str::to_owned).or_else(|| {
                                    e.get("slug").and_then(Value::as_str).map(str::to_owned)
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if !colors.is_empty() {
                    return colors;
                }
            }
        }
    }
    if let Ok(list) = std::env::var("ARBOS_AVATAR_COLORS") {
        let colors: Vec<String> = list
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if !colors.is_empty() {
            return colors;
        }
    }
    DEFAULT_COLORS.iter().map(|s| (*s).to_owned()).collect()
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
    fn default_pair_dedupes_on_color_first() {
        let trees: Vec<String> = DEFAULT_SLUGS.iter().map(|s| (*s).to_owned()).collect();
        let colors: Vec<String> = DEFAULT_COLORS.iter().map(|s| (*s).to_owned()).collect();
        let (t, c) = default_pair("P9", &trees, &colors, &[]);
        assert_eq!(t, default_for("P9", &trees));
        assert_eq!(c, default_color_for("P9", &colors));
        // Same tree taken with that colour: the tree stays, the colour moves on.
        let (t2, c2) = default_pair("P9", &trees, &colors, &[(t.clone(), c.clone())]);
        assert_eq!(t2, t);
        let ci = colors.iter().position(|x| *x == c).unwrap();
        assert_eq!(c2, colors[(ci + 1) % colors.len()]);
        // Every colour of that tree taken: the next tree.
        let all: Vec<(String, String)> = colors.iter().map(|c| (t.clone(), c.clone())).collect();
        let (t3, _) = default_pair("P9", &trees, &colors, &all);
        let ti = trees.iter().position(|x| *x == t).unwrap();
        assert_eq!(t3, trees[(ti + 1) % trees.len()]);
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
