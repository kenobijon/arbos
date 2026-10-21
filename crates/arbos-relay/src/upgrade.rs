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
    (sb.provider == "daytona" || sb.provider == "gvisor")
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
    let provider = Provider::parse(&sb.provider);
    if provider == Some(Provider::Gvisor) {
        return start_gvisor(state, user_id, &sb, force).await;
    }
    if provider != Some(Provider::Daytona) {
        return Err(anyhow!(
            "only Daytona and gvisor boxes can be upgraded (this one is {})",
            sb.provider
        ));
    }
    if sb.sandbox_id.is_none() {
        return Err(anyhow!(
            "the box row has no sandbox; use POST /admin/users/{{id}}/box/repair to re-adopt the retained one"
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
        let outcome = run(&state2, &user, &target, Source::CurrentBox).await;
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

/// gVisor sandbox (D89): the image roll is a container recreate on the same `/work` image —
/// seconds, nothing copied. Same status surface (`upgrading`, audit rows) as the Daytona path.
async fn start_gvisor(
    state: &AppState,
    user_id: &str,
    sb: &crate::db::Sandbox,
    force: bool,
) -> Result<bool> {
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
                phase: "recreating".into(),
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
        state2
            .set_box_state(&user, BoxState::Waking, Some("upgrading: recreating"))
            .await;
        let outcome = crate::fleet::recreate_with_snapshot(&state2, &user, &target).await;
        state2.upgrading.lock().await.remove(&user);
        match outcome {
            Ok(()) => {
                tracing::info!(user = %user, target, ms = t0.elapsed().as_millis() as i64, "gvisor box upgraded");
                let _ = state2.db.record_audit(
                    "relay-admin",
                    "box_upgrade",
                    &json!({ "ts": now_ms(), "user_id": user, "report": { "provider": "gvisor", "to": target }, "ms": t0.elapsed().as_millis() as i64 }),
                );
            }
            Err(e) => {
                tracing::error!(user = %user, error = %format!("{e:#}"), "gvisor box upgrade failed");
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

/// Where the `/work` to carry comes from.
#[derive(Debug, Clone)]
pub enum Source {
    /// The user's current box (normal upgrade): it is stopped and kept as the fallback.
    CurrentBox,
    /// A retained sandbox from `box_history` (repair: the row lost its sandbox).
    History { sandbox_id: String },
}

/// Pack `/work` of a sandbox into one tarball pulled through the relay. Starts it when stopped
/// and quiesces agentd first. Returns `(bytes, mib)`.
async fn pack(dt: &crate::daytona::Daytona, sandbox_id: &str) -> Result<(Vec<u8>, i64)> {
    let cur = dt.get(sandbox_id).await.context("source sandbox")?;
    if matches!(
        cur.state,
        SandboxState::Destroyed | SandboxState::Destroying
    ) {
        return Err(anyhow!(
            "sandbox {sandbox_id} is destroyed; nothing to carry over"
        ));
    }
    if cur.state != SandboxState::Started {
        dt.start(sandbox_id).await?;
        dt.wait_for(sandbox_id, Duration::from_secs(180), |s| {
            s.state == SandboxState::Started
        })
        .await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let _ = dt
        .toolbox_exec(
            sandbox_id,
            "sudo -n pkill -TERM -x arbos-agentd 2>/dev/null; sleep 3; sudo -n pkill -KILL -x arbos-agentd 2>/dev/null; sudo -n pkill -KILL -f 'pi --mode rpc' 2>/dev/null; sudo -n pkill -KILL -x prime-agent 2>/dev/null; true",
            60,
        )
        .await;
    let du = dt
        .toolbox_exec(sandbox_id, "sudo -n du -sm /work | cut -f1", 120)
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
            sandbox_id,
            "sudo -n rm -f /home/daytona/arbos-upgrade.tgz; cd /work && sudo -n tar czf /home/daytona/arbos-upgrade.tgz --exclude='./.arbos/logs' --exclude='./*/.arbos/logs' . && sudo -n chmod 644 /home/daytona/arbos-upgrade.tgz && ls -l /home/daytona/arbos-upgrade.tgz",
            900,
        )
        .await?;
    if tar.get("exitCode").and_then(Value::as_i64).unwrap_or(1) != 0 {
        return Err(anyhow!(
            "tar on the source box failed: {}",
            tar.get("result").and_then(Value::as_str).unwrap_or("")
        ));
    }
    let bytes = dt
        .toolbox_download(sandbox_id, "/home/daytona/arbos-upgrade.tgz")
        .await
        .context("download /work tarball")?;
    let _ = dt
        .toolbox_exec(
            sandbox_id,
            "sudo -n rm -f /home/daytona/arbos-upgrade.tgz",
            60,
        )
        .await;
    Ok((bytes, mib))
}

/// Put the carried `/work` onto a freshly created box and bounce it so agentd boots on it.
async fn unpack_and_start(
    state: &AppState,
    dt: &crate::daytona::Daytona,
    user_id: &str,
    new_id: &str,
    bytes: Vec<u8>,
) -> Result<()> {
    let _ = dt
        .toolbox_exec(
            new_id,
            "sudo -n pkill -TERM -x arbos-agentd 2>/dev/null; sleep 2; sudo -n pkill -KILL -x arbos-agentd 2>/dev/null; true",
            60,
        )
        .await;
    dt.toolbox_upload(new_id, "/home/daytona/arbos-upgrade.tgz", bytes)
        .await
        .context("upload /work tarball")?;
    let out = dt
        .toolbox_exec(
            new_id,
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
    set_phase(state, user_id, "start").await;
    dt.stop(new_id, true).await?;
    dt.wait_for(new_id, Duration::from_secs(120), |s| {
        s.state == SandboxState::Stopped
    })
    .await?;
    state.set_box_state(user_id, BoxState::Asleep, None).await;
    boxes::start_box(state, user_id).await?;
    // The new box must say hello; otherwise the upgrade did not work.
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        let (st, _) = state.box_state(user_id).await;
        if st == BoxState::Awake {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(anyhow!("the new box did not connect within 150 s"));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Undo a failed upgrade: delete the half-made new sandbox (if any), put the old row back, start
/// the old sandbox again, forget the history entry. The user ends where they began.
async fn rollback(
    state: &AppState,
    dt: &crate::daytona::Daytona,
    user_id: &str,
    old: &crate::db::Sandbox,
    new_sandbox: Option<&str>,
) {
    tracing::warn!(user = %user_id, new_sandbox, "upgrade rolled back");
    if let Some(id) = new_sandbox {
        let _ = dt.stop(id, true).await;
        let _ = dt.delete(id).await;
    }
    let mut restored = old.clone();
    restored.daytona_state = Some("stopped".into());
    let _ = state.db.upsert_box(&restored);
    let _ = state.db.set_box_held(user_id, false);
    if let Some(old_id) = &old.sandbox_id {
        let _ = state.db.remove_box_history(old_id);
    }
    state
        .set_box_state(user_id, BoxState::Asleep, Some("upgrade rolled back"))
        .await;
    if let Err(e) = boxes::start_box(state, user_id).await {
        tracing::error!(user = %user_id, error = %e, "restart of the old box after rollback failed");
    }
}

async fn run(state: &AppState, user_id: &str, target: &str, source: Source) -> Result<Value> {
    let dt = state
        .daytona
        .as_ref()
        .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
    let (source_id, old_row) = match &source {
        Source::CurrentBox => {
            let old = state
                .db
                .user_box(user_id)?
                .ok_or_else(|| anyhow!("box vanished"))?;
            let id = old
                .sandbox_id
                .clone()
                .ok_or_else(|| anyhow!("the box row has no sandbox id; use the repair path"))?;
            (id, Some(old))
        }
        Source::History { sandbox_id } => (sandbox_id.clone(), None),
    };

    set_phase(state, user_id, "pack").await;
    let (bytes, mib) = pack(dt, &source_id).await?;
    tracing::info!(user = %user_id, mib, bytes = bytes.len(), "source /work packed");

    if let Some(old) = &old_row {
        set_phase(state, user_id, "stop").await;
        let _ = dt.stop(&source_id, false).await;
        state
            .db
            .insert_box_history(old, now_ms() + KEEP_OLD_SANDBOX_MS)?;
    }

    // From here on every failure rolls the user back to the old box (normal upgrade) or leaves
    // the retained sandbox untouched (repair).
    set_phase(state, user_id, "create").await;
    let created = boxes::create_box(state, user_id, Provider::Daytona, Some(target)).await;
    let mut new = match created {
        Ok(n) => n,
        Err(e) => {
            if let Some(old) = &old_row {
                rollback(state, dt, user_id, old, None).await;
            }
            return Err(e.context("create the new box"));
        }
    };
    if let Some(old) = &old_row {
        new.migrated_from = old.migrated_from.clone();
    }
    state.db.upsert_box(&new)?;
    let new_id = match new.sandbox_id.clone() {
        Some(id) => id,
        None => {
            if let Some(old) = &old_row {
                rollback(state, dt, user_id, old, None).await;
            }
            return Err(anyhow!("new box has no sandbox id"));
        }
    };

    set_phase(state, user_id, "unpack").await;
    if let Err(e) = unpack_and_start(state, dt, user_id, &new_id, bytes).await {
        if let Some(old) = &old_row {
            rollback(state, dt, user_id, old, Some(&new_id)).await;
        } else {
            let _ = dt.stop(&new_id, true).await;
            let _ = dt.delete(&new_id).await;
            let mut empty = new.clone();
            empty.sandbox_id = None;
            let _ = state.db.upsert_box(&empty);
        }
        return Err(e.context("carry /work onto the new box"));
    }
    Ok(json!({
        "source_sandbox": source_id, "new_sandbox": new_id,
        "from": old_row.as_ref().and_then(|o| o.snapshot.clone()), "to": target, "work_mib": mib,
        "old_kept_until": old_row.as_ref().map(|_| now_ms() + KEEP_OLD_SANDBOX_MS),
        "repair": matches!(source, Source::History { .. }),
    }))
}

/// Repair: the user's row has no usable sandbox (a failed upgrade, a deleted sandbox) but a
/// retained one exists in `box_history`. Carry its `/work` onto a fresh box from the default
/// snapshot. `Ok(false)` when nothing is wrong or a run is in progress.
pub async fn start_repair(state: &AppState, user_id: &str) -> Result<bool> {
    let row = state.db.user_box(user_id)?;
    let dt = state
        .daytona
        .as_ref()
        .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
    let usable = match row.as_ref().and_then(|r| r.sandbox_id.clone()) {
        Some(id) => !matches!(
            dt.get(&id).await.map(|s| s.state),
            Err(_) | Ok(SandboxState::Destroyed) | Ok(SandboxState::Destroying)
        ),
        None => false,
    };
    if usable {
        return Ok(false);
    }
    let retained = state
        .db
        .latest_box_history(user_id)?
        .ok_or_else(|| anyhow!("no retained sandbox for {user_id}"))?;
    // Cheapest repair: the history entry kept the full row (token hash, device id, urls) and the
    // sandbox still exists — put the row back and start it; no copy needed.
    if let Some(mut old_row) = retained.row.clone() {
        if dt.get(&retained.sandbox_id).await.is_ok() {
            old_row.daytona_state = Some("stopped".into());
            state.db.upsert_box(&old_row)?;
            state.db.set_box_held(user_id, false)?;
            state.db.remove_box_history(&retained.sandbox_id)?;
            state
                .set_box_state(
                    user_id,
                    BoxState::Asleep,
                    Some("re-adopted the retained box"),
                )
                .await;
            boxes::ensure_awake(state, user_id, "start").await;
            let _ = state.db.record_audit(
                "relay-admin",
                "box_readopt",
                &json!({ "ts": now_ms(), "user_id": user_id, "sandbox": retained.sandbox_id }),
            );
            tracing::info!(user = %user_id, sandbox = %retained.sandbox_id, "retained box re-adopted");
            return Ok(true);
        }
    }
    let (sandbox_id, snapshot) = (retained.sandbox_id, retained.snapshot);
    let target = state
        .db
        .user_snapshot(user_id)?
        .unwrap_or_else(|| state.cfg.daytona_snapshot.clone());
    {
        let mut running = state.upgrading.lock().await;
        if running.contains_key(user_id) {
            return Ok(false);
        }
        running.insert(
            user_id.to_owned(),
            UpgradeStatus {
                phase: "repair".into(),
                since: now_ms(),
                from_snapshot: snapshot,
                to_snapshot: target.clone(),
            },
        );
    }
    let state2 = state.clone();
    let user = user_id.to_owned();
    tokio::spawn(async move {
        let outcome = run(&state2, &user, &target, Source::History { sandbox_id }).await;
        state2.upgrading.lock().await.remove(&user);
        match outcome {
            Ok(report) => {
                tracing::info!(user = %user, ?report, "box repaired");
                let _ = state2.db.record_audit(
                    "relay-admin",
                    "box_repair",
                    &json!({ "ts": now_ms(), "user_id": user, "report": report }),
                );
            }
            Err(e) => {
                tracing::error!(user = %user, error = %format!("{e:#}"), "box repair failed");
                let _ = state2.db.record_audit(
                    "relay-admin",
                    "box_repair_failed",
                    &json!({ "ts": now_ms(), "user_id": user, "error": format!("{e:#}") }),
                );
                state2
                    .set_box_state(&user, BoxState::Error, Some(&format!("repair failed: {e}")))
                    .await;
            }
        }
    });
    Ok(true)
}

/// `POST /admin/users/{id}/box/repair`.
pub async fn admin_repair(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    match start_repair(&state, &id).await {
        Ok(true) => (
            StatusCode::ACCEPTED,
            Json(json!({ "user_id": id, "upgrade": status_json(&state, &id).await })),
        )
            .into_response(),
        Ok(false) => Json(json!({ "user_id": id, "upgrade": status_json(&state, &id).await, "note": "nothing to repair (the box row has a live sandbox) or a run is in progress" })).into_response(),
        Err(e) => api_error(StatusCode::BAD_GATEWAY, "repair", e.to_string()),
    }
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
