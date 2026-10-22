//! Continuous delivery: what is installable, and what is installed.
//!
//! - `POST /admin/release {platform, channel, version, build, url}` (admin token) — the Release
//!   workflow (Mac) and `ios.yml` (TestFlight, after distribute) say "this build is installable".
//!   Stored as the current build of that platform/channel and fanned out as the push kind
//!   `release_published` on `/me/pushes` to every live device, so a Mac checks its appcast at
//!   once instead of waiting for its next poll. A stable Mac release also resets the
//!   `/download/mac` redirect cache and touches `<data>/release-published`, which the relay
//!   host's `arbos-refresh-mac-download.path` unit turns into an immediate refresh.
//! - `GET /releases/current` (device) — the current builds, one per platform/channel.
//! - `GET /admin/installs[?since=<ms>]` (admin token) — one row per device and app version with
//!   `installed_at` = the first time the relay saw that device on that version. The feedback loop
//!   reads it for its merge → installed lead time.
//!
//! Where the installed signal comes from: every Mac/iPhone request carries
//! `X-Arbos-Client: mac/0.6.30`, `mac/0.6.29 (1084)` or `ios/1.0 (202610192230)`;
//! [`note_client`] is called from the `Principal` extractor and records version changes (one DB
//! write per device per version, then a `last_seen_at` refresh at most every 5 minutes).

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

use arbos_core::api::{
    PushEventDto, ReleaseDto, ReleasePublishedPayloadDto, ReleasesDto,
};
use arbos_core::ids::now_ms;

use crate::auth::Principal;
use crate::db::Db;
use crate::signin::api_error;
use crate::state::AppState;

pub const RELEASES_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS releases (
  platform TEXT NOT NULL, channel TEXT NOT NULL, version TEXT NOT NULL, build TEXT NOT NULL DEFAULT '',
  url TEXT NOT NULL DEFAULT '', published_at INTEGER NOT NULL,
  PRIMARY KEY (platform, channel)
);
CREATE TABLE IF NOT EXISTS device_versions (
  device_id TEXT NOT NULL, user_id TEXT NOT NULL, platform TEXT NOT NULL,
  app_version TEXT NOT NULL, build TEXT NOT NULL DEFAULT '',
  first_seen_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL,
  PRIMARY KEY (device_id, platform, app_version, build)
);
CREATE INDEX IF NOT EXISTS device_versions_seen ON device_versions(first_seen_at);
"#;

/// device_id → (platform, version, build, last write ms): what was last written for a device,
/// so a request only touches the DB when the version changes or the row is stale.
pub type ClientSeen = Mutex<HashMap<String, (String, String, String, i64)>>;

const REFRESH_EVERY_MS: i64 = 5 * 60_000;

/// `mac/0.6.29 (1084)` → `("mac", "0.6.29", "1084")`; `ios/1.0 (2026…)` → `("ios", "1.0", "2026…")`.
pub fn parse_client(header: &str) -> Option<(String, String, String)> {
    let h = header.trim();
    let (platform, rest) = h.split_once('/')?;
    let platform = platform.trim().to_ascii_lowercase();
    if platform.is_empty() || platform.len() > 20 {
        return None;
    }
    let rest = rest.trim();
    let (version, build) = match rest.split_once('(') {
        Some((v, b)) => (v.trim(), b.trim_end_matches(')').trim()),
        None => (rest, ""),
    };
    if version.is_empty() || version.len() > 40 || build.len() > 40 {
        return None;
    }
    Some((platform, version.to_owned(), build.to_owned()))
}

/// Record what a device runs. Cheap when nothing changed (one mutex lookup).
pub fn note_client(state: &AppState, device_id: &str, user_id: &str, header: &str) {
    let Some((platform, version, build)) = parse_client(header) else {
        return;
    };
    let now = now_ms();
    {
        let mut seen = state.client_seen.lock().unwrap();
        if let Some((p, v, b, at)) = seen.get(device_id) {
            if *p == platform && *v == version && *b == build && now - *at < REFRESH_EVERY_MS {
                return;
            }
        }
        seen.insert(
            device_id.to_owned(),
            (platform.clone(), version.clone(), build.clone(), now),
        );
    }
    if let Err(e) = state
        .db
        .note_device_version(device_id, user_id, &platform, &version, &build, now)
    {
        tracing::warn!(device_id, error = %e, "device version not recorded");
    }
}

impl Db {
    pub fn note_device_version(
        &self,
        device_id: &str,
        user_id: &str,
        platform: &str,
        version: &str,
        build: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO device_versions(device_id, user_id, platform, app_version, build, first_seen_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(device_id, platform, app_version, build) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                params![device_id, user_id, platform, version, build, now],
            )
            .map(|_| ())
        })
    }

    /// Every (device, version) row first seen at or after `since`, newest first.
    pub fn installs_since(&self, since: i64) -> anyhow::Result<Vec<Value>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT v.device_id, v.user_id, u.login, d.name, v.platform, v.app_version, v.build, v.first_seen_at, v.last_seen_at
                 FROM device_versions v
                 LEFT JOIN devices d ON d.id = v.device_id
                 LEFT JOIN users u ON u.id = v.user_id
                 WHERE v.first_seen_at >= ?1
                 ORDER BY v.first_seen_at DESC",
            )?;
            let rows = st.query_map(params![since], |r| {
                Ok(json!({
                    "device_id": r.get::<_, String>(0)?,
                    "user_id": r.get::<_, String>(1)?,
                    "login": r.get::<_, Option<String>>(2)?,
                    "device_name": r.get::<_, Option<String>>(3)?,
                    "platform": r.get::<_, String>(4)?,
                    "app_version": r.get::<_, String>(5)?,
                    "build": r.get::<_, String>(6)?,
                    "installed_at": r.get::<_, i64>(7)?,
                    "last_seen_at": r.get::<_, i64>(8)?,
                }))
            })?;
            rows.collect()
        })
    }

    pub fn set_release(
        &self,
        platform: &str,
        channel: &str,
        version: &str,
        build: &str,
        url: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO releases(platform, channel, version, build, url, published_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(platform, channel) DO UPDATE SET version = excluded.version, build = excluded.build,
                   url = excluded.url, published_at = excluded.published_at",
                params![platform, channel, version, build, url, now],
            )
            .map(|_| ())
        })
    }

    pub fn all_releases(&self) -> anyhow::Result<Vec<ReleaseDto>> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT platform, channel, version, build, url, published_at FROM releases ORDER BY platform, channel",
            )?;
            let rows = st.query_map([], |r| {
                Ok(ReleaseDto {
                    platform: r.get(0)?,
                    channel: r.get(1)?,
                    version: r.get(2)?,
                    build: r.get(3)?,
                    url: r.get(4)?,
                    published_at: r.get(5)?,
                })
            })?;
            rows.collect()
        })
    }

    /// Queue `kind` for every live device of every user (release announcements).
    pub fn push_everyone(&self, kind: &str, payload: &Value) -> anyhow::Result<usize> {
        let now = now_ms();
        let body = payload.to_string();
        self.with(|c| {
            c.execute(
                "INSERT INTO user_pushes(user_id, device_id, kind, payload, chat_id, created_at)
                 SELECT user_id, id, ?1, ?2, NULL, ?3 FROM devices WHERE revoked_at IS NULL",
                params![kind, body, now],
            )
        })
    }
}

#[derive(Deserialize)]
pub struct ReleaseBody {
    pub platform: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    pub version: String,
    #[serde(default)]
    pub build: String,
    #[serde(default)]
    pub url: String,
}

fn default_channel() -> String {
    "stable".into()
}

fn clean(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

/// `POST /admin/release`: a build is installable. Idempotent — the same version twice stores the
/// same row and pushes again (the apps only act when the version is newer than what they run).
pub async fn admin_release(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<ReleaseBody>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    let platform = clean(&b.platform, 20).to_ascii_lowercase();
    let channel = clean(&b.channel, 20).to_ascii_lowercase();
    let version = clean(&b.version, 40);
    let build = clean(&b.build, 40);
    let url = clean(&b.url, 400);
    if !matches!(platform.as_str(), "mac" | "ios") || version.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_release",
            "platform must be mac or ios and version must be set",
        );
    }
    let now = now_ms();
    if let Err(e) = state
        .db
        .set_release(&platform, &channel, &version, &build, &url, now)
    {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    if platform == "mac" && channel == "stable" {
        // /download/mac must point at the new DMG at once: drop the relay's own redirect cache
        // and touch the flag that arbos-refresh-mac-download.path watches on the relay host
        // (the 15-minute timer stays as the fallback).
        *state.download_cache.lock().await = None;
        let flag = state.cfg.data_dir.join("release-published");
        if let Err(e) = std::fs::write(&flag, format!("{platform} {channel} {version} {now}\n")) {
            tracing::warn!(error = %e, "release-published flag not written");
        }
    }
    let release = ReleasePublishedPayloadDto {
        platform: platform.clone(),
        channel: channel.clone(),
        version: version.clone(),
        build: build.clone(),
        url: url.clone(),
        published_at: now,
    };
    let event = PushEventDto::ReleasePublished(release.clone());
    let (kind, payload) = event.kind_and_payload();
    let pushed = match state.db.push_everyone(&kind, &payload) {
        Ok(n) => n,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    };
    tracing::info!(
        platform,
        channel,
        version,
        build,
        pushed,
        "release published"
    );
    let release_row = ReleaseDto {
        platform,
        channel,
        version,
        build,
        url,
        published_at: now,
    };
    Json(json!({ "ok": true, "pushed_devices": pushed, "release": release_row })).into_response()
}

/// `GET /releases/current`: the current builds per platform/channel, for the apps' own check.
pub async fn current(State(state): State<AppState>, principal: Principal) -> Response {
    if !matches!(principal, Principal::Device { .. } | Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "devices only");
    }
    match state.db.all_releases() {
        Ok(list) => Json(ReleasesDto { releases: list }).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

#[derive(Deserialize, Default)]
pub struct InstallsQuery {
    pub since: Option<i64>,
}

/// `GET /admin/installs?since=<ms>`: per device and version, when it was first seen.
pub async fn admin_installs(
    State(state): State<AppState>,
    principal: Principal,
    Query(q): Query<InstallsQuery>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    match state.db.installs_since(q.since.unwrap_or(0)) {
        Ok(rows) => Json(json!({ "installs": rows })).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_client_headers() {
        assert_eq!(
            parse_client("mac/0.6.30"),
            Some(("mac".into(), "0.6.30".into(), String::new()))
        );
        assert_eq!(
            parse_client("mac/0.6.29 (1084)"),
            Some(("mac".into(), "0.6.29".into(), "1084".into()))
        );
        assert_eq!(
            parse_client("ios/1.0 (202610192230)"),
            Some(("ios".into(), "1.0".into(), "202610192230".into()))
        );
        assert_eq!(parse_client("garbage"), None);
        assert_eq!(parse_client("mac/"), None);
    }

    #[test]
    fn records_versions_and_releases() {
        let state = crate::state::test_support::test_state(crate::state::AuthMode::Github);
        state
            .db
            .note_device_version("dev1", "u1", "mac", "0.6.29", "1084", 1_000)
            .unwrap();
        state
            .db
            .note_device_version("dev1", "u1", "mac", "0.6.29", "1084", 2_000)
            .unwrap();
        state
            .db
            .note_device_version("dev1", "u1", "mac", "0.6.30", "", 3_000)
            .unwrap();
        let rows = state.db.installs_since(0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["app_version"], "0.6.30");
        assert_eq!(rows[0]["installed_at"], 3_000);
        assert_eq!(rows[1]["installed_at"], 1_000);
        assert_eq!(rows[1]["last_seen_at"], 2_000);
        assert_eq!(state.db.installs_since(2_500).unwrap().len(), 1);

        state
            .db
            .set_release("mac", "stable", "0.6.30", "", "https://x", 5_000)
            .unwrap();
        state
            .db
            .set_release("mac", "stable", "0.6.31", "", "https://y", 6_000)
            .unwrap();
        let all = state.db.all_releases().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["version"], "0.6.31");
    }
}
