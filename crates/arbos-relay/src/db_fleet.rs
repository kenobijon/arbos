//! Storage for the host fleet (D89): the sign-up waitlist, runtime fleet settings, and the
//! eviction/admission event log. Same SQLite file as `db.rs`; created from `Db::open` through
//! `FLEET_SCHEMA`.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use arbos_core::ids::now_ms;

use crate::db::Db;

pub const FLEET_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS waitlist (
  user_id TEXT PRIMARY KEY, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS fleet_settings (
  key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS fleet_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, kind TEXT NOT NULL,
  user_id TEXT, detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS fleet_events_at ON fleet_events(at);
"#;

#[derive(Debug, Clone, Serialize)]
pub struct WaitlistEntry {
    pub user_id: String,
    pub created_at: i64,
    /// 1-based place in line (oldest first).
    pub position: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetEvent {
    pub id: i64,
    pub at: i64,
    pub kind: String,
    pub user_id: Option<String>,
    pub detail: String,
}

impl Db {
    /// Put a user in line (idempotent: an existing entry keeps its place).
    pub fn waitlist_add(&self, user_id: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO waitlist(user_id, created_at) VALUES (?1, ?2)",
                params![user_id, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn waitlist_remove(&self, user_id: &str) -> Result<bool> {
        self.with(|c| {
            c.execute("DELETE FROM waitlist WHERE user_id = ?1", params![user_id])
                .map(|n| n > 0)
        })
    }

    pub fn is_waitlisted(&self, user_id: &str) -> Result<bool> {
        self.with(|c| {
            c.query_row(
                "SELECT 1 FROM waitlist WHERE user_id = ?1",
                params![user_id],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
        })
    }

    /// Everyone in line, oldest first, with their 1-based position.
    pub fn waitlist(&self) -> Result<Vec<WaitlistEntry>> {
        self.with(|c| {
            let mut st =
                c.prepare("SELECT user_id, created_at FROM waitlist ORDER BY created_at, user_id")?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            let mut out = Vec::new();
            for (i, r) in rows.enumerate() {
                let (user_id, created_at) = r?;
                out.push(WaitlistEntry {
                    user_id,
                    created_at,
                    position: i + 1,
                });
            }
            Ok(out)
        })
    }

    pub fn waitlist_position(&self, user_id: &str) -> Result<Option<usize>> {
        Ok(self
            .waitlist()?
            .into_iter()
            .find(|e| e.user_id == user_id)
            .map(|e| e.position))
    }

    pub fn fleet_setting(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row(
                "SELECT value FROM fleet_settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
        })
    }

    pub fn set_fleet_setting(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO fleet_settings(key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn fleet_event(&self, kind: &str, user_id: Option<&str>, detail: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO fleet_events(at, kind, user_id, detail) VALUES (?1, ?2, ?3, ?4)",
                params![now_ms(), kind, user_id, detail],
            )
            .map(|_| ())
        })
    }

    /// Newest first.
    pub fn fleet_events(&self, limit: usize) -> Result<Vec<FleetEvent>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, at, kind, user_id, detail FROM fleet_events ORDER BY at DESC, id DESC LIMIT ?1",
            )?;
            let rows = st.query_map(params![limit as i64], |r| {
                Ok(FleetEvent {
                    id: r.get(0)?,
                    at: r.get(1)?,
                    kind: r.get(2)?,
                    user_id: r.get(3)?,
                    detail: r.get(4)?,
                })
            })?;
            rows.collect()
        })
    }

    /// How many boxes a provider holds (resident count for the host gate).
    pub fn count_boxes_of_provider(&self, provider: &str) -> Result<usize> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM boxes WHERE provider = ?1",
                params![provider],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as usize)
        })
    }
}
