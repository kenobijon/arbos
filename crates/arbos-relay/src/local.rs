//! User-computer access (F9). `arbos-local` helpers hold one WebSocket each
//! (`WS /local/{deviceId}`, device token); a project box asks the relay to forward one op
//! (`POST /projects/{id}/local/{device}/exec`, box token) and the relay waits for the result
//! on that socket. The relay never runs anything itself and never sees a device's files beyond
//! what passes through the reply.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, oneshot, Mutex};

use arbos_core::api::{LocalDeviceDto, LocalDevicesDto};
use arbos_core::ids::ulid;
use arbos_core::local::{
    LocalClientFrame, LocalExecRequest, LocalOp, LocalResult, LocalServerFrame,
};

use crate::auth::Principal;
use crate::state::AppState;

const PING_EVERY: Duration = Duration::from_secs(20);
/// Relay-side slack on top of the op's own timeout (network + helper scheduling).
const EXEC_GRACE: Duration = Duration::from_secs(5);
const RESULT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// One live helper socket.
pub struct LocalConn {
    pub tx: mpsc::Sender<String>,
    pending: Mutex<HashMap<String, oneshot::Sender<LocalResult>>>,
}

pub type LocalConns = Arc<Mutex<HashMap<String, Arc<LocalConn>>>>;

type ApiResult = Result<Response, (StatusCode, String)>;

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// `WS /local/{deviceId}` — the helper's own device token only.
pub async fn ws_local(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    principal: Principal,
    Path(device_id): Path<String>,
) -> Response {
    match &principal {
        Principal::Device { id, .. } if *id == device_id => {}
        Principal::Device { .. } => {
            return (StatusCode::FORBIDDEN, "token belongs to another device").into_response()
        }
        Principal::Box { .. } | Principal::Admin => {
            return (StatusCode::FORBIDDEN, "device token required").into_response()
        }
    }
    ws.max_message_size(RESULT_MAX_BYTES)
        .on_upgrade(move |socket| run(socket, state, device_id))
}

async fn run(socket: WebSocket, state: AppState, device_id: String) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<String>(64);
    let conn = Arc::new(LocalConn {
        tx,
        pending: Mutex::new(HashMap::new()),
    });
    // A reconnect replaces the previous socket for this device; its pending ops fail below.
    let previous = state
        .local_conns
        .lock()
        .await
        .insert(device_id.clone(), conn.clone());
    if let Some(prev) = previous {
        fail_pending(&prev, "helper reconnected").await;
    }
    let mut said_hello = false;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(Ok(msg)) = incoming else { break };
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    Message::Binary(_) | Message::Ping(_) | Message::Pong(_) => continue,
                };
                let frame: LocalClientFrame = match serde_json::from_str(&text) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(%device_id, error = %e, "local: bad frame");
                        continue;
                    }
                };
                match frame {
                    LocalClientFrame::Hello(h) => {
                        said_hello = true;
                        let _ = state.db.local_device_hello(&device_id, &h.hostname, &h.os, &h.home);
                        tracing::info!(%device_id, host = %h.hostname, os = %h.os, "local helper connected");
                    }
                    LocalClientFrame::Result { id, result } => {
                        let _ = state.db.local_device_seen(&device_id, true);
                        if let Some(waiter) = conn.pending.lock().await.remove(&id) {
                            let _ = waiter.send(result);
                        }
                    }
                    LocalClientFrame::Pong => {
                        let _ = state.db.local_device_seen(&device_id, true);
                    }
                }
            }
            out = rx.recv() => {
                let Some(text) = out else { break };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            _ = ping.tick() => {
                let Ok(text) = serde_json::to_string(&LocalServerFrame::Ping) else { continue };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        }
    }
    let mut conns = state.local_conns.lock().await;
    if conns.get(&device_id).is_some_and(|c| Arc::ptr_eq(c, &conn)) {
        conns.remove(&device_id);
        if said_hello {
            let _ = state.db.local_device_seen(&device_id, false);
        }
    }
    drop(conns);
    fail_pending(&conn, "helper disconnected").await;
    tracing::info!(%device_id, "local helper disconnected");
}

async fn fail_pending(conn: &LocalConn, why: &str) {
    for (_, waiter) in conn.pending.lock().await.drain() {
        let _ = waiter.send(LocalResult::Error {
            message: why.to_owned(),
        });
    }
}

/// `POST /projects/{id}/local/{device}/exec` (box token): body = one `LocalOp`.
pub async fn exec(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, device_id)): Path<(String, String)>,
    Json(op): Json<LocalOp>,
) -> ApiResult {
    if !state.allows(&principal, &project_id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    if !matches!(principal, Principal::Box { .. } | Principal::Admin) {
        return Err((StatusCode::FORBIDDEN, "box token required".into()));
    }
    if state.db.project(&project_id).map_err(internal)?.is_none() {
        return Err((StatusCode::NOT_FOUND, "no such project".into()));
    }
    let known = state.db.local_device(&device_id).map_err(internal)?;
    if known.is_none() {
        return Err((StatusCode::NOT_FOUND, "no such local device".into()));
    }
    let conn = state.local_conns.lock().await.get(&device_id).cloned();
    let Some(conn) = conn else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "kind": "error", "message": "device is offline" })),
        )
            .into_response());
    };
    let id = ulid();
    let (tx, rx) = oneshot::channel();
    conn.pending.lock().await.insert(id.clone(), tx);
    let wait = Duration::from_secs(op.timeout_s()) + EXEC_GRACE;
    let frame = LocalServerFrame::Exec(LocalExecRequest {
        id: id.clone(),
        project_id: project_id.clone(),
        op,
    });
    let text = serde_json::to_string(&frame).map_err(internal)?;
    if conn.tx.send(text).await.is_err() {
        conn.pending.lock().await.remove(&id);
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "kind": "error", "message": "device socket closed" })),
        )
            .into_response());
    }
    let result = match tokio::time::timeout(wait, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => LocalResult::Error {
            message: "helper disconnected".into(),
        },
        Err(_) => {
            conn.pending.lock().await.remove(&id);
            return Ok((
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({ "kind": "error", "message": format!("no result within {} s", wait.as_secs()) })),
            )
                .into_response());
        }
    };
    Ok(Json(serde_json::to_value(result).map_err(internal)?).into_response())
}

/// `GET /local/devices` — device tokens (Settings → This Mac) and box tokens (`local_devices`
/// tool). Each caller sees their own helpers only: a local device is a paired device, so its
/// owner is `devices.user_id`, and a box asks on behalf of the user it serves. The admin token has
/// no user and keeps the whole list (#1952).
pub async fn list(State(state): State<AppState>, principal: Principal) -> ApiResult {
    let devices = match principal.user_id() {
        Some(caller) => state.db.local_devices_of_user(caller).map_err(internal)?,
        None => state.db.local_devices().map_err(internal)?,
    };
    let online = state.local_conns.lock().await;
    let items: Vec<LocalDeviceDto> = devices
        .into_iter()
        .map(|d| {
            let connected = online.contains_key(&d.device_id);
            LocalDeviceDto {
                device_id: d.device_id,
                name: d.name,
                hostname: d.hostname,
                os: d.os,
                home: d.home,
                connected,
                last_seen: d.last_seen,
                first_seen: d.first_seen,
                allow_reads: d.allow_reads,
            }
        })
        .collect();
    Ok(Json(LocalDevicesDto { items }).into_response())
}

#[derive(Deserialize)]
pub struct PatchBody {
    #[serde(default)]
    pub allow_reads: Option<bool>,
}

/// Does this principal own the helper on `device_id`? A local device is a paired device, so its
/// owner is `devices.user_id`. Another user's device — and an unknown one — answer 404 rather
/// than 403, as `api::revoke_device` does, so neither route is an enumeration oracle (#1952).
/// The admin token has no user and stays unscoped.
fn owns_device(
    state: &AppState,
    principal: &Principal,
    device_id: &str,
) -> Result<bool, (StatusCode, String)> {
    let Some(caller) = principal.user_id() else {
        return Ok(true);
    };
    let owner = state.db.device_user(device_id).map_err(internal)?;
    Ok(owner.as_deref() == Some(caller))
}

fn no_such_device() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "no such local device".to_owned())
}

/// `PATCH /local/devices/{id}` (device token): the per-device "allow reads without asking" toggle.
pub async fn patch(
    State(state): State<AppState>,
    principal: Principal,
    Path(device_id): Path<String>,
    Json(b): Json<PatchBody>,
) -> ApiResult {
    if principal.is_box() {
        return Err((
            StatusCode::FORBIDDEN,
            "box tokens cannot change device settings".into(),
        ));
    }
    if !owns_device(&state, &principal, &device_id)? {
        return Err(no_such_device());
    }
    if let Some(allow) = b.allow_reads {
        let ok = state
            .db
            .local_device_set_allow_reads(&device_id, allow)
            .map_err(internal)?;
        if !ok {
            return Err((StatusCode::NOT_FOUND, "no such local device".into()));
        }
    }
    let d = state
        .db
        .local_device(&device_id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such local device".to_owned()))?;
    Ok(Json(serde_json::to_value(d).map_err(internal)?).into_response())
}

/// `DELETE /local/devices/{id}` (device token): forget the helper and revoke its token.
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(device_id): Path<String>,
) -> ApiResult {
    if principal.is_box() {
        return Err((
            StatusCode::FORBIDDEN,
            "box tokens cannot remove devices".into(),
        ));
    }
    // The revocation below reaches any paired device id, helper row or not, so the ownership
    // check has to come first — #1950's check belongs on this route as well.
    if !owns_device(&state, &principal, &device_id)? {
        return Err(no_such_device());
    }
    let ok = state.db.local_device_delete(&device_id).map_err(internal)?;
    let _ = state.db.revoke_device(&device_id);
    if let Some(conn) = state.local_conns.lock().await.remove(&device_id) {
        fail_pending(&conn, "device removed").await;
    }
    Ok(if ok {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
    .into_response())
}

#[cfg(test)]
mod tests {
    use arbos_core::ids::ulid;

    use super::*;
    use crate::people::tests_support::person;
    use crate::state::test_support::test_state;
    use crate::state::AuthMode;

    fn status(r: ApiResult) -> StatusCode {
        match r {
            Ok(resp) => resp.status(),
            Err((s, _)) => s,
        }
    }

    /// One paired device of `user`, optionally running the arbos-local helper.
    fn device_of(state: &AppState, user: &str, helper: bool) -> String {
        let id = ulid();
        state
            .db
            .insert_device(&id, "mac", &ulid(), user, "github")
            .expect("device");
        if helper {
            state
                .db
                .local_device_hello(&id, "a-mac", "macos", "/Users/a")
                .expect("hello");
        }
        id
    }

    fn revoked(state: &AppState, device: &str) -> bool {
        state
            .db
            .device_status(device)
            .unwrap()
            .map(|(_, revoked)| revoked)
            .expect("device row")
    }

    /// #1952: `list` ignored its principal entirely and returned every user's helpers — each
    /// one's hostname, operating system and home directory path — to any device token. Each
    /// caller now sees their own; a box sees the helpers of the user it serves, which is what
    /// the `local_devices` tool is for; the admin token keeps the whole list.
    #[tokio::test]
    async fn list_shows_each_caller_only_their_own_helpers() {
        let state = test_state(AuthMode::Github);
        let (alice, alice_dev) = person(&state, 1);
        let (bob, bob_dev) = person(&state, 2);
        let hers = device_of(&state, &alice, true);
        let his = device_of(&state, &bob, true);

        let ids = |r: ApiResult| -> Vec<String> {
            let body = r.expect("ok").into_body();
            let bytes =
                futures::executor::block_on(axum::body::to_bytes(body, usize::MAX)).expect("body");
            serde_json::from_slice::<LocalDevicesDto>(&bytes)
                .expect("dto")
                .items
                .into_iter()
                .map(|d| d.device_id)
                .collect()
        };

        assert_eq!(
            ids(list(State(state.clone()), alice_dev).await),
            vec![hers.clone()],
            "Alice sees her own helper and not Bob's"
        );
        assert_eq!(
            ids(list(State(state.clone()), bob_dev).await),
            vec![his.clone()]
        );

        // Alice's box asks on her behalf and gets the same answer.
        assert_eq!(
            ids(list(
                State(state.clone()),
                Principal::Box {
                    user_id: alice.clone()
                }
            )
            .await),
            vec![hers.clone()]
        );

        let all = ids(list(State(state.clone()), Principal::Admin).await);
        assert_eq!(all.len(), 2, "the admin token still sees both: {all:?}");
    }

    /// #1952: `patch` refused boxes and nothing else, so any signed-in device could turn
    /// `allow_reads` on for another user's helper — consent to read that person's files
    /// without asking, given to a stranger.
    #[tokio::test]
    async fn patch_refuses_another_users_local_device() {
        let state = test_state(AuthMode::Github);
        let (alice, alice_dev) = person(&state, 1);
        let (_bob, bob_dev) = person(&state, 2);
        let helper = device_of(&state, &alice, true);
        let body = || {
            Json(PatchBody {
                allow_reads: Some(true),
            })
        };

        let foreign = patch(State(state.clone()), bob_dev, Path(helper.clone()), body()).await;
        assert_eq!(status(foreign), StatusCode::NOT_FOUND);
        assert!(
            !state.db.local_device(&helper).unwrap().unwrap().allow_reads,
            "the toggle stays off for a stranger"
        );

        let own = patch(
            State(state.clone()),
            alice_dev,
            Path(helper.clone()),
            body(),
        )
        .await;
        assert_eq!(status(own), StatusCode::OK);
        assert!(state.db.local_device(&helper).unwrap().unwrap().allow_reads);
    }

    /// #1952: `delete` refused boxes and nothing else. It also called `revoke_device`
    /// unconditionally on a plain paired-device id, so it reached #1950's "revoke anyone's
    /// sessions" through a second route — even for an id with no helper row at all.
    #[tokio::test]
    async fn delete_refuses_another_users_local_device_and_keeps_its_sessions() {
        let state = test_state(AuthMode::Github);
        let (alice, alice_dev) = person(&state, 1);
        let (_bob, bob_dev) = person(&state, 2);
        let helper = device_of(&state, &alice, true);
        let plain = device_of(&state, &alice, false);

        let foreign = delete(State(state.clone()), bob_dev.clone(), Path(helper.clone())).await;
        assert_eq!(status(foreign), StatusCode::NOT_FOUND);
        assert!(state.db.local_device(&helper).unwrap().is_some());
        assert!(
            !revoked(&state, &helper),
            "Alice keeps her helper's sessions"
        );

        // A device with no helper row is not a way to revoke that device either (#1950).
        let no_row = delete(State(state.clone()), bob_dev, Path(plain.clone())).await;
        assert_eq!(status(no_row), StatusCode::NOT_FOUND);
        assert!(!revoked(&state, &plain), "Alice keeps her other sessions");

        let own = delete(State(state.clone()), alice_dev, Path(helper.clone())).await;
        assert_eq!(status(own), StatusCode::NO_CONTENT);
        assert!(state.db.local_device(&helper).unwrap().is_none());
        assert!(revoked(&state, &helper), "her own removal still revokes it");
    }
}
