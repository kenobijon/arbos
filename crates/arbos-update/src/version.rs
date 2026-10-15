//! Build versions are dotted integers: `0.1.0` from `Cargo.toml`, with the
//! commit count behind it in CI (`0.1.0.212`). Comparison is numeric per
//! component; a missing component counts as 0, so `0.1.0` < `0.1.0.1`.

use std::cmp::Ordering;

/// A parsed dotted version. Anything that is not a decimal component (a
/// `-beta` suffix, say) is ignored from that point on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version(pub Vec<u64>);

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim().trim_start_matches('v');
        let mut parts = Vec::new();
        for component in text.split('.') {
            let digits: String = component
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if digits.is_empty() {
                break;
            }
            parts.push(digits.parse().ok()?);
            if digits.len() != component.len() {
                break;
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(Version(parts))
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.0.len().max(other.0.len());
        for i in 0..n {
            let a = self.0.get(i).copied().unwrap_or(0);
            let b = other.0.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => continue,
                other => return other,
            }
        }
        Ordering::Equal
    }
}

/// `true` when `candidate` is strictly newer than `current`. Unparseable
/// strings never count as newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}
