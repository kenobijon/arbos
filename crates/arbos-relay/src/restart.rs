//! Planned restarts (#1347): tell every room socket before the relay goes down on purpose, so the
//! apps redial at once instead of backing off ("Reconnecting…" for minutes on Oct 22).
//!
//! Sequence, per open room socket:
//! 1. notice — an `Error` frame `{code: "restart_soon", message: "<seconds>"}` (the apps show
//!    "Reconnecting shortly…" and keep the socket open);
//! 2. close — a 1012 Service Restart close frame (the apps reset their backoff and redial 1.5 s
//!    later with a fresh session);
//! 3. drain — wait, bounded, for the closes to go out; then the process exits / systemd restarts.
//!
//! `POST /admin/restart` runs 1–3 for the deploy script, which then does `systemctl restart`.
//! `SIGTERM` runs 2–3 on its own, so a plain `systemctl restart` (an image-roll fallback, a hand
//! restart) is graceful too, just without the notice. Nothing here changes the wire format:
//! `restart_soon` is a new value of an existing header field and 1012 is a standard close code.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;

use arbos_core::wire::{Frame, FrameType};

use crate::auth::Principal;
use crate::signin::api_error;
use crate::state::{AppState, RoomBroadcast};

pub const DEFAULT_NOTICE: Duration = Duration::from_secs(10);
pub const DEFAULT_DRAIN: Duration = Duration::from_secs(5);
const MAX_NOTICE: Duration = Duration::from_secs(60);
const MAX_DRAIN: Duration = Duration::from_secs(30);
/// If the process is still alive this long after `/admin/restart` closed its sockets (the
/// restart never came), stop bouncing new sockets with 1012 and serve normally again.
const CLOSING_RESET: Duration = Duration::from_secs(90);

/// Step 1: tell every room socket a restart comes in `seconds`. Returns how many rooms were told.
pub async fn announce(state: &AppState, seconds: u64) -> usize {
    let bytes: Bytes = Frame::new(
        FrameType::Error,
        json!({ "code": "restart_soon", "message": seconds.to_string() }),
        Bytes::new(),
    )
    .encode();
    let rooms = state.rooms.read().await;
    let mut n = 0;
    for room in rooms.values() {
        // from_conn 0 is never a live socket (ids start at 1), so every member gets it.
        if room
            .tx
            .send(RoomBroadcast {
                from_conn: 0,
                bytes: bytes.clone(),
                seq: 0,
            })
            .is_ok()
        {
            n += 1;
        }
    }
    n
}

/// Steps 2 + 3: flip the signal (every room socket sends 1012 and ends) and wait up to `drain`
/// for them to be gone. Returns (sockets open before, sockets still open after).
pub async fn close_and_drain(state: &AppState, drain: Duration) -> (usize, usize) {
    let before = state.restart.open_sockets.load(Ordering::Relaxed);
    state.restart.closing.send_replace(true);
    let deadline = Instant::now() + drain;
    loop {
        let left = state.restart.open_sockets.load(Ordering::Relaxed);
        if left == 0 || Instant::now() >= deadline {
            return (before, left);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[derive(Deserialize, Default)]
pub struct RestartBody {
    /// Seconds between the `restart_soon` notice and the close (default 10, max 60; 0 = no notice).
    pub notice_s: Option<u64>,
    /// Seconds to wait for the sockets to close (default 5, max 30).
    pub drain_s: Option<u64>,
}

/// `POST /admin/restart {"notice_s": 10, "drain_s": 5}` — notice, 1012 close, drain; answers when
/// the relay is ready to be restarted. The caller (deploy-relay.sh, the deploy worker) then runs
/// `systemctl restart arbos-relay`. Admin token only. Takes up to notice_s + drain_s to answer.
pub async fn admin_restart(
    State(state): State<AppState>,
    principal: Principal,
    body: Option<Json<RestartBody>>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    let Json(b) = body.unwrap_or_default();
    let notice =
        Duration::from_secs(b.notice_s.unwrap_or(DEFAULT_NOTICE.as_secs())).min(MAX_NOTICE);
    let drain = Duration::from_secs(b.drain_s.unwrap_or(DEFAULT_DRAIN.as_secs())).min(MAX_DRAIN);
    if state.restart.is_closing() {
        return api_error(
            StatusCode::CONFLICT,
            "already_closing",
            "a restart is already in progress",
        );
    }
    let t0 = Instant::now();
    let mut rooms = 0;
    if !notice.is_zero() {
        rooms = announce(&state, notice.as_secs()).await;
        tracing::info!(
            rooms,
            seconds = notice.as_secs(),
            "planned restart: notice sent"
        );
        tokio::time::sleep(notice).await;
    }
    let (before, left) = close_and_drain(&state, drain).await;
    tracing::info!(
        sockets = before,
        still_open = left,
        took_ms = t0.elapsed().as_millis() as u64,
        "planned restart: room sockets closed with 1012; ready for systemctl restart"
    );
    let st = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(CLOSING_RESET).await;
        if st.restart.is_closing() {
            st.restart.closing.send_replace(false);
            tracing::warn!(
                "planned restart: no restart came within {CLOSING_RESET:?}; serving sockets again"
            );
        }
    });
    Json(json!({
        "rooms_notified": rooms,
        "sockets_closed": before,
        "still_open": left,
        "drained": left == 0,
        "took_ms": t0.elapsed().as_millis() as u64,
    }))
    .into_response()
}

/// On SIGTERM (what `systemctl restart`/`stop` sends): close every room socket with 1012, drain
/// briefly, then exit. Without this the default action kills the process mid-frame and every
/// client sees a dropped stream (1006) and backs off.
pub fn install_sigterm_handler(state: AppState) {
    tokio::spawn(async move {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "SIGTERM handler not installed");
                    return;
                }
            };
        term.recv().await;
        let (before, left) = close_and_drain(&state, DEFAULT_DRAIN).await;
        tracing::info!(
            sockets = before,
            still_open = left,
            "SIGTERM: room sockets closed with 1012; exiting"
        );
        std::process::exit(0);
    });
}
