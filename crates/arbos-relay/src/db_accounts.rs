//! Storage for self-serve accounts: GitHub users, sign-in flows, refresh tokens, the credit
//! ledger, and the kill switch. Same SQLite file as `db.rs`;
//! these tables are created from `Db::open` through `ACCOUNTS_SCHEMA`.
//!
//! Money is USD as REAL. A ledger row is positive for a grant and negative for a debit; the
//! balance is the sum. `debit` refuses to take a user below zero unless the user has
//! `allow_negative` (the admin override), which is the "ledger never negative" invariant.

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use arbos_core::ids::now_ms;

use crate::db::Db;

pub const ACCOUNTS_SCHEMA: &str = r#"
CREATE UNIQUE INDEX IF NOT EXISTS users_github ON users(github_id) WHERE github_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS auth_flows (
  state TEXT PRIMARY KEY, code_verifier TEXT NOT NULL, redirect TEXT NOT NULL,
  device_name TEXT NOT NULL, invite TEXT, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS auth_grants (
  code_hash TEXT PRIMARY KEY, device_id TEXT NOT NULL, user_id TEXT NOT NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER
);
CREATE TABLE IF NOT EXISTS refresh_tokens (
  token_hash TEXT PRIMARY KEY, device_id TEXT NOT NULL, user_id TEXT NOT NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, rotated_at INTEGER, revoked_at INTEGER
);
CREATE INDEX IF NOT EXISTS refresh_tokens_device ON refresh_tokens(device_id);
CREATE TABLE IF NOT EXISTS credit_ledger (
  id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, at INTEGER NOT NULL,
  kind TEXT NOT NULL, usd REAL NOT NULL, ref TEXT, note TEXT, actor TEXT NOT NULL DEFAULT 'relay'
);
CREATE INDEX IF NOT EXISTS credit_ledger_user_at ON credit_ledger(user_id, at);
CREATE INDEX IF NOT EXISTS credit_ledger_at ON credit_ledger(at);
CREATE TABLE IF NOT EXISTS machine_meter (
  user_id TEXT PRIMARY KEY, last_tick_at INTEGER NOT NULL, last_state TEXT NOT NULL
);
"#;

/// Additive columns (SQLite has no `ADD COLUMN IF NOT EXISTS`; failures mean "already there").
pub fn migrate_accounts(c: &Connection) {
    for stmt in [
        "ALTER TABLE users ADD COLUMN github_id INTEGER",
        "ALTER TABLE users ADD COLUMN login TEXT",
        "ALTER TABLE users ADD COLUMN avatar_url TEXT",
        "ALTER TABLE users ADD COLUMN name TEXT",
        "ALTER TABLE users ADD COLUMN last_login_at INTEGER",
        "ALTER TABLE users ADD COLUMN cap_unlimited INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE users ADD COLUMN allow_negative INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE devices ADD COLUMN kind TEXT NOT NULL DEFAULT 'paircode'",
    ] {
        let _ = c.execute(stmt, []);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub github_id: Option<i64>,
    pub login: Option<String>,
    pub avatar_url: Option<String>,
    pub name: Option<String>,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
    /// Daily debit cap in USD; `None` = unlimited (admin).
    pub daily_cap_usd: Option<f64>,
    pub allow_negative: bool,
}

/// What GitHub tells us about the signed-in account.
#[derive(Debug, Clone)]
pub struct GithubIdentity {
    pub id: i64,
    pub login: String,
    pub avatar_url: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthFlow {
    pub state: String,
    pub code_verifier: String,
    pub redirect: String,
    pub device_name: String,
    pub invite: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RefreshRow {
    pub device_id: String,
    pub user_id: String,
    pub expires_at: i64,
    pub rotated_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Credits {
    pub balance_usd: f64,
    pub granted_usd: f64,
    pub spent_usd: f64,
    pub spent_today_usd: f64,
    pub daily_cap_usd: Option<f64>,
    pub allow_negative: bool,
}

/// Why a debit was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum DebitRefused {
    /// Balance would go below zero and the user has no override.
    Insufficient { balance_usd: f64, needed_usd: f64 },
    /// Today's debits already reach the user's daily cap.
    DailyCap { cap_usd: f64, spent_today_usd: f64 },
    /// The global kill switch is on.
    ReadOnly { reason: String },
}

impl std::fmt::Display for DebitRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DebitRefused::Insufficient {
                balance_usd,
                needed_usd,
            } => write!(
                f,
                "credits exhausted: balance ${balance_usd:.4}, needed ${needed_usd:.4}"
            ),
            DebitRefused::DailyCap {
                cap_usd,
                spent_today_usd,
            } => write!(
                f,
                "daily cap ${cap_usd:.2} reached (spent ${spent_today_usd:.2} today)"
            ),
            DebitRefused::ReadOnly { reason } => write!(f, "read-only: {reason}"),
        }
    }
}

/// Kill switch record kept in `meta` under `kill_switch`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KillSwitch {
    pub on: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub at: i64,
    /// `true` when tripped by the global daily cap; it clears at the next UTC day.
    #[serde(default)]
    pub auto: bool,
}

const KILL_SWITCH_KEY: &str = "kill_switch";

fn user_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    let cap: f64 = r.get(7)?;
    let unlimited: i64 = r.get(8)?;
    Ok(User {
        id: r.get(0)?,
        github_id: r.get(1)?,
        login: r.get(2)?,
        avatar_url: r.get(3)?,
        name: r.get(4)?,
        created_at: r.get(5)?,
        last_login_at: r.get(6)?,
        daily_cap_usd: if unlimited != 0 { None } else { Some(cap) },
        allow_negative: r.get::<_, i64>(9)? != 0,
    })
}

const USER_COLS: &str = "id, github_id, login, avatar_url, name, created_at, last_login_at, spend_cap_usd, cap_unlimited, allow_negative";

impl Db {
    // ---- users --------------------------------------------------------------------------------

    pub fn user(&self, id: &str) -> Result<Option<User>> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {USER_COLS} FROM users WHERE id = ?1"),
                params![id],
                user_from_row,
            )
            .optional()
        })
    }

    pub fn user_by_github_id(&self, github_id: i64) -> Result<Option<User>> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {USER_COLS} FROM users WHERE github_id = ?1"),
                params![github_id],
                user_from_row,
            )
            .optional()
        })
    }

    /// Insert or refresh the user behind a GitHub account. Returns `(user, created)`; the id is
    /// `gh_<github id>`, so one GitHub account is one user (and one machine) forever.
    pub fn upsert_github_user(
        &self,
        gh: &GithubIdentity,
        default_daily_cap_usd: f64,
    ) -> Result<(User, bool)> {
        let id = format!("gh_{}", gh.id);
        let now = now_ms();
        let created = self.with(|c| {
            let existing: Option<String> = c
                .query_row(
                    "SELECT status FROM users WHERE github_id = ?1",
                    params![gh.id],
                    |r| r.get(0),
                )
                .optional()?;
            match existing.as_deref() {
                Some(status) => {
                    // An `invited` placeholder (someone opened a DM with this login before it
                    // ever signed in) becomes a member now: first sign-in, with the welcome grant.
                    let first = status == "invited";
                    c.execute(
                        "UPDATE users SET login = ?2, avatar_url = ?3, name = ?4, last_login_at = ?5, status = 'member',
                            spend_cap_usd = CASE WHEN status = 'invited' THEN ?6 ELSE spend_cap_usd END
                         WHERE github_id = ?1",
                        params![gh.id, gh.login, gh.avatar_url, gh.name, now, default_daily_cap_usd],
                    )?;
                    Ok(first)
                }
                None => {
                    c.execute(
                        "INSERT INTO users(id, snapshot, spend_cap_usd, created_at, github_id, login, avatar_url, name, last_login_at, status)
                         VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?3, 'member')",
                        params![id, default_daily_cap_usd, now, gh.id, gh.login, gh.avatar_url, gh.name],
                    )?;
                    Ok(true)
                }
            }
        })?;
        let user = self
            .user_by_github_id(gh.id)?
            .ok_or_else(|| anyhow!("user vanished"))?;
        Ok((user, created))
    }

    pub fn list_users(&self) -> Result<Vec<User>> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {USER_COLS} FROM users ORDER BY created_at"
            ))?;
            let rows = st.query_map([], user_from_row)?;
            rows.collect()
        })
    }

    /// Admin: daily cap (`None` = unlimited) and the negative-balance override.
    pub fn set_user_limits(
        &self,
        user_id: &str,
        daily_cap_usd: Option<Option<f64>>,
        allow_negative: Option<bool>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO users(id, created_at) VALUES (?1, ?2)",
                params![user_id, now_ms()],
            )?;
            if let Some(cap) = daily_cap_usd {
                match cap {
                    Some(v) => c.execute(
                        "UPDATE users SET spend_cap_usd = ?2, cap_unlimited = 0 WHERE id = ?1",
                        params![user_id, v],
                    )?,
                    None => c.execute(
                        "UPDATE users SET cap_unlimited = 1 WHERE id = ?1",
                        params![user_id],
                    )?,
                };
            }
            if let Some(neg) = allow_negative {
                c.execute(
                    "UPDATE users SET allow_negative = ?2 WHERE id = ?1",
                    params![user_id, neg as i64],
                )?;
            }
            Ok(())
        })
    }

    // ---- sign-in flows and grants -------------------------------------------------------------

    pub fn insert_auth_flow(&self, f: &AuthFlow, ttl_ms: i64) -> Result<()> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "DELETE FROM auth_flows WHERE expires_at < ?1",
                params![now],
            )?;
            c.execute(
                "INSERT INTO auth_flows(state, code_verifier, redirect, device_name, invite, created_at, expires_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![f.state, f.code_verifier, f.redirect, f.device_name, f.invite, now, now + ttl_ms],
            )
            .map(|_| ())
        })
    }

    /// Consume a flow by state (one use).
    pub fn take_auth_flow(&self, state: &str) -> Result<Option<AuthFlow>> {
        let now = now_ms();
        self.with(|c| {
            let row = c
                .query_row(
                    "SELECT state, code_verifier, redirect, device_name, invite FROM auth_flows WHERE state = ?1 AND expires_at > ?2",
                    params![state, now],
                    |r| {
                        Ok(AuthFlow {
                            state: r.get(0)?,
                            code_verifier: r.get(1)?,
                            redirect: r.get(2)?,
                            device_name: r.get(3)?,
                            invite: r.get(4)?,
                        })
                    },
                )
                .optional()?;
            c.execute("DELETE FROM auth_flows WHERE state = ?1", params![state])?;
            Ok(row)
        })
    }

    /// A signed-in device row (no long-lived token; the JWT carries the id).
    pub fn insert_device(
        &self,
        device_id: &str,
        name: &str,
        token_hash: &str,
        user_id: &str,
        kind: &str,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO devices(id, name, token_hash, created_at, user_id, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![device_id, name, token_hash, now_ms(), user_id, kind],
            )
            .map(|_| ())
        })
    }

    /// Device row as `(user_id, revoked)`; `None` when the id is unknown.
    pub fn device_status(&self, device_id: &str) -> Result<Option<(String, bool)>> {
        self.with(|c| {
            c.query_row(
                "SELECT user_id, revoked_at IS NOT NULL FROM devices WHERE id = ?1",
                params![device_id],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .optional()
        })
    }

    pub fn touch_device(&self, device_id: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE devices SET last_seen_at = ?2 WHERE id = ?1",
                params![device_id, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn devices_of_user(&self, user_id: &str) -> Result<Vec<serde_json::Value>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, name, created_at, revoked_at, last_seen_at, kind FROM devices WHERE user_id = ?1 ORDER BY created_at",
            )?;
            let rows = st.query_map(params![user_id], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "created_at": r.get::<_, i64>(2)?,
                    "revoked_at": r.get::<_, Option<i64>>(3)?, "last_seen_at": r.get::<_, Option<i64>>(4)?,
                    "kind": r.get::<_, String>(5)?,
                }))
            })?;
            rows.collect()
        })
    }

    pub fn insert_auth_grant(
        &self,
        code_hash: &str,
        device_id: &str,
        user_id: &str,
        ttl_ms: i64,
    ) -> Result<()> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "DELETE FROM auth_grants WHERE expires_at < ?1",
                params![now - 3_600_000],
            )?;
            c.execute(
                "INSERT INTO auth_grants(code_hash, device_id, user_id, created_at, expires_at) VALUES (?1,?2,?3,?4,?5)",
                params![code_hash, device_id, user_id, now, now + ttl_ms],
            )
            .map(|_| ())
        })
    }

    /// Redeem a grant code once: `(device_id, user_id)`.
    pub fn take_auth_grant(&self, code_hash: &str) -> Result<Option<(String, String)>> {
        let now = now_ms();
        self.with(|c| {
            let n = c.execute(
                "UPDATE auth_grants SET used_at = ?2 WHERE code_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
                params![code_hash, now],
            )?;
            if n == 0 {
                return Ok(None);
            }
            c.query_row(
                "SELECT device_id, user_id FROM auth_grants WHERE code_hash = ?1",
                params![code_hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })
    }

    // ---- refresh tokens -----------------------------------------------------------------------

    pub fn insert_refresh(
        &self,
        token_hash: &str,
        device_id: &str,
        user_id: &str,
        ttl_ms: i64,
    ) -> Result<()> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO refresh_tokens(token_hash, device_id, user_id, created_at, expires_at) VALUES (?1,?2,?3,?4,?5)",
                params![token_hash, device_id, user_id, now, now + ttl_ms],
            )
            .map(|_| ())
        })
    }

    pub fn refresh_row(&self, token_hash: &str) -> Result<Option<RefreshRow>> {
        self.with(|c| {
            c.query_row(
                "SELECT device_id, user_id, expires_at, rotated_at, revoked_at FROM refresh_tokens WHERE token_hash = ?1",
                params![token_hash],
                |r| {
                    Ok(RefreshRow {
                        device_id: r.get(0)?,
                        user_id: r.get(1)?,
                        expires_at: r.get(2)?,
                        rotated_at: r.get(3)?,
                        revoked_at: r.get(4)?,
                    })
                },
            )
            .optional()
        })
    }

    pub fn mark_refresh_rotated(&self, token_hash: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE refresh_tokens SET rotated_at = ?2 WHERE token_hash = ?1",
                params![token_hash, now_ms()],
            )
            .map(|_| ())
        })
    }

    /// Revoke a device and every refresh token it holds (logout, admin revoke, token reuse).
    pub fn revoke_device_fully(&self, device_id: &str) -> Result<bool> {
        let now = now_ms();
        self.with(|c| {
            c.execute(
                "UPDATE refresh_tokens SET revoked_at = ?2 WHERE device_id = ?1 AND revoked_at IS NULL",
                params![device_id, now],
            )?;
            c.execute(
                "UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![device_id, now],
            )
            .map(|n| n > 0)
        })
    }

    // ---- credit ledger ------------------------------------------------------------------------

    pub fn grant_credits(&self, user_id: &str, usd: f64, note: &str, actor: &str) -> Result<()> {
        if !usd.is_finite() || usd <= 0.0 {
            return Err(anyhow!("grant must be a positive amount"));
        }
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO users(id, created_at) VALUES (?1, ?2)",
                params![user_id, now_ms()],
            )?;
            c.execute(
                "INSERT INTO credit_ledger(user_id, at, kind, usd, note, actor) VALUES (?1, ?2, 'grant', ?3, ?4, ?5)",
                params![user_id, now_ms(), usd, note, actor],
            )
            .map(|_| ())
        })
    }

    /// Debits across every user since `since_ms` (the global cap).
    pub fn debited_since_all(&self, since_ms: i64) -> Result<f64> {
        self.with(|c| {
            c.query_row(
                "SELECT COALESCE(-SUM(usd), 0.0) FROM credit_ledger WHERE usd < 0 AND at >= ?1",
                params![since_ms],
                |r| r.get(0),
            )
        })
    }

    /// Take `usd` from the user, atomically checked against the balance (unless the user has
    /// the negative override) and the daily cap. The kill switch is checked by the caller
    /// (`credits::check`), because it is relay-wide, not per user.
    pub fn debit(
        &self,
        user_id: &str,
        usd: f64,
        kind: &str,
        reference: Option<&str>,
        start_of_day_ms: i64,
    ) -> Result<std::result::Result<f64, DebitRefused>> {
        if !usd.is_finite() || usd < 0.0 {
            return Err(anyhow!("debit must be a non-negative amount"));
        }
        self.with(|c| {
            let (cap, unlimited, allow_negative): (f64, i64, i64) = c
                .query_row(
                    "SELECT spend_cap_usd, cap_unlimited, allow_negative FROM users WHERE id = ?1",
                    params![user_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .unwrap_or((0.0, 0, 0));
            let balance: f64 = c.query_row(
                "SELECT COALESCE(SUM(usd), 0.0) FROM credit_ledger WHERE user_id = ?1",
                params![user_id],
                |r| r.get(0),
            )?;
            if allow_negative == 0 && balance - usd < -1e-9 {
                return Ok(Err(DebitRefused::Insufficient {
                    balance_usd: balance,
                    needed_usd: usd,
                }));
            }
            if unlimited == 0 {
                let today: f64 = c.query_row(
                    "SELECT COALESCE(-SUM(usd), 0.0) FROM credit_ledger WHERE user_id = ?1 AND usd < 0 AND at >= ?2",
                    params![user_id, start_of_day_ms],
                    |r| r.get(0),
                )?;
                if today >= cap {
                    return Ok(Err(DebitRefused::DailyCap {
                        cap_usd: cap,
                        spent_today_usd: today,
                    }));
                }
            }
            if usd > 0.0 {
                c.execute(
                    "INSERT INTO credit_ledger(user_id, at, kind, usd, ref) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![user_id, now_ms(), kind, -usd, reference],
                )?;
            }
            Ok(Ok(balance - usd))
        })
    }

    pub fn credits(&self, user_id: &str, start_of_day_ms: i64) -> Result<Credits> {
        let user = self.user(user_id)?;
        self.with(|c| {
            let (granted, spent): (f64, f64) = c.query_row(
                "SELECT COALESCE(SUM(CASE WHEN usd > 0 THEN usd ELSE 0 END), 0.0),
                        COALESCE(SUM(CASE WHEN usd < 0 THEN -usd ELSE 0 END), 0.0)
                 FROM credit_ledger WHERE user_id = ?1",
                params![user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let today: f64 = c.query_row(
                "SELECT COALESCE(-SUM(usd), 0.0) FROM credit_ledger WHERE user_id = ?1 AND usd < 0 AND at >= ?2",
                params![user_id, start_of_day_ms],
                |r| r.get(0),
            )?;
            Ok(Credits {
                balance_usd: granted - spent,
                granted_usd: granted,
                spent_usd: spent,
                spent_today_usd: today,
                daily_cap_usd: user.as_ref().and_then(|u| u.daily_cap_usd),
                allow_negative: user.as_ref().is_some_and(|u| u.allow_negative),
            })
        })
    }

    // ---- kill switch --------------------------------------------------------------------------

    pub fn kill_switch(&self) -> Result<KillSwitch> {
        Ok(self
            .meta_get(KILL_SWITCH_KEY)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    pub fn set_kill_switch(&self, ks: &KillSwitch) -> Result<()> {
        self.meta_set(KILL_SWITCH_KEY, &serde_json::to_string(ks)?)
    }

    // ---- machine metering ---------------------------------------------------------------------

    /// Last metering tick for a user's box: `(at_ms, state)`.
    pub fn meter_tick(&self, user_id: &str) -> Result<Option<(i64, String)>> {
        self.with(|c| {
            c.query_row(
                "SELECT last_tick_at, last_state FROM machine_meter WHERE user_id = ?1",
                params![user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })
    }

    pub fn set_meter_tick(&self, user_id: &str, at: i64, state: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO machine_meter(user_id, last_tick_at, last_state) VALUES (?1, ?2, ?3)
                 ON CONFLICT(user_id) DO UPDATE SET last_tick_at = excluded.last_tick_at, last_state = excluded.last_state",
                params![user_id, at, state],
            )
            .map(|_| ())
        })
    }
}
