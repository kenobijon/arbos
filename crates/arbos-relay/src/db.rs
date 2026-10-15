//! Relay storage: one SQLite file, forward-only migrations (D50). The relay
//! stores opaque rows and never parses Loro bytes (crdt-sync §9).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use arbos_core::ids::now_ms;

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS pairing_codes (
  code TEXT PRIMARY KEY, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER
);
CREATE TABLE IF NOT EXISTS devices (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL, revoked_at INTEGER, last_seen_at INTEGER
);
CREATE TABLE IF NOT EXISTS projects (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, icon TEXT NOT NULL DEFAULT '',
  instructions TEXT NOT NULL DEFAULT '', models TEXT NOT NULL DEFAULT '{}',
  state TEXT NOT NULL DEFAULT 'waking', created_at INTEGER NOT NULL, last_active_at INTEGER NOT NULL,
  spend_cap_daily REAL NOT NULL DEFAULT 25.0,
  project_token_hash TEXT
);
CREATE TABLE IF NOT EXISTS chats (
  id TEXT PRIMARY KEY, project_id TEXT NOT NULL, title TEXT NOT NULL DEFAULT 'New chat',
  is_pinned INTEGER NOT NULL DEFAULT 0, position INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, doc_epoch INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS chats_project ON chats(project_id, position);
CREATE TABLE IF NOT EXISTS sandboxes (
  project_id TEXT PRIMARY KEY, provider TEXT NOT NULL, sandbox_id TEXT,
  daytona_state TEXT, device_id TEXT NOT NULL, box_token_hash TEXT NOT NULL, mcp_token TEXT NOT NULL,
  snapshot TEXT, region TEXT, last_started_at INTEGER, last_stopped_at INTEGER, last_seen_at INTEGER,
  box_url TEXT, preview_token TEXT, local_pid INTEGER
);
CREATE TABLE IF NOT EXISTS rows (
  room TEXT NOT NULL, seq INTEGER NOT NULL, device TEXT NOT NULL, batch_id TEXT NOT NULL,
  bytes BLOB NOT NULL, created_at INTEGER NOT NULL,
  PRIMARY KEY (room, seq)
);
CREATE UNIQUE INDEX IF NOT EXISTS rows_batch ON rows(room, batch_id);
CREATE TABLE IF NOT EXISTS checkpoints (
  room TEXT PRIMARY KEY, seq INTEGER NOT NULL, frontier BLOB NOT NULL, bytes BLOB NOT NULL,
  size INTEGER NOT NULL, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS room_seq (room TEXT PRIMARY KEY, head INTEGER NOT NULL, floor INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS blobs (
  project_id TEXT NOT NULL, key TEXT NOT NULL, size INTEGER NOT NULL, mime TEXT, created_at INTEGER NOT NULL,
  PRIMARY KEY (project_id, key)
);
CREATE TABLE IF NOT EXISTS usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, at INTEGER NOT NULL, api TEXT NOT NULL,
  model TEXT NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER NOT NULL,
  cache_write INTEGER NOT NULL, usd REAL NOT NULL, latency_ms INTEGER NOT NULL, ttfb_ms INTEGER
);
CREATE INDEX IF NOT EXISTS usage_project_at ON usage(project_id, at);
CREATE TABLE IF NOT EXISTS wake_jobs (
  project_id TEXT PRIMARY KEY, reason TEXT NOT NULL, due_at INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, last_error TEXT
);
CREATE TABLE IF NOT EXISTS push_queue (
  id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL,
  created_at INTEGER NOT NULL, delivered_at INTEGER
);
CREATE TABLE IF NOT EXISTS timings (
  id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, kind TEXT NOT NULL, ms INTEGER NOT NULL,
  detail TEXT, at INTEGER NOT NULL
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub instructions: String,
    pub models: serde_json::Value,
    pub state: String,
    pub created_at: i64,
    pub last_active_at: i64,
    pub spend_cap_daily: f64,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub is_pinned: bool,
    pub position: i64,
    pub created_at: i64,
    pub doc_epoch: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sandbox {
    pub project_id: String,
    pub provider: String,
    pub sandbox_id: Option<String>,
    pub daytona_state: Option<String>,
    pub device_id: String,
    pub box_token_hash: String,
    pub mcp_token: String,
    pub snapshot: Option<String>,
    pub last_started_at: Option<i64>,
    pub last_stopped_at: Option<i64>,
    pub last_seen_at: Option<i64>,
    pub box_url: Option<String>,
    pub preview_token: Option<String>,
    pub local_pid: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub seq: i64,
    pub device: String,
    pub batch_id: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct RoomState {
    pub head: i64,
    pub floor: i64,
    pub checkpoint_seq: i64,
    pub checkpoint_size: i64,
    pub checkpoint_frontier: Vec<u8>,
    pub row_count: i64,
    pub row_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub usd: f64,
    pub calls: i64,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let c = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        c.execute_batch(SCHEMA)?;
        // Forward-only, additive migrations (D50).
        let _ = c.execute(
            "ALTER TABLE projects ADD COLUMN color TEXT NOT NULL DEFAULT '#8b7cf6'",
            [],
        );
        Ok(Self(Arc::new(Mutex::new(c))))
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let c = self.0.lock().expect("db lock");
        Ok(f(&c)?)
    }

    // ---- pairing and devices ------------------------------------------------------------------

    pub fn new_pairing_code(&self, code: &str, ttl_ms: i64) -> Result<()> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO pairing_codes(code, created_at, expires_at) VALUES (?1, ?2, ?3)",
                params![code, now, now + ttl_ms],
            )
            .map(|_| ())
        })
    }

    /// Consume a pairing code and register a device. Returns the device id.
    pub fn redeem_pairing_code(
        &self,
        code: &str,
        device_id: &str,
        name: &str,
        token_hash: &str,
    ) -> Result<bool> {
        let now = now_ms();
        self.with(|c| {
            let n = c.execute(
                "UPDATE pairing_codes SET used_at = ?2 WHERE code = ?1 AND used_at IS NULL AND expires_at > ?2",
                params![code, now],
            )?;
            if n == 0 {
                return Ok(false);
            }
            c.execute(
                "INSERT INTO devices(id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![device_id, name, token_hash, now],
            )?;
            Ok(true)
        })
    }

    pub fn device_by_token_hash(&self, h: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row(
                "SELECT id FROM devices WHERE token_hash = ?1 AND revoked_at IS NULL",
                params![h],
                |r| r.get(0),
            )
            .optional()
        })
    }

    pub fn device_count(&self) -> Result<i64> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL",
                [],
                |r| r.get(0),
            )
        })
    }

    pub fn revoke_device(&self, id: &str) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![id, now_ms()],
            )
            .map(|n| n > 0)
        })
    }

    pub fn list_devices(&self) -> Result<Vec<serde_json::Value>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id, name, created_at, revoked_at, last_seen_at FROM devices ORDER BY created_at")?;
            let rows = st.query_map([], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "created_at": r.get::<_, i64>(2)?,
                    "revoked_at": r.get::<_, Option<i64>>(3)?, "last_seen_at": r.get::<_, Option<i64>>(4)?,
                }))
            })?;
            rows.collect()
        })
    }

    // ---- projects and chats -------------------------------------------------------------------

    pub fn insert_project(&self, p: &Project) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO projects(id, name, icon, instructions, models, state, created_at, last_active_at, spend_cap_daily, color)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![p.id, p.name, p.icon, p.instructions, p.models.to_string(), p.state, p.created_at, p.last_active_at, p.spend_cap_daily, p.color],
            )
            .map(|_| ())
        })
    }

    fn row_to_project(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
        Ok(Project {
            id: r.get(0)?,
            name: r.get(1)?,
            icon: r.get(2)?,
            instructions: r.get(3)?,
            models: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
            state: r.get(5)?,
            created_at: r.get(6)?,
            last_active_at: r.get(7)?,
            spend_cap_daily: r.get(8)?,
            color: r.get(9)?,
        })
    }

    const PROJECT_COLS: &'static str =
        "id, name, icon, instructions, models, state, created_at, last_active_at, spend_cap_daily, color";

    pub fn project(&self, id_or_name: &str) -> Result<Option<Project>> {
        self.with(|c| {
            c.query_row(
                &format!(
                    "SELECT {} FROM projects WHERE id = ?1 OR name = ?1",
                    Self::PROJECT_COLS
                ),
                params![id_or_name],
                Self::row_to_project,
            )
            .optional()
        })
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {} FROM projects ORDER BY last_active_at DESC",
                Self::PROJECT_COLS
            ))?;
            let rows = st.query_map([], Self::row_to_project)?;
            rows.collect()
        })
    }

    /// Partial update of the editable project fields (settings writes from clients or the box).
    #[allow(clippy::too_many_arguments)]
    pub fn update_project(
        &self,
        id: &str,
        name: Option<&str>,
        icon: Option<&str>,
        color: Option<&str>,
        instructions: Option<&str>,
        models: Option<&serde_json::Value>,
        spend_cap_daily: Option<f64>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE projects SET name = COALESCE(?2, name), icon = COALESCE(?3, icon), color = COALESCE(?4, color),
                        instructions = COALESCE(?5, instructions), models = COALESCE(?6, models), spend_cap_daily = COALESCE(?7, spend_cap_daily)
                 WHERE id = ?1",
                params![id, name, icon, color, instructions, models.map(|m| m.to_string()), spend_cap_daily],
            )
            .map(|_| ())
        })
    }

    pub fn update_chat_title(&self, project_id: &str, chat_id: &str, title: &str) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "UPDATE chats SET title = ?3 WHERE project_id = ?1 AND id = ?2",
                params![project_id, chat_id, title],
            )
            .map(|n| n > 0)
        })
    }

    pub fn set_project_state(&self, id: &str, state: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE projects SET state = ?2, last_active_at = ?3 WHERE id = ?1",
                params![id, state, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn insert_chat(&self, ch: &Chat) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO chats(id, project_id, title, is_pinned, position, created_at, doc_epoch) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![ch.id, ch.project_id, ch.title, ch.is_pinned as i64, ch.position, ch.created_at, ch.doc_epoch],
            )
            .map(|_| ())
        })
    }

    pub fn list_chats(&self, project_id: &str) -> Result<Vec<Chat>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, project_id, title, is_pinned, position, created_at, doc_epoch FROM chats WHERE project_id = ?1 ORDER BY position, created_at",
            )?;
            let rows = st.query_map(params![project_id], |r| {
                Ok(Chat {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    title: r.get(2)?,
                    is_pinned: r.get::<_, i64>(3)? != 0,
                    position: r.get(4)?,
                    created_at: r.get(5)?,
                    doc_epoch: r.get(6)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn chat_exists(&self, project_id: &str, chat_id: &str) -> Result<bool> {
        self.with(|c| {
            c.query_row(
                "SELECT 1 FROM chats WHERE project_id = ?1 AND id = ?2",
                params![project_id, chat_id],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
        })
    }

    // ---- sandboxes ----------------------------------------------------------------------------

    pub fn upsert_sandbox(&self, s: &Sandbox) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO sandboxes(project_id, provider, sandbox_id, daytona_state, device_id, box_token_hash, mcp_token, snapshot,
                    last_started_at, last_stopped_at, last_seen_at, box_url, preview_token, local_pid)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
                 ON CONFLICT(project_id) DO UPDATE SET provider=excluded.provider, sandbox_id=excluded.sandbox_id,
                    daytona_state=excluded.daytona_state, device_id=excluded.device_id, box_token_hash=excluded.box_token_hash,
                    mcp_token=excluded.mcp_token, snapshot=excluded.snapshot, last_started_at=excluded.last_started_at,
                    last_stopped_at=excluded.last_stopped_at, last_seen_at=excluded.last_seen_at, box_url=excluded.box_url,
                    preview_token=excluded.preview_token, local_pid=excluded.local_pid",
                params![
                    s.project_id, s.provider, s.sandbox_id, s.daytona_state, s.device_id, s.box_token_hash, s.mcp_token, s.snapshot,
                    s.last_started_at, s.last_stopped_at, s.last_seen_at, s.box_url, s.preview_token, s.local_pid
                ],
            )
            .map(|_| ())
        })
    }

    pub fn sandbox(&self, project_id: &str) -> Result<Option<Sandbox>> {
        self.with(|c| {
            c.query_row(
                "SELECT project_id, provider, sandbox_id, daytona_state, device_id, box_token_hash, mcp_token, snapshot,
                        last_started_at, last_stopped_at, last_seen_at, box_url, preview_token, local_pid
                 FROM sandboxes WHERE project_id = ?1",
                params![project_id],
                |r| {
                    Ok(Sandbox {
                        project_id: r.get(0)?,
                        provider: r.get(1)?,
                        sandbox_id: r.get(2)?,
                        daytona_state: r.get(3)?,
                        device_id: r.get(4)?,
                        box_token_hash: r.get(5)?,
                        mcp_token: r.get(6)?,
                        snapshot: r.get(7)?,
                        last_started_at: r.get(8)?,
                        last_stopped_at: r.get(9)?,
                        last_seen_at: r.get(10)?,
                        box_url: r.get(11)?,
                        preview_token: r.get(12)?,
                        local_pid: r.get(13)?,
                    })
                },
            )
            .optional()
        })
    }

    pub fn project_by_box_token_hash(&self, h: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row(
                "SELECT project_id FROM sandboxes WHERE box_token_hash = ?1",
                params![h],
                |r| r.get(0),
            )
            .optional()
        })
    }

    pub fn touch_sandbox(&self, project_id: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE sandboxes SET last_seen_at = ?2 WHERE project_id = ?1",
                params![project_id, now_ms()],
            )
            .map(|_| ())
        })
    }

    // ---- rooms --------------------------------------------------------------------------------

    pub fn room_state(&self, room: &str) -> Result<RoomState> {
        self.with(|c| {
            let (head, floor): (i64, i64) = c
                .query_row(
                    "SELECT head, floor FROM room_seq WHERE room = ?1",
                    params![room],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or((0, 0));
            let cp: Option<(i64, i64, Vec<u8>)> = c
                .query_row(
                    "SELECT seq, size, frontier FROM checkpoints WHERE room = ?1",
                    params![room],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let (row_count, row_bytes): (i64, i64) = c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(bytes)),0) FROM rows WHERE room = ?1",
                params![room],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let (checkpoint_seq, checkpoint_size, checkpoint_frontier) =
                cp.unwrap_or((0, 0, Vec::new()));
            Ok(RoomState {
                head,
                floor,
                checkpoint_seq,
                checkpoint_size,
                checkpoint_frontier,
                row_count,
                row_bytes,
            })
        })
    }

    /// Append a row. Returns `(seq, dup)`; a duplicate batch id returns the existing seq (idempotent push).
    pub fn append_row(
        &self,
        room: &str,
        device: &str,
        batch_id: &str,
        bytes: &[u8],
    ) -> Result<(i64, bool)> {
        let now = now_ms();
        self.with(|c| {
            let existing: Option<i64> = c
                .query_row("SELECT seq FROM rows WHERE room = ?1 AND batch_id = ?2", params![room, batch_id], |r| r.get(0))
                .optional()?;
            if let Some(seq) = existing {
                return Ok((seq, true));
            }
            c.execute("INSERT INTO room_seq(room, head, floor) VALUES (?1, 0, 0) ON CONFLICT(room) DO NOTHING", params![room])?;
            c.execute("UPDATE room_seq SET head = head + 1 WHERE room = ?1", params![room])?;
            let seq: i64 = c.query_row("SELECT head FROM room_seq WHERE room = ?1", params![room], |r| r.get(0))?;
            c.execute(
                "INSERT INTO rows(room, seq, device, batch_id, bytes, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![room, seq, device, batch_id, bytes, now],
            )?;
            Ok((seq, false))
        })
    }

    pub fn rows_after(
        &self,
        room: &str,
        after: i64,
        exclude_device: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Row>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT seq, device, batch_id, bytes FROM rows WHERE room = ?1 AND seq > ?2 AND (?3 IS NULL OR device != ?3)
                 ORDER BY seq LIMIT ?4",
            )?;
            let rows = st.query_map(params![room, after, exclude_device, limit], |r| {
                Ok(Row { seq: r.get(0)?, device: r.get(1)?, batch_id: r.get(2)?, bytes: r.get(3)? })
            })?;
            rows.collect()
        })
    }

    /// Store a checkpoint and delete rows it covers (guard: `seq_covered >= floor`).
    pub fn put_checkpoint(
        &self,
        room: &str,
        seq_covered: i64,
        frontier: &[u8],
        bytes: &[u8],
    ) -> Result<bool> {
        self.with(|c| {
            let floor: i64 = c
                .query_row("SELECT floor FROM room_seq WHERE room = ?1", params![room], |r| r.get(0))
                .optional()?
                .unwrap_or(0);
            if seq_covered < floor {
                return Ok(false);
            }
            c.execute(
                "INSERT INTO checkpoints(room, seq, frontier, bytes, size, created_at) VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(room) DO UPDATE SET seq=excluded.seq, frontier=excluded.frontier, bytes=excluded.bytes,
                 size=excluded.size, created_at=excluded.created_at",
                params![room, seq_covered, frontier, bytes, bytes.len() as i64, now_ms()],
            )?;
            c.execute("DELETE FROM rows WHERE room = ?1 AND seq <= ?2", params![room, seq_covered])?;
            c.execute("INSERT INTO room_seq(room, head, floor) VALUES (?1, ?2, ?2) ON CONFLICT(room) DO UPDATE SET floor = ?2, head = MAX(head, ?2)", params![room, seq_covered])?;
            Ok(true)
        })
    }

    pub fn checkpoint_bytes(&self, room: &str) -> Result<Option<(i64, Vec<u8>)>> {
        self.with(|c| {
            c.query_row(
                "SELECT seq, bytes FROM checkpoints WHERE room = ?1",
                params![room],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })
    }

    // ---- blobs --------------------------------------------------------------------------------

    pub fn record_blob(
        &self,
        project_id: &str,
        key: &str,
        size: i64,
        mime: Option<&str>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO blobs(project_id, key, size, mime, created_at) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(project_id, key) DO UPDATE SET size=excluded.size, mime=excluded.mime",
                params![project_id, key, size, mime, now_ms()],
            )
            .map(|_| ())
        })
    }

    // ---- usage / spend ------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn record_usage(
        &self,
        project_id: &str,
        api: &str,
        model: &str,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_write: i64,
        usd: f64,
        latency_ms: i64,
        ttfb_ms: Option<i64>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO usage(project_id, at, api, model, input, output, cache_read, cache_write, usd, latency_ms, ttfb_ms)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![project_id, now_ms(), api, model, input, output, cache_read, cache_write, usd, latency_ms, ttfb_ms],
            )
            .map(|_| ())
        })
    }

    pub fn usage_since(&self, project_id: &str, since_ms: i64) -> Result<UsageTotals> {
        self.with(|c| {
            c.query_row(
                "SELECT COALESCE(SUM(input),0), COALESCE(SUM(output),0), COALESCE(SUM(cache_read),0), COALESCE(SUM(cache_write),0),
                        COALESCE(SUM(usd),0.0), COUNT(*) FROM usage WHERE project_id = ?1 AND at >= ?2",
                params![project_id, since_ms],
                |r| {
                    Ok(UsageTotals {
                        input: r.get(0)?,
                        output: r.get(1)?,
                        cache_read: r.get(2)?,
                        cache_write: r.get(3)?,
                        usd: r.get(4)?,
                        calls: r.get(5)?,
                    })
                },
            )
        })
    }

    // ---- wake jobs, push queue, timings -------------------------------------------------------

    pub fn upsert_wake_job(&self, project_id: &str, reason: &str) -> Result<()> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO wake_jobs(project_id, reason, due_at, attempts, created_at) VALUES (?1,?2,?3,0,?3)
                 ON CONFLICT(project_id) DO UPDATE SET reason = excluded.reason, due_at = MIN(wake_jobs.due_at, excluded.due_at)",
                params![project_id, reason, now],
            )
            .map(|_| ())
        })
    }

    pub fn due_wake_jobs(&self) -> Result<Vec<(String, String, i64)>> {
        self.with(|c| {
            let mut st =
                c.prepare("SELECT project_id, reason, attempts FROM wake_jobs WHERE due_at <= ?1")?;
            let rows =
                st.query_map(params![now_ms()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect()
        })
    }

    pub fn finish_wake_job(&self, project_id: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "DELETE FROM wake_jobs WHERE project_id = ?1",
                params![project_id],
            )
            .map(|_| ())
        })
    }

    pub fn retry_wake_job(&self, project_id: &str, delay_ms: i64, err: &str) -> Result<i64> {
        self.with(|c| {
            c.execute(
                "UPDATE wake_jobs SET attempts = attempts + 1, due_at = ?2, last_error = ?3 WHERE project_id = ?1",
                params![project_id, now_ms() + delay_ms, err],
            )?;
            c.query_row("SELECT attempts FROM wake_jobs WHERE project_id = ?1", params![project_id], |r| r.get(0))
        })
    }

    pub fn enqueue_push(
        &self,
        project_id: &str,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO push_queue(project_id, kind, payload, created_at) VALUES (?1,?2,?3,?4)",
                params![project_id, kind, payload.to_string(), now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn take_pushes(&self, project_id: &str) -> Result<Vec<(i64, String, serde_json::Value)>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT id, kind, payload FROM push_queue WHERE project_id = ?1 AND delivered_at IS NULL ORDER BY id")?;
            let items: Vec<(i64, String, serde_json::Value)> = st
                .query_map(params![project_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default()))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (id, _, _) in &items {
                c.execute("UPDATE push_queue SET delivered_at = ?2 WHERE id = ?1", params![id, now_ms()])?;
            }
            Ok(items)
        })
    }

    pub fn record_timing(
        &self,
        project_id: &str,
        kind: &str,
        ms: i64,
        detail: Option<&str>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO timings(project_id, kind, ms, detail, at) VALUES (?1,?2,?3,?4,?5)",
                params![project_id, kind, ms, detail, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn timings(&self, project_id: &str) -> Result<Vec<serde_json::Value>> {
        self.with(|c| {
            let mut st = c.prepare("SELECT kind, ms, detail, at FROM timings WHERE project_id = ?1 ORDER BY at")?;
            let rows = st.query_map(params![project_id], |r| {
                Ok(serde_json::json!({"kind": r.get::<_, String>(0)?, "ms": r.get::<_, i64>(1)?, "detail": r.get::<_, Option<String>>(2)?, "at": r.get::<_, i64>(3)?}))
            })?;
            rows.collect()
        })
    }
}
