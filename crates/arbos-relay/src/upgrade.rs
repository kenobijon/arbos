//! Box upgrade: recreate a user's Daytona box from the current default snapshot and carry `/work`
//! over. agentd is baked into the image, so a box made from an older snapshot never gets new
//! agent features until it is upgraded.
//!
//! Steps (one task per user, idempotent while it runs):
//!   pack   — start the old sandbox if needed, stop its agentd, tar `/work` (logs excluded), pull
//!            the tarball through the relay;
//!   stop   — hibernate the old sandbox and record it in `box_history` (kept 7 days as fallback);
//!   create — a fresh box from the target snapshot (new tokens; the `boxes` row is replaced);
//!   unpack — stop the new agentd, extract `/work`, reset the per-workspace owner markers;
//!   start  — bounce the sandbox so the entrypoint starts agentd on the carried `/work`.
//! Progress is in `GET /machine` (`upgrade.phase`); failures leave the old box as the user's box
//! again when the new one was never created, else the new box with whatever was unpacked.

use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use arbos_core::ids::now_ms;
use arbos_core::wire::BoxState;

use crate::auth::Principal;
use crate::boxes::{self, Provider};
use crate::daytona::SandboxState;
use crate::signin::api_error;
use crate::state::{AppState, UpgradeStatus};

const KEEP_OLD_SANDBOX_MS: i64 = 7 * 24 * 3_600_000;
const MAX_WORK_MIB: i64 = 3500;

/// Is the box behind the relay's default snapshot?
pub fn image_outdated(state: &AppState, sb: &crate::db::Sandbox) -> bool {
    sb.provider == "daytona"
        && sb
            .snapshot
            .as_deref()
            .is_some_and(|s| s != state.cfg.daytona_snapshot)
}

/// `upgrade` object for `GET /machine` and `/admin/users`.
pub async fn status_json(state: &AppState, user_id: &str) -> Value {
    let running = state.upgrading.lock().await.get(user_id).cloned();
    match running {
        Some(u) => json!({
            "in_progress": true, "phase": u.phase, "since": u.since,
            "from": u.from_snapshot, "to": u.to_snapshot,
        }),
        None => json!({ "in_progress": false }),
    }
}

async fn set_phase(state: &AppState, user_id: &str, phase: &str) {
    if let Some(u) = state.upgrading.lock().await.get_mut(user_id) {
        u.phase = phase.to_owned();
    }
    state
        .set_box_state(
            user_id,
            BoxState::Waking,
            Some(&format!("upgrading: {phase}")),
        )
        .await;
}

/// Start an upgrade for `user_id` in the background. `Ok(false)` when one is already running
/// or the box is already on the target snapshot.
pub async fn start(state: &AppState, user_id: &str, force: bool) -> Result<bool> {
    let sb = state
        .db
        .user_box(user_id)?
        .ok_or_else(|| anyhow!("no box for user {user_id}"))?;
    if Provider::parse(&sb.provider) != Some(Provider::Daytona) {
        return Err(anyhow!(
            "only Daytona boxes can be upgraded (this one is {})",
            sb.provider
        ));
    }
    let target = state
        .db
        .user_snapshot(user_id)?
        .unwrap_or_else(|| state.cfg.daytona_snapshot.clone());
    if !force && sb.snapshot.as_deref() == Some(target.as_str()) {
        return Ok(false);
    }
    {
        let mut running = state.upgrading.lock().await;
        if running.contains_key(user_id) {
            return Ok(false);
        }
        running.insert(
            user_id.to_owned(),
            UpgradeStatus {
                phase: "queued".into(),
                since: now_ms(),
                from_snapshot: sb.snapshot.clone(),
                to_snapshot: target.clone(),
            },
        );
    }
    let state2 = state.clone();
    let user = user_id.to_owned();
    tokio::spawn(async move {
        let t0 = Instant::now();
        let outcome = run(&state2, &user, &target).await;
        state2.upgrading.lock().await.remove(&user);
        match outcome {
            Ok(report) => {
                tracing::info!(user = %user, ?report, ms = t0.elapsed().as_millis() as i64, "box upgraded");
                let _ = state2.db.record_audit(
                    "relay-admin",
                    "box_upgrade",
                    &json!({ "ts": now_ms(), "user_id": user, "report": report, "ms": t0.elapsed().as_millis() as i64 }),
                );
            }
            Err(e) => {
                tracing::error!(user = %user, error = %format!("{e:#}"), "box upgrade failed");
                let _ = state2.db.record_audit(
                    "relay-admin",
                    "box_upgrade_failed",
                    &json!({ "ts": now_ms(), "user_id": user, "error": format!("{e:#}") }),
                );
                state2
                    .set_box_state(
                        &user,
                        BoxState::Error,
                        Some(&format!("upgrade failed: {e}")),
                    )
                    .await;
            }
        }
    });
    Ok(true)
}

async fn run(state: &AppState, user_id: &str, target: &str) -> Result<Value> {
    let dt = state
        .daytona
        .as_ref()
        .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
    let old = state
        .db
        .user_box(user_id)?
        .ok_or_else(|| anyhow!("box vanished"))?;
    let old_id = old
        .sandbox_id
        .clone()
        .ok_or_else(|| anyhow!("box has no sandbox id"))?;

    // ---- pack --------------------------------------------------------------------------------
    set_phase(state, user_id, "pack").await;
    let cur = dt.get(&old_id).await.context("old sandbox")?;
    if matches!(
        cur.state,
        SandboxState::Destroyed | SandboxState::Destroying
    ) {
        return Err(anyhow!(
            "old sandbox {old_id} is destroyed; nothing to carry over"
        ));
    }
    if cur.state != SandboxState::Started {
        dt.start(&old_id).await?;
        dt.wait_for(&old_id, Duration::from_secs(180), |s| {
            s.state == SandboxState::Started
        })
        .await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let _ = dt
        .toolbox_exec(
            &old_id,
            "sudo -n pkill -TERM -x arbos-agentd 2>/dev/null; sleep 3; sudo -n pkill -KILL -x arbos-agentd 2>/dev/null; sudo -n pkill -KILL -f 'pi --mode rpc' 2>/dev/null; sudo -n pkill -KILL -x prime-agent 2>/dev/null; true",
            60,
        )
        .await;
    let du = dt
        .toolbox_exec(&old_id, "sudo -n du -sm /work | cut -f1", 120)
        .await?;
    let mib: i64 = du
        .get("result")
        .and_then(Value::as_str)
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if mib > MAX_WORK_MIB {
        return Err(anyhow!(
            "/work is {mib} MiB; too large to carry through the relay in one piece"
        ));
    }
    let tar = dt
        .toolbox_exec(
            &old_id,
            "sudo -n rm -f /home/daytona/arbos-upgrade.tgz; cd /work && sudo -n tar czf /home/daytona/arbos-upgrade.tgz --exclude='./.arbos/logs' --exclude='./*/.arbos/logs' . && sudo -n chmod 644 /home/daytona/arbos-upgrade.tgz && ls -l /home/daytona/arbos-upgrade.tgz",
            900,
        )
        .await?;
    if tar.get("exitCode").and_then(Value::as_i64).unwrap_or(1) != 0 {
        return Err(anyhow!(
            "tar on the old box failed: {}",
            tar.get("result").and_then(Value::as_str).unwrap_or("")
        ));
    }
    let bytes = dt
        .toolbox_download(&old_id, "/home/daytona/arbos-upgrade.tgz")
        .await
        .context("download /work tarball")?;
    let _ = dt
        .toolbox_exec(&old_id, "sudo -n rm -f /home/daytona/arbos-upgrade.tgz", 60)
        .await;
    tracing::info!(user = %user_id, mib, bytes = bytes.len(), "old /work packed");

    // ---- stop the old box, keep it 7 days ----------------------------------------------------
    set_phase(state, user_id, "stop").await;
    let _ = dt.stop(&old_id, false).await;
    state.db.insert_box_history(
        user_id,
        &old_id,
        old.snapshot.as_deref(),
        now_ms() + KEEP_OLD_SANDBOX_MS,
    )?;

    // ---- create the new box ------------------------------------------------------------------
    set_phase(state, user_id, "create").await;
    let mut new = boxes::create_box(state, user_id, Provider::Daytona, Some(target))
        .await
        .context("create the new box")?;
    new.migrated_from = old.migrated_from.clone();
    state.db.upsert_box(&new)?;
    let new_id = new
        .sandbox_id
        .clone()
        .ok_or_else(|| anyhow!("new box has no sandbox id"))?;

    // ---- unpack -------------------------------------------------------------------------------
    set_phase(state, user_id, "unpack").await;
    // The fresh agentd is booting on an empty /work; stop it before replacing the tree.
    let _ = dt
        .toolbox_exec(
            &new_id,
            "sudo -n pkill -TERM -x arbos-agentd 2>/dev/null; sleep 2; sudo -n pkill -KILL -x arbos-agentd 2>/dev/null; true",
            60,
        )
        .await;
    dt.toolbox_upload(&new_id, "/home/daytona/arbos-upgrade.tgz", bytes)
        .await
        .context("upload /work tarball")?;
    let out = dt
        .toolbox_exec(
            &new_id,
            "sudo -n rm -rf /work/* /work/.[!.]* 2>/dev/null; sudo -n tar xzf /home/daytona/arbos-upgrade.tgz -C /work && sudo -n find /work -maxdepth 3 -name .owner -path '*/.arbos/.owner' -delete; rm -f /home/daytona/arbos-upgrade.tgz && echo ok",
            900,
        )
        .await?;
    if out.get("exitCode").and_then(Value::as_i64).unwrap_or(1) != 0 {
        return Err(anyhow!(
            "extract on the new box failed: {}",
            out.get("result").and_then(Value::as_str).unwrap_or("")
        ));
    }

    // ---- start: bounce so the entrypoint runs agentd on the carried /work --------------------
    set_phase(state, user_id, "start").await;
    dt.stop(&new_id, true).await?;
    dt.wait_for(&new_id, Duration::from_secs(120), |s| {
        s.state == SandboxState::Stopped
    })
    .await?;
    state.set_box_state(user_id, BoxState::Asleep, None).await;
    boxes::start_box(state, user_id).await?;
    Ok(json!({
        "old_sandbox": old_id, "new_sandbox": new_id,
        "from": old.snapshot, "to": target, "work_mib": mib,
        "old_kept_until": now_ms() + KEEP_OLD_SANDBOX_MS,
    }))
}

/// Hourly: delete old sandboxes whose fallback window has passed.
pub async fn spawn_history_sweeper(state: AppState) {
    let Some(_) = state.daytona.as_ref() else {
        return;
    };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let Some(dt) = state.daytona.as_ref() else {
                return;
            };
            for (user_id, sandbox_id) in state.db.box_history_due().unwrap_or_default() {
                match dt.delete(&sandbox_id).await {
                    Ok(_) => {
                        tracing::info!(user = %user_id, sandbox = %sandbox_id, "old box deleted after the fallback window");
                        let _ = state.db.mark_box_history_deleted(&sandbox_id);
                    }
                    Err(e) if e.to_string().contains("404") => {
                        let _ = state.db.mark_box_history_deleted(&sandbox_id);
                    }
                    Err(e) => tracing::warn!(sandbox = %sandbox_id, error = %e, "old box delete"),
                }
            }
        }
    });
}

// ---- handlers -----------------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct UpgradeQuery {
    /// Recreate even when the snapshot already matches.
    #[serde(default)]
    pub force: bool,
}

async fn kick(state: &AppState, user_id: &str, force: bool) -> Response {
    match start(state, user_id, force).await {
        Ok(true) => (
            StatusCode::ACCEPTED,
            Json(json!({ "user_id": user_id, "upgrade": status_json(state, user_id).await })),
        )
            .into_response(),
        Ok(false) => {
            let st = status_json(state, user_id).await;
            if st["in_progress"].as_bool() == Some(true) {
                (
                    StatusCode::ACCEPTED,
                    Json(json!({ "user_id": user_id, "upgrade": st })),
                )
                    .into_response()
            } else {
                Json(json!({ "user_id": user_id, "upgrade": st, "note": "already on the current snapshot (use ?force=true to recreate anyway)" }))
                    .into_response()
            }
        }
        Err(e) => api_error(StatusCode::BAD_GATEWAY, "upgrade", e.to_string()),
    }
}

/// `POST /machine/upgrade[?force=true]` — the caller's own box.
pub async fn machine_upgrade(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Query(q): axum::extract::Query<UpgradeQuery>,
) -> Response {
    let Principal::Device { user_id, .. } = &principal else {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a signed-in device is required",
        );
    };
    if let Err(r) = crate::credits::check(&state, user_id) {
        return crate::credits::refused_response(&r);
    }
    kick(&state, user_id, q.force).await
}

/// `POST /admin/users/{id}/box/upgrade[?force=true]`.
pub async fn admin_upgrade(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<UpgradeQuery>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    kick(&state, &id, q.force).await
}
