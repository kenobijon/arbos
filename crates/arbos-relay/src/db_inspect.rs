//! Relay-side rows a debugging reader merges with the box's window (`inspect.rs`): model usage,
//! voice-call membership, routine runs and the mirrored agentd audit. Every query is bounded by
//! `at > after_ms` and `LIMIT`, on an indexed column, oldest first.

use anyhow::Result;
use rusqlite::params;
use serde_json::{json, Value};

use crate::db::Db;

impl Db {
    /// Metered model calls of a project after `after_ms` (`usage` table): api, model, tokens,
    /// cost and latency — the spend side of every turn.
    pub fn usage_rows_after(
        &self,
        project_id: &str,
        after_ms: i64,
        limit: i64,
    ) -> Result<Vec<Value>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, at, api, model, input, output, cache_read, cache_write, usd, latency_ms, ttfb_ms
                 FROM usage WHERE project_id = ?1 AND at > ?2 ORDER BY at ASC, id ASC LIMIT ?3",
            )?;
            let rows = st.query_map(params![project_id, after_ms, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "api": r.get::<_, String>(2)?,
                    "model": r.get::<_, String>(3)?, "input": r.get::<_, i64>(4)?, "output": r.get::<_, i64>(5)?,
                    "cache_read": r.get::<_, i64>(6)?, "cache_write": r.get::<_, i64>(7)?, "usd": r.get::<_, f64>(8)?,
                    "latency_ms": r.get::<_, i64>(9)?, "ttfb_ms": r.get::<_, Option<i64>>(10)?,
                }))
            })?;
            rows.collect()
        })
    }

    /// Calls an agent joined after `after_ms` (`call_agents` ⋈ `calls`): when it joined and left,
    /// which chat, who started the call (#1436 group-call subscribe).
    pub fn call_agent_rows_after(
        &self,
        project_id: &str,
        after_ms: i64,
        limit: i64,
    ) -> Result<Vec<Value>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT a.call_id, a.joined_at, a.left_at, c.chat_id, c.kind, c.started_by, c.started_at, c.ended_at, c.end_reason
                 FROM call_agents a LEFT JOIN calls c ON c.id = a.call_id
                 WHERE a.project_id = ?1 AND a.joined_at > ?2 ORDER BY a.joined_at ASC LIMIT ?3",
            )?;
            let rows = st.query_map(params![project_id, after_ms, limit], |r| {
                Ok(json!({
                    "call_id": r.get::<_, String>(0)?, "joined_at": r.get::<_, i64>(1)?, "left_at": r.get::<_, Option<i64>>(2)?,
                    "chat_id": r.get::<_, Option<String>>(3)?, "kind": r.get::<_, Option<String>>(4)?,
                    "started_by": r.get::<_, Option<String>>(5)?, "started_at": r.get::<_, Option<i64>>(6)?,
                    "ended_at": r.get::<_, Option<i64>>(7)?, "end_reason": r.get::<_, Option<String>>(8)?,
                }))
            })?;
            rows.collect()
        })
    }

    /// Routine runs of a project after `after_ms` (`routine_runs` ⋈ `routines`).
    pub fn routine_run_rows_after(
        &self,
        project_id: &str,
        after_ms: i64,
        limit: i64,
    ) -> Result<Vec<Value>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT r.id, r.routine_id, t.name, t.chat_id, r.started_at, r.finished_at, r.status, r.message_id, r.note
                 FROM routine_runs r JOIN routines t ON t.id = r.routine_id
                 WHERE t.project_id = ?1 AND r.started_at > ?2 ORDER BY r.started_at ASC LIMIT ?3",
            )?;
            let rows = st.query_map(params![project_id, after_ms, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?, "routine_id": r.get::<_, String>(1)?, "routine": r.get::<_, String>(2)?,
                    "chat_id": r.get::<_, String>(3)?, "started_at": r.get::<_, i64>(4)?, "finished_at": r.get::<_, Option<i64>>(5)?,
                    "status": r.get::<_, String>(6)?, "message_id": r.get::<_, Option<String>>(7)?, "note": r.get::<_, Option<String>>(8)?,
                }))
            })?;
            rows.collect()
        })
    }

    /// The agentd audit entries the box mirrored to the relay (`POST /projects/{id}/audit`) after
    /// `after_ms` — the fallback when the box itself cannot be reached.
    pub fn audit_rows_after(
        &self,
        project_id: &str,
        after_ms: i64,
        limit: i64,
    ) -> Result<Vec<Value>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, kind, entry, at FROM audit WHERE project_id = ?1 AND at > ?2 ORDER BY at ASC, id ASC LIMIT ?3",
            )?;
            let rows = st.query_map(params![project_id, after_ms, limit], |r| {
                let raw: String = r.get(2)?;
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?,
                    "entry": serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null), "at": r.get::<_, i64>(3)?,
                }))
            })?;
            rows.collect()
        })
    }
}
