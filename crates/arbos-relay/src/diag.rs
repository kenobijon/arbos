//! Recent relay warnings in memory, per account (`GET /me/relay-errors`). A `tracing` layer
//! keeps the last `RING` warn/error events with their fields; a reader gets the lines whose
//! fields name one of its own identifiers (user id, device ids, project ids, chat ids / rooms),
//! so one account never sees another's trouble. The admin sees every line. No log shipper, no
//! file: a restart empties it (which `healthz.uptime_s` already tells you).

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{json, Value};
use tracing::field::{Field, Visit};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use arbos_core::ids::now_ms;
use arbos_core::redact::scrub_value;

const RING: usize = 2_000;

fn ring() -> &'static Mutex<VecDeque<Value>> {
    static R: std::sync::OnceLock<Mutex<VecDeque<Value>>> = std::sync::OnceLock::new();
    R.get_or_init(|| Mutex::new(VecDeque::with_capacity(RING)))
}

pub struct WarnRing;

struct Collect {
    message: String,
    fields: serde_json::Map<String, Value>,
}

impl Visit for Collect {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let s = format!("{value:?}");
        if field.name() == "message" {
            self.message = s;
        } else {
            self.fields.insert(field.name().to_owned(), json!(s));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.fields.insert(field.name().to_owned(), json!(value));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name().to_owned(), json!(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name().to_owned(), json!(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().to_owned(), json!(value));
    }
}

impl<S: Subscriber> Layer<S> for WarnRing {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > tracing::Level::WARN {
            return;
        }
        let mut c = Collect {
            message: String::new(),
            fields: serde_json::Map::new(),
        };
        event.record(&mut c);
        push(json!({
            "at": now_ms(),
            "level": if level == tracing::Level::ERROR { "error" } else { "warn" },
            "target": event.metadata().target(),
            "message": c.message,
            "fields": Value::Object(c.fields),
        }));
    }
}

fn push(entry: Value) {
    if let Ok(mut r) = ring().lock() {
        if r.len() >= RING {
            r.pop_front();
        }
        r.push_back(entry);
    }
}

/// True when any field value (or the message) contains one of `ids`.
fn mentions(entry: &Value, ids: &[String]) -> bool {
    let hay = |s: &str| {
        ids.iter()
            .any(|id| !id.is_empty() && s.contains(id.as_str()))
    };
    if entry["message"].as_str().is_some_and(hay) {
        return true;
    }
    entry["fields"]
        .as_object()
        .map(|f| {
            f.values().any(|v| match v {
                Value::String(s) => hay(s),
                other => hay(&other.to_string()),
            })
        })
        .unwrap_or(false)
}

/// Lines after `since` that name one of `ids` (`all` = every line, admin), oldest first,
/// scrubbed, at most `limit`.
pub fn recent(since: i64, ids: &[String], all: bool, limit: usize) -> Vec<Value> {
    let Ok(r) = ring().lock() else {
        return Vec::new();
    };
    r.iter()
        .filter(|e| e["at"].as_i64().unwrap_or(0) > since)
        .filter(|e| all || mentions(e, ids))
        .take(limit)
        .cloned()
        .map(|mut e| {
            scrub_value(&mut e);
            e
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lines_naming_the_caller_come_back() {
        push(
            json!({ "at": 10, "level": "warn", "target": "t", "message": "box unreachable", "fields": { "user": "u_alice" } }),
        );
        push(
            json!({ "at": 11, "level": "error", "target": "t", "message": "room /room/people/CHAT1 closed", "fields": { "code": 1012 } }),
        );
        push(
            json!({ "at": 12, "level": "warn", "target": "t", "message": "token ghp_abcdefghijklmnopqrstuvwxyz", "fields": { "user": "u_bob" } }),
        );
        let alice = recent(5, &["u_alice".into(), "CHAT1".into()], false, 100);
        assert_eq!(alice.len(), 2, "{alice:?}");
        assert!(alice.iter().all(|e| !e.to_string().contains("u_bob")));
        let bob = recent(5, &["u_bob".into()], false, 100);
        assert_eq!(bob.len(), 1);
        assert!(bob[0]["message"]
            .as_str()
            .unwrap()
            .contains("[REDACTED:token]"));
        let none = recent(5, &[String::new()], false, 100);
        assert!(none.is_empty(), "an empty id must not match everything");
        assert!(recent(5, &[], true, 100).len() >= 3);
    }
}
