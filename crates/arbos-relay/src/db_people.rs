//! Storage for people chats (D72): chats, memberships (the ACL), per-device read marks, blocks,
//! per-user notification queue, and the rate-limit event log. Same SQLite file; created from
//! `Db::open` through `PEOPLE_SCHEMA`.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use arbos_core::ids::now_ms;

use crate::db::Db;

pub const PEOPLE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS people_chats (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, created_by TEXT NOT NULL, created_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE TABLE IF NOT EXISTS people_members (
  chat_id TEXT NOT NULL, member TEXT NOT NULL, role TEXT NOT NULL, added_by TEXT NOT NULL,
  joined_at INTEGER NOT NULL, left_at INTEGER, settings TEXT NOT NULL DEFAULT '{}',
  muted INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (chat_id, member)
);
CREATE INDEX IF NOT EXISTS people_members_member ON people_members(member);
CREATE TABLE IF NOT EXISTS people_reads (
  chat_id TEXT NOT NULL, device_id TEXT NOT NULL, seq INTEGER NOT NULL DEFAULT 0,
  at INTEGER NOT NULL, hidden INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (chat_id, device_id)
);
CREATE TABLE IF NOT EXISTS blocks (
  blocker TEXT NOT NULL, blocked TEXT NOT NULL, created_at INTEGER NOT NULL,
  PRIMARY KEY (blocker, blocked)
);
CREATE TABLE IF NOT EXISTS user_pushes (
  id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, device_id TEXT NOT NULL,
  kind TEXT NOT NULL, payload TEXT NOT NULL, chat_id TEXT, created_at INTEGER NOT NULL, delivered_at INTEGER
);
CREATE INDEX IF NOT EXISTS user_pushes_device ON user_pushes(device_id, delivered_at);
CREATE TABLE IF NOT EXISTS rate_events (
  user_id TEXT NOT NULL, kind TEXT NOT NULL, at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS rate_events_user ON rate_events(user_id, kind, at);
"#;

pub fn migrate_people(c: &Connection) {
    let _ = c.execute(
        "ALTER TABLE users ADD COLUMN status TEXT NOT NULL DEFAULT 'member'",
        [],
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeopleChat {
    pub id: String,
    pub kind: String,
    pub created_by: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Membership {
    pub chat_id: String,
    /// `user:<user_id>` or `agent:<project_id>`.
    pub member: String,
    /// `owner` | `member` | `agent`.
    pub role: String,
    pub added_by: String,
    pub joined_at: i64,
    pub settings: Value,
    pub muted: bool,
}

impl Membership {
    pub fn user_id(&self) -> Option<&str> {
        self.member.strip_prefix("user:")
    }

    pub fn project_id(&self) -> Option<&str> {
        self.member.strip_prefix("agent:")
    }
}

/// Who is in a room right now: `(members, head_seq, last_row_at)` come from separate calls; this
/// is the chat plus its live memberships.
#[derive(Debug, Clone)]
pub struct ChatWithMembers {
    pub chat: PeopleChat,
    pub members: Vec<Membership>,
}

fn membership_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Membership> {
    Ok(Membership {
        chat_id: r.get(0)?,
        member: r.get(1)?,
        role: r.get(2)?,
        added_by: r.get(3)?,
        joined_at: r.get(4)?,
        settings: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
        muted: r.get::<_, i64>(6)? != 0,
    })
}

const MEMBER_COLS: &str = "chat_id, member, role, added_by, joined_at, settings, muted";

impl Db {
    // ---- chats and members --------------------------------------------------------------------

    pub fn insert_people_chat(&self, chat: &PeopleChat) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO people_chats(id, kind, created_by, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![chat.id, chat.kind, chat.created_by, chat.created_at],
            )
            .map(|_| ())
        })
    }

    pub fn people_chat(&self, id: &str) -> Result<Option<PeopleChat>> {
        self.with(|c| {
            c.query_row(
                "SELECT id, kind, created_by, created_at FROM people_chats WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |r| {
                    Ok(PeopleChat {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        created_by: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .optional()
        })
    }

    pub fn add_member(
        &self,
        chat_id: &str,
        member: &str,
        role: &str,
        added_by: &str,
        settings: &Value,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO people_members(chat_id, member, role, added_by, joined_at, settings)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(chat_id, member) DO UPDATE SET role = excluded.role, added_by = excluded.added_by,
                    joined_at = excluded.joined_at, left_at = NULL, settings = excluded.settings",
                params![chat_id, member, role, added_by, now_ms(), settings.to_string()],
            )
            .map(|_| ())
        })
    }

    pub fn remove_member(&self, chat_id: &str, member: &str) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "UPDATE people_members SET left_at = ?3 WHERE chat_id = ?1 AND member = ?2 AND left_at IS NULL",
                params![chat_id, member, now_ms()],
            )
            .map(|n| n > 0)
        })
    }

    pub fn set_member_settings(
        &self,
        chat_id: &str,
        member: &str,
        settings: &Value,
    ) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "UPDATE people_members SET settings = ?3 WHERE chat_id = ?1 AND member = ?2 AND left_at IS NULL",
                params![chat_id, member, settings.to_string()],
            )
            .map(|n| n > 0)
        })
    }

    pub fn members_of(&self, chat_id: &str) -> Result<Vec<Membership>> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {MEMBER_COLS} FROM people_members WHERE chat_id = ?1 AND left_at IS NULL ORDER BY joined_at"
            ))?;
            let rows = st.query_map(params![chat_id], membership_from_row)?;
            rows.collect()
        })
    }

    pub fn membership(&self, chat_id: &str, member: &str) -> Result<Option<Membership>> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {MEMBER_COLS} FROM people_members WHERE chat_id = ?1 AND member = ?2 AND left_at IS NULL"),
                params![chat_id, member],
                membership_from_row,
            )
            .optional()
        })
    }

    /// Live chats a member (`user:<id>` or `agent:<project>`) is in.
    pub fn chats_of_member(&self, member: &str) -> Result<Vec<ChatWithMembers>> {
        let chats: Vec<PeopleChat> = self.with(|c| {
            let mut st = c.prepare(
                "SELECT p.id, p.kind, p.created_by, p.created_at FROM people_chats p
                 JOIN people_members m ON m.chat_id = p.id
                 WHERE m.member = ?1 AND m.left_at IS NULL AND p.deleted_at IS NULL
                 ORDER BY p.created_at DESC",
            )?;
            let rows = st.query_map(params![member], |r| {
                Ok(PeopleChat {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    created_by: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })?;
            rows.collect()
        })?;
        chats
            .into_iter()
            .map(|chat| {
                let members = self.members_of(&chat.id)?;
                Ok(ChatWithMembers { chat, members })
            })
            .collect()
    }

    /// The existing DM between two users, if any (both still members).
    pub fn dm_between(&self, a: &str, b: &str) -> Result<Option<PeopleChat>> {
        self.with(|c| {
            c.query_row(
                "SELECT p.id, p.kind, p.created_by, p.created_at FROM people_chats p
                 JOIN people_members ma ON ma.chat_id = p.id AND ma.member = ?1 AND ma.left_at IS NULL
                 JOIN people_members mb ON mb.chat_id = p.id AND mb.member = ?2 AND mb.left_at IS NULL
                 WHERE p.kind = 'dm' AND p.deleted_at IS NULL ORDER BY p.created_at LIMIT 1",
                params![format!("user:{a}"), format!("user:{b}")],
                |r| {
                    Ok(PeopleChat {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        created_by: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .optional()
        })
    }

    pub fn delete_people_chat(&self, chat_id: &str) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "UPDATE people_chats SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                params![chat_id, now_ms()],
            )
            .map(|n| n > 0)
        })
    }

    pub fn people_chats_created_since(&self, user_id: &str, since_ms: i64) -> Result<i64> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM people_chats WHERE created_by = ?1 AND created_at >= ?2",
                params![user_id, since_ms],
                |r| r.get(0),
            )
        })
    }

    /// Drop a room entirely: rows, checkpoint, sequence, blob index.
    pub fn delete_room(&self, room: &str) -> Result<()> {
        self.with(|c| {
            c.execute("DELETE FROM rows WHERE room = ?1", params![room])?;
            c.execute("DELETE FROM checkpoints WHERE room = ?1", params![room])?;
            c.execute("DELETE FROM room_seq WHERE room = ?1", params![room])?;
            Ok(())
        })
    }

    // ---- read marks and hide ------------------------------------------------------------------

    pub fn read_mark(&self, chat_id: &str, device_id: &str) -> Result<(i64, bool)> {
        self.with(|c| {
            c.query_row(
                "SELECT seq, hidden FROM people_reads WHERE chat_id = ?1 AND device_id = ?2",
                params![chat_id, device_id],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map(|o| o.unwrap_or((0, false)))
        })
    }

    pub fn set_read_mark(&self, chat_id: &str, device_id: &str, seq: i64) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO people_reads(chat_id, device_id, seq, at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(chat_id, device_id) DO UPDATE SET seq = MAX(people_reads.seq, excluded.seq), at = excluded.at, hidden = 0",
                params![chat_id, device_id, seq, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn set_hidden(&self, chat_id: &str, device_id: &str, hidden: bool) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO people_reads(chat_id, device_id, seq, at, hidden) VALUES (?1, ?2, 0, ?3, ?4)
                 ON CONFLICT(chat_id, device_id) DO UPDATE SET hidden = excluded.hidden, at = excluded.at",
                params![chat_id, device_id, now_ms(), hidden as i64],
            )
            .map(|_| ())
        })
    }

    /// `(head_seq, last_row_at)` of a room; zeros when it has no rows.
    pub fn room_activity(&self, room: &str) -> Result<(i64, i64)> {
        self.with(|c| {
            let head: i64 = c
                .query_row(
                    "SELECT head FROM room_seq WHERE room = ?1",
                    params![room],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            let last: i64 = c
                .query_row(
                    "SELECT COALESCE(MAX(created_at), 0) FROM rows WHERE room = ?1",
                    params![room],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            Ok((head, last))
        })
    }

    // ---- blocks -------------------------------------------------------------------------------

    pub fn block(&self, blocker: &str, blocked: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO blocks(blocker, blocked, created_at) VALUES (?1, ?2, ?3)",
                params![blocker, blocked, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn unblock(&self, blocker: &str, blocked: &str) -> Result<bool> {
        self.with(|c| {
            c.execute(
                "DELETE FROM blocks WHERE blocker = ?1 AND blocked = ?2",
                params![blocker, blocked],
            )
            .map(|n| n > 0)
        })
    }

    /// Is either of the two blocking the other?
    pub fn blocked_either_way(&self, a: &str, b: &str) -> Result<bool> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM blocks WHERE (blocker = ?1 AND blocked = ?2) OR (blocker = ?2 AND blocked = ?1)",
                params![a, b],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
        })
    }

    /// Does anyone in this list block `user`?
    pub fn blocked_by_any(&self, user: &str, others: &[String]) -> Result<bool> {
        for o in others {
            if o != user {
                let hit: bool = self.with(|c| {
                    c.query_row(
                        "SELECT COUNT(*) FROM blocks WHERE blocker = ?1 AND blocked = ?2",
                        params![o, user],
                        |r| r.get::<_, i64>(0),
                    )
                    .map(|n| n > 0)
                })?;
                if hit {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn blocks_of(&self, blocker: &str) -> Result<Vec<(String, i64)>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT blocked, created_at FROM blocks WHERE blocker = ?1 ORDER BY created_at DESC",
            )?;
            let rows = st.query_map(params![blocker], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })
    }

    // ---- per-user pushes (fan-out to every live device) ---------------------------------------

    /// Queue a notification for every non-revoked device of a user. `coalesce_chat` keeps at
    /// most one undelivered push of this kind per chat per device.
    pub fn push_user(
        &self,
        user_id: &str,
        kind: &str,
        payload: &Value,
        coalesce_chat: Option<&str>,
    ) -> Result<()> {
        let now = now_ms();
        let body = payload.to_string();
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id FROM devices WHERE user_id = ?1 AND revoked_at IS NULL",
            )?;
            let devices: Vec<String> = st
                .query_map(params![user_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for d in devices {
                if let Some(chat) = coalesce_chat {
                    let pending: i64 = c.query_row(
                        "SELECT COUNT(*) FROM user_pushes WHERE device_id = ?1 AND kind = ?2 AND chat_id = ?3 AND delivered_at IS NULL",
                        params![d, kind, chat],
                        |r| r.get(0),
                    )?;
                    if pending > 0 {
                        continue;
                    }
                }
                c.execute(
                    "INSERT INTO user_pushes(user_id, device_id, kind, payload, chat_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![user_id, d, kind, body, coalesce_chat, now],
                )?;
            }
            Ok(())
        })
    }

    pub fn take_user_pushes(&self, device_id: &str) -> Result<Vec<(i64, String, Value, i64)>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, kind, payload, created_at FROM user_pushes WHERE device_id = ?1 AND delivered_at IS NULL ORDER BY id",
            )?;
            let items: Vec<(i64, String, Value, i64)> = st
                .query_map(params![device_id], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                        r.get(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let now = now_ms();
            for (id, _, _, _) in &items {
                c.execute(
                    "UPDATE user_pushes SET delivered_at = ?2 WHERE id = ?1",
                    params![id, now],
                )?;
            }
            c.execute(
                "DELETE FROM user_pushes WHERE delivered_at IS NOT NULL AND delivered_at < ?1",
                params![now - 7 * 24 * 3_600_000],
            )?;
            Ok(items)
        })
    }

    // ---- rate limits (sliding window over logged events) --------------------------------------

    /// Log one event and return how many of this kind the user has in the window (incl. this one).
    pub fn rate_event(&self, user_id: &str, kind: &str, window_ms: i64) -> Result<i64> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO rate_events(user_id, kind, at) VALUES (?1, ?2, ?3)",
                params![user_id, kind, now],
            )?;
            c.execute(
                "DELETE FROM rate_events WHERE at < ?1",
                params![now - 24 * 3_600_000],
            )?;
            c.query_row(
                "SELECT COUNT(*) FROM rate_events WHERE user_id = ?1 AND kind = ?2 AND at >= ?3",
                params![user_id, kind, now - window_ms],
                |r| r.get(0),
            )
        })
    }

    // ---- users: invited placeholders ----------------------------------------------------------

    /// Prefix search on GitHub logins (members and invited placeholders).
    pub fn search_users(&self, prefix: &str, limit: i64) -> Result<Vec<Value>> {
        let like = format!(
            "{}%",
            prefix.replace('%', "").replace('_', "\\_").to_lowercase()
        );
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, login, avatar_url, name, status FROM users WHERE login IS NOT NULL AND lower(login) LIKE ?1 ESCAPE '\\' ORDER BY (status = 'member') DESC, lower(login) LIMIT ?2",
            )?;
            let rows = st.query_map(params![like, limit], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?, "handle": r.get::<_, String>(1)?,
                    "avatar_url": r.get::<_, Option<String>>(2)?, "name": r.get::<_, Option<String>>(3)?,
                    "status": r.get::<_, String>(4)?,
                }))
            })?;
            rows.collect()
        })
    }

    /// A placeholder for a GitHub account that has not signed in (`status = invited`). No grant
    /// until the first sign-in. Returns the user id.
    pub fn upsert_invited_user(
        &self,
        github_id: i64,
        login: &str,
        avatar_url: Option<&str>,
        name: Option<&str>,
    ) -> Result<String> {
        let id = format!("gh_{github_id}");
        self.with(|c| {
            c.execute(
                "INSERT INTO users(id, snapshot, spend_cap_usd, created_at, github_id, login, avatar_url, name, status)
                 VALUES (?1, NULL, 0.0, ?2, ?3, ?4, ?5, ?6, 'invited')
                 ON CONFLICT(id) DO UPDATE SET login = excluded.login,
                    avatar_url = COALESCE(excluded.avatar_url, users.avatar_url), name = COALESCE(excluded.name, users.name)",
                params![id, now_ms(), github_id, login, avatar_url, name],
            )
            .map(|_| ())
        })?;
        Ok(id)
    }

    /// `(id, login, avatar_url, name, status)` for a handle, case-insensitive.
    pub fn user_by_handle(&self, handle: &str) -> Result<Option<Value>> {
        self.with(|c| {
            c.query_row(
                "SELECT id, login, avatar_url, name, status FROM users WHERE lower(login) = lower(?1)",
                params![handle],
                |r| {
                    Ok(serde_json::json!({
                        "id": r.get::<_, String>(0)?, "handle": r.get::<_, String>(1)?,
                        "avatar_url": r.get::<_, Option<String>>(2)?, "name": r.get::<_, Option<String>>(3)?,
                        "status": r.get::<_, String>(4)?,
                    }))
                },
            )
            .optional()
        })
    }

    /// Public profile fields for a user id (for member lists).
    pub fn user_card(&self, user_id: &str) -> Result<Option<Value>> {
        self.with(|c| {
            c.query_row(
                "SELECT id, login, avatar_url, name, status, created_at FROM users WHERE id = ?1",
                params![user_id],
                |r| {
                    Ok(serde_json::json!({
                        "id": r.get::<_, String>(0)?, "handle": r.get::<_, Option<String>>(1)?,
                        "avatar_url": r.get::<_, Option<String>>(2)?, "name": r.get::<_, Option<String>>(3)?,
                        "status": r.get::<_, String>(4)?, "created_at": r.get::<_, i64>(5)?,
                    }))
                },
            )
            .optional()
        })
    }
}
