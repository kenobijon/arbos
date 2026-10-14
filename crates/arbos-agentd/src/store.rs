//! Box storage: SQLite + FTS5 (design §5, §6.3, §6.7; memory-decision-models §5.6).
//! T3 history is append-only and searchable; memory is never hard-deleted (D7).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- Room cursor + snapshot committed together (crdt-sync §2.3).
CREATE TABLE IF NOT EXISTS room_docs (
  room TEXT PRIMARY KEY, cursor INTEGER NOT NULL DEFAULT 0, snapshot BLOB, acked_vv BLOB, updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS chats (
  id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT 'New chat', is_pinned INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, last_message_at INTEGER,
  compaction_summary TEXT, compacted_through TEXT, compaction_version INTEGER NOT NULL DEFAULT 0,
  pi_session_path TEXT
);

CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY, subagent_id TEXT, path TEXT, cwd TEXT, model TEXT, thinking_level TEXT,
  last_cursor TEXT, compactions INTEGER NOT NULL DEFAULT 0, context_pct INTEGER NOT NULL DEFAULT 0,
  harness TEXT NOT NULL, role TEXT NOT NULL, harness_version TEXT, created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS subagents (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, purpose TEXT NOT NULL, summary TEXT NOT NULL DEFAULT '',
  tags TEXT NOT NULL DEFAULT '[]', last_seen_at INTEGER, status TEXT NOT NULL DEFAULT 'idle',
  turns INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, last_turn_at INTEGER, retired_reason TEXT,
  harness TEXT NOT NULL, handles TEXT NOT NULL DEFAULT '[]', chat_id TEXT
);

-- T3: one row per message / tool call / tool output / compaction / memory event.
CREATE TABLE IF NOT EXISTS t3 (
  id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, subagent_id TEXT, session_id TEXT, kind TEXT NOT NULL,
  role TEXT, at INTEGER NOT NULL, text TEXT NOT NULL, source TEXT, size INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS t3_thread_at ON t3(thread_id, at);
CREATE VIRTUAL TABLE IF NOT EXISTS t3_fts USING fts5(text, id UNINDEXED, thread_id UNINDEXED, kind UNINDEXED, tokenize = 'porter unicode61');

CREATE TABLE IF NOT EXISTS memory (
  id TEXT PRIMARY KEY, tier TEXT NOT NULL, thread_id TEXT, kind TEXT NOT NULL, scope TEXT NOT NULL,
  text TEXT NOT NULL, source TEXT, valid_at INTEGER, invalid_at INTEGER, created_at INTEGER NOT NULL, expired_at INTEGER,
  supersedes TEXT NOT NULL DEFAULT '[]', confidence REAL NOT NULL DEFAULT 0.5, importance INTEGER NOT NULL DEFAULT 5,
  last_accessed_at INTEGER, access_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS memory_tier_live ON memory(tier, expired_at);
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(text, id UNINDEXED, tier UNINDEXED, tokenize = 'porter unicode61');

CREATE TABLE IF NOT EXISTS memory_decision_log (
  id TEXT PRIMARY KEY, side TEXT NOT NULL, event_id TEXT, gate TEXT, action TEXT, target_ids TEXT,
  reason TEXT, confidence REAL, model TEXT, latency_ms INTEGER, usage TEXT, created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS handles (
  id TEXT PRIMARY KEY, chat_id TEXT, session_id TEXT, kind TEXT NOT NULL, chars INTEGER NOT NULL, lines INTEGER NOT NULL,
  path TEXT NOT NULL, note TEXT NOT NULL, blob_key TEXT, created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS scratch (
  chat_id TEXT NOT NULL, name TEXT NOT NULL, value TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (chat_id, name)
);

CREATE TABLE IF NOT EXISTS turn_metrics (
  id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, message_id TEXT NOT NULL, route TEXT, ttft_ms INTEGER, route_ms INTEGER,
  total_ms INTEGER, tool_calls INTEGER NOT NULL DEFAULT 0, escalated INTEGER NOT NULL DEFAULT 0, usage TEXT, created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS asks (
  id TEXT PRIMARY KEY, chat_id TEXT, session_id TEXT, question TEXT NOT NULL, answered INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRow {
    pub id: String,
    pub title: String,
    pub is_pinned: bool,
    pub created_at: i64,
    pub compaction_summary: Option<String>,
    pub compacted_through: Option<String>,
    pub compaction_version: i64,
    pub pi_session_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentRow {
    pub id: String,
    pub name: String,
    pub purpose: String,
    pub summary: String,
    pub tags: Value,
    pub last_seen_at: Option<i64>,
    pub status: String,
    pub turns: i64,
    pub created_at: i64,
    pub last_turn_at: Option<i64>,
    pub harness: String,
    pub chat_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct T3Row {
    pub id: String,
    pub thread_id: String,
    pub subagent_id: Option<String>,
    pub session_id: Option<String>,
    pub kind: String,
    pub role: Option<String>,
    pub at: i64,
    pub text: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRow {
    pub id: String,
    pub tier: String,
    pub thread_id: Option<String>,
    pub kind: String,
    pub scope: String,
    pub text: String,
    pub valid_at: Option<i64>,
    pub invalid_at: Option<i64>,
    pub created_at: i64,
    pub expired_at: Option<i64>,
    pub supersedes: Value,
    pub confidence: f64,
    pub importance: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandleRow {
    pub id: String,
    pub chat_id: Option<String>,
    pub kind: String,
    pub chars: i64,
    pub lines: i64,
    pub path: String,
    pub note: String,
    pub created_at: i64,
}

/// Write-time redaction before T3 append (D19): tokens, keys, Authorization headers, `.env`-style values.
pub fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    let patterns: [(&str, &str); 6] = [
        (r"sk-or-v1-[A-Za-z0-9]{20,}", "[REDACTED:openrouter-key]"),
        (r"sk-ant-[A-Za-z0-9_\-]{20,}", "[REDACTED:anthropic-key]"),
        (r"sk-[A-Za-z0-9]{32,}", "[REDACTED:openai-key]"),
        (r"dtn_[A-Za-z0-9]{20,}", "[REDACTED:daytona-key]"),
        (
            r"(?i)(authorization:\s*bearer\s+)[A-Za-z0-9._\-]+",
            "${1}[REDACTED:bearer]",
        ),
        (
            r"(?i)\b(gh[pousr]_[A-Za-z0-9]{20,}|xox[baprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16})\b",
            "[REDACTED:token]",
        ),
    ];
    for (pat, rep) in patterns {
        if let Ok(re) = regex_lite::Regex::new(pat) {
            out = re.replace_all(&out, rep).into_owned();
        }
    }
    // KEY=value lines for well-known secret names.
    if let Ok(re) = regex_lite::Regex::new(
        r"(?im)^(\s*(?:export\s+)?[A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD)[A-Z0-9_]*\s*=\s*)\S+",
    ) {
        out = re.replace_all(&out, "${1}[REDACTED:env]").into_owned();
    }
    out
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let c = Connection::open(path)?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        c.execute_batch(SCHEMA)?;
        Ok(Self(Arc::new(Mutex::new(c))))
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let c = self.0.lock().expect("store lock");
        Ok(f(&c)?)
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
        })
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| c.execute("INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value]).map(|_| ()))
    }

    // ---- room docs ----------------------------------------------------------------------------

    pub fn room_doc(&self, room: &str) -> Result<Option<(i64, Option<Vec<u8>>, Option<Vec<u8>>)>> {
        self.with(|c| {
            c.query_row(
                "SELECT cursor, snapshot, acked_vv FROM room_docs WHERE room = ?1",
                params![room],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })
    }

    /// Cursor and snapshot land in one transaction so a restored box never disagrees with its cursor.
    pub fn save_room_doc(
        &self,
        room: &str,
        cursor: i64,
        snapshot: &[u8],
        acked_vv: &[u8],
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO room_docs(room, cursor, snapshot, acked_vv, updated_at) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(room) DO UPDATE SET cursor=excluded.cursor, snapshot=excluded.snapshot, acked_vv=excluded.acked_vv, updated_at=excluded.updated_at",
                params![room, cursor, snapshot, acked_vv, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn save_room_cursor(&self, room: &str, cursor: i64) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO room_docs(room, cursor, updated_at) VALUES (?1,?2,?3) ON CONFLICT(room) DO UPDATE SET cursor = excluded.cursor, updated_at = excluded.updated_at",
                params![room, cursor, now_ms()],
            )
            .map(|_| ())
        })
    }

    // ---- chats --------------------------------------------------------------------------------

    pub fn upsert_chat(
        &self,
        id: &str,
        title: &str,
        is_pinned: bool,
        created_at: i64,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO chats(id, title, is_pinned, created_at) VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET title = excluded.title, is_pinned = excluded.is_pinned",
                params![id, title, is_pinned as i64, created_at],
            )
            .map(|_| ())
        })
    }

    pub fn chats(&self) -> Result<Vec<ChatRow>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id, title, is_pinned, created_at, compaction_summary, compacted_through, compaction_version, pi_session_path FROM chats ORDER BY created_at")?;
            let rows = st.query_map([], |r| {
                Ok(ChatRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    is_pinned: r.get::<_, i64>(2)? != 0,
                    created_at: r.get(3)?,
                    compaction_summary: r.get(4)?,
                    compacted_through: r.get(5)?,
                    compaction_version: r.get(6)?,
                    pi_session_path: r.get(7)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn chat(&self, id: &str) -> Result<Option<ChatRow>> {
        Ok(self.chats()?.into_iter().find(|c| c.id == id))
    }

    pub fn set_chat_session_path(&self, id: &str, path: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE chats SET pi_session_path = ?2 WHERE id = ?1",
                params![id, path],
            )
            .map(|_| ())
        })
    }

    pub fn set_chat_compaction(&self, id: &str, summary: &str, through: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE chats SET compaction_summary = ?2, compacted_through = ?3, compaction_version = compaction_version + 1 WHERE id = ?1",
                params![id, summary, through],
            )
            .map(|_| ())
        })
    }

    pub fn touch_chat(&self, id: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE chats SET last_message_at = ?2 WHERE id = ?1",
                params![id, now_ms()],
            )
            .map(|_| ())
        })
    }

    // ---- sessions and subagents ---------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_session(
        &self,
        id: &str,
        subagent_id: Option<&str>,
        path: Option<&str>,
        cwd: &str,
        model: &str,
        harness: &str,
        role: &str,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO sessions(id, subagent_id, path, cwd, model, harness, role, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(id) DO UPDATE SET path = COALESCE(excluded.path, sessions.path), model = excluded.model",
                params![id, subagent_id, path, cwd, model, harness, role, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn session_cursor(&self, id: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row(
                "SELECT last_cursor FROM sessions WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map(|o: Option<Option<String>>| o.flatten())
        })
    }

    pub fn set_session_cursor(&self, id: &str, cursor: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE sessions SET last_cursor = ?2 WHERE id = ?1",
                params![id, cursor],
            )
            .map(|_| ())
        })
    }

    pub fn set_session_path(&self, id: &str, path: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE sessions SET path = ?2 WHERE id = ?1",
                params![id, path],
            )
            .map(|_| ())
        })
    }

    pub fn session_path(&self, id: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row(
                "SELECT path FROM sessions WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map(|o: Option<Option<String>>| o.flatten())
        })
    }

    pub fn bump_session_compactions(&self, id: &str) -> Result<i64> {
        self.with(|c| {
            c.execute(
                "UPDATE sessions SET compactions = compactions + 1 WHERE id = ?1",
                params![id],
            )?;
            c.query_row(
                "SELECT compactions FROM sessions WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
        })
    }

    pub fn insert_subagent(
        &self,
        id: &str,
        name: &str,
        purpose: &str,
        harness: &str,
        chat_id: &str,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO subagents(id, name, purpose, harness, chat_id, created_at, status) VALUES (?1,?2,?3,?4,?5,?6,'idle')",
                params![id, name, purpose, harness, chat_id, now_ms()],
            )
            .map(|_| ())
        })
    }

    fn row_to_subagent(r: &rusqlite::Row<'_>) -> rusqlite::Result<SubagentRow> {
        Ok(SubagentRow {
            id: r.get(0)?,
            name: r.get(1)?,
            purpose: r.get(2)?,
            summary: r.get(3)?,
            tags: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(json!([])),
            last_seen_at: r.get(5)?,
            status: r.get(6)?,
            turns: r.get(7)?,
            created_at: r.get(8)?,
            last_turn_at: r.get(9)?,
            harness: r.get(10)?,
            chat_id: r.get(11)?,
        })
    }

    const SUBAGENT_COLS: &'static str = "id, name, purpose, summary, tags, last_seen_at, status, turns, created_at, last_turn_at, harness, chat_id";

    pub fn subagents(&self) -> Result<Vec<SubagentRow>> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {} FROM subagents ORDER BY COALESCE(last_turn_at, created_at) DESC",
                Self::SUBAGENT_COLS
            ))?;
            let rows = st.query_map([], Self::row_to_subagent)?;
            rows.collect()
        })
    }

    pub fn subagent(&self, id: &str) -> Result<Option<SubagentRow>> {
        self.with(|c| {
            c.query_row(
                &format!(
                    "SELECT {} FROM subagents WHERE id = ?1",
                    Self::SUBAGENT_COLS
                ),
                params![id],
                Self::row_to_subagent,
            )
            .optional()
        })
    }

    pub fn set_subagent_status(&self, id: &str, status: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE subagents SET status = ?2, last_turn_at = CASE WHEN ?2 = 'running' THEN ?3 ELSE last_turn_at END,
                        turns = turns + CASE WHEN ?2 = 'running' THEN 1 ELSE 0 END, last_seen_at = ?3 WHERE id = ?1",
                params![id, status, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn set_subagent_summary(&self, id: &str, summary: &str, tags: &Value) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE subagents SET summary = ?2, tags = ?3 WHERE id = ?1",
                params![id, summary, tags.to_string()],
            )
            .map(|_| ())
        })
    }

    // ---- T3 -----------------------------------------------------------------------------------

    /// Append one T3 entry (redacted at write time, D19). Returns the entry id.
    #[allow(clippy::too_many_arguments)]
    pub fn t3_append(
        &self,
        thread_id: &str,
        subagent_id: Option<&str>,
        session_id: Option<&str>,
        kind: &str,
        role: Option<&str>,
        text: &str,
        source: Option<&Value>,
    ) -> Result<String> {
        let id = ulid();
        let text = redact(text);
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO t3(id, thread_id, subagent_id, session_id, kind, role, at, text, source, size) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![id, thread_id, subagent_id, session_id, kind, role, now, text, source.map(|s| s.to_string()), text.len() as i64],
            )?;
            c.execute("INSERT INTO t3_fts(text, id, thread_id, kind) VALUES (?1,?2,?3,?4)", params![text, id, thread_id, kind])?;
            Ok(())
        })?;
        Ok(id)
    }

    pub fn t3_search(
        &self,
        query: &str,
        thread_id: Option<&str>,
        kind: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<(T3Row, f64, String)>> {
        let q = fts_query(query);
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT t.id, t.thread_id, t.subagent_id, t.session_id, t.kind, t.role, t.at, t.text, t.size, bm25(t3_fts) AS score,
                        snippet(t3_fts, 0, '[', ']', '…', 24)
                 FROM t3_fts JOIN t3 t ON t.id = t3_fts.id
                 WHERE t3_fts MATCH ?1 AND (?2 IS NULL OR t.thread_id = ?2) AND (?3 IS NULL OR t.kind = ?3)
                 ORDER BY score LIMIT ?4 OFFSET ?5",
            )?;
            let rows = st.query_map(params![q, thread_id, kind, limit, offset], |r| {
                Ok((
                    T3Row {
                        id: r.get(0)?,
                        thread_id: r.get(1)?,
                        subagent_id: r.get(2)?,
                        session_id: r.get(3)?,
                        kind: r.get(4)?,
                        role: r.get(5)?,
                        at: r.get(6)?,
                        text: r.get(7)?,
                        size: r.get(8)?,
                    },
                    r.get::<_, f64>(9)?,
                    r.get::<_, String>(10)?,
                ))
            })?;
            rows.collect()
        })
    }

    pub fn t3_get(&self, id: &str) -> Result<Option<T3Row>> {
        self.with(|c| {
            c.query_row("SELECT id, thread_id, subagent_id, session_id, kind, role, at, text, size FROM t3 WHERE id = ?1", params![id], |r| {
                Ok(T3Row { id: r.get(0)?, thread_id: r.get(1)?, subagent_id: r.get(2)?, session_id: r.get(3)?, kind: r.get(4)?, role: r.get(5)?, at: r.get(6)?, text: r.get(7)?, size: r.get(8)? })
            })
            .optional()
        })
    }

    /// Verbatim turns of a thread in order; `from_id`/`to_id` bound the range, `limit` from the end when unbounded.
    pub fn t3_thread(
        &self,
        thread_id: &str,
        from_id: Option<&str>,
        to_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<T3Row>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, thread_id, subagent_id, session_id, kind, role, at, text, size FROM (
                    SELECT * FROM t3 WHERE thread_id = ?1 AND (?2 IS NULL OR id >= ?2) AND (?3 IS NULL OR id <= ?3) ORDER BY id DESC LIMIT ?4
                 ) ORDER BY id ASC",
            )?;
            let rows = st.query_map(params![thread_id, from_id, to_id, limit], |r| {
                Ok(T3Row { id: r.get(0)?, thread_id: r.get(1)?, subagent_id: r.get(2)?, session_id: r.get(3)?, kind: r.get(4)?, role: r.get(5)?, at: r.get(6)?, text: r.get(7)?, size: r.get(8)? })
            })?;
            rows.collect()
        })
    }

    // ---- memory -------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn memory_add(
        &self,
        tier: &str,
        thread_id: Option<&str>,
        kind: &str,
        scope: &str,
        text: &str,
        source: Option<&Value>,
        valid_at: Option<i64>,
        confidence: f64,
        importance: i64,
        supersedes: &[String],
    ) -> Result<String> {
        let id = ulid();
        let text = redact(text);
        self.with(|c| {
            c.execute(
                "INSERT INTO memory(id, tier, thread_id, kind, scope, text, source, valid_at, created_at, supersedes, confidence, importance) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![id, tier, thread_id, kind, scope, text, source.map(|s| s.to_string()), valid_at, now_ms(), serde_json::to_string(supersedes).unwrap_or_default(), confidence, importance],
            )?;
            c.execute("INSERT INTO memory_fts(text, id, tier) VALUES (?1,?2,?3)", params![text, id, tier])?;
            Ok(())
        })?;
        Ok(id)
    }

    /// INVALIDATE: set `invalid_at` and `expired_at`; the row stays (D7).
    pub fn memory_invalidate(&self, id: &str, invalid_at: Option<i64>) -> Result<()> {
        let now = now_ms();
        self.with(|c| c.execute("UPDATE memory SET invalid_at = COALESCE(?2, ?3), expired_at = ?3 WHERE id = ?1 AND expired_at IS NULL", params![id, invalid_at, now]).map(|_| ()))
    }

    fn row_to_memory(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRow> {
        Ok(MemoryRow {
            id: r.get(0)?,
            tier: r.get(1)?,
            thread_id: r.get(2)?,
            kind: r.get(3)?,
            scope: r.get(4)?,
            text: r.get(5)?,
            valid_at: r.get(6)?,
            invalid_at: r.get(7)?,
            created_at: r.get(8)?,
            expired_at: r.get(9)?,
            supersedes: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or(json!([])),
            confidence: r.get(11)?,
            importance: r.get(12)?,
        })
    }

    const MEMORY_COLS: &'static str = "m.id, m.tier, m.thread_id, m.kind, m.scope, m.text, m.valid_at, m.invalid_at, m.created_at, m.expired_at, m.supersedes, m.confidence, m.importance";

    pub fn memory_search(
        &self,
        query: &str,
        tiers: &[String],
        include_expired: bool,
        limit: i64,
    ) -> Result<Vec<(MemoryRow, f64)>> {
        let q = fts_query(query);
        let tiers_csv = if tiers.is_empty() {
            None
        } else {
            Some(tiers.join(","))
        };
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {}, bm25(memory_fts) FROM memory_fts JOIN memory m ON m.id = memory_fts.id
                 WHERE memory_fts MATCH ?1 AND (?2 OR m.expired_at IS NULL) AND (?3 IS NULL OR instr(?3, m.tier) > 0)
                 ORDER BY bm25(memory_fts) LIMIT ?4",
                Self::MEMORY_COLS
            ))?;
            let rows = st.query_map(params![q, include_expired, tiers_csv, limit], |r| Ok((Self::row_to_memory(r)?, r.get::<_, f64>(13)?)))?;
            rows.collect()
        })
    }

    pub fn memory_live(
        &self,
        tier: &str,
        thread_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<MemoryRow>> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {} FROM memory m WHERE m.tier = ?1 AND m.expired_at IS NULL AND (?2 IS NULL OR m.thread_id = ?2) ORDER BY m.importance DESC, m.created_at DESC LIMIT ?3",
                Self::MEMORY_COLS
            ))?;
            let rows = st.query_map(params![tier, thread_id, limit], Self::row_to_memory)?;
            rows.collect()
        })
    }

    pub fn memory_by_ids(&self, ids: &[String]) -> Result<Vec<MemoryRow>> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(m) = self.with(|c| {
                c.query_row(
                    &format!("SELECT {} FROM memory m WHERE m.id = ?1", Self::MEMORY_COLS),
                    params![id],
                    Self::row_to_memory,
                )
                .optional()
            })? {
                out.push(m);
            }
        }
        Ok(out)
    }

    pub fn memory_touch(&self, ids: &[String]) -> Result<()> {
        self.with(|c| {
            for id in ids {
                c.execute("UPDATE memory SET last_accessed_at = ?2, access_count = access_count + 1 WHERE id = ?1", params![id, now_ms()])?;
            }
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_decision(
        &self,
        side: &str,
        event_id: Option<&str>,
        gate: Option<&Value>,
        action: Option<&str>,
        target_ids: &[String],
        reason: Option<&str>,
        confidence: Option<f64>,
        model: Option<&str>,
        latency_ms: Option<i64>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO memory_decision_log(id, side, event_id, gate, action, target_ids, reason, confidence, model, latency_ms, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![ulid(), side, event_id, gate.map(|g| g.to_string()), action, serde_json::to_string(target_ids).unwrap_or_default(), reason, confidence, model, latency_ms, now_ms()],
            )
            .map(|_| ())
        })
    }

    // ---- handles and scratch ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_handle(
        &self,
        id: &str,
        chat_id: Option<&str>,
        session_id: Option<&str>,
        kind: &str,
        chars: i64,
        lines: i64,
        path: &str,
        note: &str,
        blob_key: Option<&str>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO handles(id, chat_id, session_id, kind, chars, lines, path, note, blob_key, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(id) DO UPDATE SET note = excluded.note, blob_key = COALESCE(excluded.blob_key, handles.blob_key)",
                params![id, chat_id, session_id, kind, chars, lines, path, note, blob_key, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn handle(&self, id: &str) -> Result<Option<HandleRow>> {
        self.with(|c| {
            c.query_row("SELECT id, chat_id, kind, chars, lines, path, note, created_at FROM handles WHERE id = ?1", params![id], |r| {
                Ok(HandleRow { id: r.get(0)?, chat_id: r.get(1)?, kind: r.get(2)?, chars: r.get(3)?, lines: r.get(4)?, path: r.get(5)?, note: r.get(6)?, created_at: r.get(7)? })
            })
            .optional()
        })
    }

    pub fn handles_for_chat(&self, chat_id: &str, limit: i64) -> Result<Vec<HandleRow>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id, chat_id, kind, chars, lines, path, note, created_at FROM handles WHERE chat_id = ?1 ORDER BY created_at DESC LIMIT ?2")?;
            let rows = st.query_map(params![chat_id, limit], |r| {
                Ok(HandleRow { id: r.get(0)?, chat_id: r.get(1)?, kind: r.get(2)?, chars: r.get(3)?, lines: r.get(4)?, path: r.get(5)?, note: r.get(6)?, created_at: r.get(7)? })
            })?;
            rows.collect()
        })
    }

    pub fn scratch_set(&self, chat_id: &str, name: &str, value: Option<&str>) -> Result<()> {
        self.with(|c| {
            match value {
                Some(v) => c.execute(
                    "INSERT INTO scratch(chat_id, name, value, updated_at) VALUES (?1,?2,?3,?4) ON CONFLICT(chat_id, name) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                    params![chat_id, name, v, now_ms()],
                )?,
                None => c.execute("DELETE FROM scratch WHERE chat_id = ?1 AND name = ?2", params![chat_id, name])?,
            };
            Ok(())
        })
    }

    pub fn scratch_get(&self, chat_id: &str, name: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row(
                "SELECT value FROM scratch WHERE chat_id = ?1 AND name = ?2",
                params![chat_id, name],
                |r| r.get(0),
            )
            .optional()
        })
    }

    pub fn scratch_all(&self, chat_id: &str) -> Result<Vec<(String, String)>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT name, value FROM scratch WHERE chat_id = ?1 ORDER BY updated_at DESC LIMIT 40")?;
            let rows = st.query_map(params![chat_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
    }

    // ---- metrics and asks ---------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn record_turn(
        &self,
        chat_id: &str,
        message_id: &str,
        route: Option<&str>,
        ttft_ms: Option<i64>,
        route_ms: Option<i64>,
        total_ms: i64,
        tool_calls: i64,
        escalated: bool,
        usage: Option<&Value>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO turn_metrics(id, chat_id, message_id, route, ttft_ms, route_ms, total_ms, tool_calls, escalated, usage, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![ulid(), chat_id, message_id, route, ttft_ms, route_ms, total_ms, tool_calls, escalated as i64, usage.map(|u| u.to_string()), now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn turn_metrics(&self, limit: i64) -> Result<Vec<Value>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT chat_id, message_id, route, ttft_ms, route_ms, total_ms, tool_calls, escalated, usage, created_at FROM turn_metrics ORDER BY created_at DESC LIMIT ?1")?;
            let rows = st.query_map(params![limit], |r| {
                Ok(json!({
                    "chat_id": r.get::<_, String>(0)?, "message_id": r.get::<_, String>(1)?, "route": r.get::<_, Option<String>>(2)?,
                    "ttft_ms": r.get::<_, Option<i64>>(3)?, "route_ms": r.get::<_, Option<i64>>(4)?, "total_ms": r.get::<_, i64>(5)?,
                    "tool_calls": r.get::<_, i64>(6)?, "escalated": r.get::<_, i64>(7)? != 0,
                    "usage": r.get::<_, Option<String>>(8)?.and_then(|s| serde_json::from_str::<Value>(&s).ok()), "created_at": r.get::<_, i64>(9)?,
                }))
            })?;
            rows.collect()
        })
    }

    pub fn insert_ask(
        &self,
        id: &str,
        chat_id: Option<&str>,
        session_id: Option<&str>,
        question: &str,
    ) -> Result<()> {
        self.with(|c| c.execute("INSERT INTO asks(id, chat_id, session_id, question, created_at) VALUES (?1,?2,?3,?4,?5)", params![id, chat_id, session_id, question, now_ms()]).map(|_| ()))
    }

    pub fn open_ask(&self, chat_id: &str) -> Result<Option<(String, Option<String>, String)>> {
        self.with(|c| {
            c.query_row("SELECT id, session_id, question FROM asks WHERE chat_id = ?1 AND answered = 0 ORDER BY created_at DESC LIMIT 1", params![chat_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()
        })
    }

    pub fn answer_ask(&self, id: &str) -> Result<()> {
        self.with(|c| {
            c.execute("UPDATE asks SET answered = 1 WHERE id = ?1", params![id])
                .map(|_| ())
        })
    }
}

/// Turn free text into a safe FTS5 query: quoted terms joined with OR-less implicit AND, prefix match on the last.
fn fts_query(q: &str) -> String {
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect();
    if terms.is_empty() {
        return "\"\"".to_owned();
    }
    terms.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D19 redaction is applied before anything lands in T3.
    #[test]
    fn redaction_removes_known_secret_shapes() {
        let s = redact("key sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789 and Authorization: Bearer abc.def-ghi and\nOPENAI_API_KEY=sk-live-whatever\n");
        assert!(!s.contains("sk-or-v1-abcdef"));
        assert!(s.contains("[REDACTED:openrouter-key]"));
        assert!(s.contains("Authorization: Bearer [REDACTED:bearer]"));
        assert!(s.contains("OPENAI_API_KEY=[REDACTED:env]"));
    }
}
