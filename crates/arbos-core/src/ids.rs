//! ULIDs for every entity (design §5) and millisecond UTC timestamps (D48).

use ulid::Ulid;

/// A fresh, time-sortable 26-character id.
pub fn ulid() -> String {
    Ulid::new().to_string()
}

/// Current time in UTC milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// True when `s` parses as a ULID.
pub fn is_ulid(s: &str) -> bool {
    Ulid::from_string(s).is_ok()
}
