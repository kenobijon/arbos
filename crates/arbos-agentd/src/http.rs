//! The box's HTTP surfaces: `:7788` (box-level: `/healthz`, `/browser/stream`; per workspace:
//! `/w/{project}/history`, `/w/{project}/metrics`, `/w/{project}/t3/search`) and each workspace's
//! loopback model stub that forwards to the relay proxy with the box token.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde_json::json;

use crate::agent::Agent;
use crate::workspace::Supervisor;

type S = Arc<Agent>;
type B = Arc<Supervisor>;

/// Box health: every workspace with its pi/extension state. 503 until at least one workspace runs
/// (or the relay listed none yet).
async fn healthz(State(b): State<B>) -> Response {
    let ws = b.list().await;
    let mut items = Vec::new();
    let mut all_ok = true;
    for w in &ws {
        let ok = w.agent.is_healthy();
        all_ok &= ok;
        items.push(json!({
            "project": w.project_id,
            "ok": ok,
            "user": w.cfg.run_as.as_ref().map(|r| r.user.clone()),
            "pi": w.agent.pi_ready.load(std::sync::atomic::Ordering::SeqCst),
            "extension": w.agent.ext_loaded.load(std::sync::atomic::Ordering::SeqCst),
            "mcp_port": w.cfg.mcp_port,
            "started_at": w.started_at,
        }));
    }
    let body = json!({
        "ok": all_ok, "user": b.cfg.user_id, "device": b.cfg.device_id, "version": env!("CARGO_PKG_VERSION"), "booted_at": b.booted_at,
        "isolation": { "enforced": b.isolation.enabled, "cross_read_blocked": *b.cross_read_blocked.read().await },
        "workspaces": items,
    });
    (
        if all_ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(body),
    )
        .into_response()
}

async fn metrics(State(b): State<B>, Path(project): Path<String>) -> Response {
    match b.get(&project).await {
        Some(w) => {
            Json(json!({ "turns": w.store.turn_metrics(200).unwrap_or_default() })).into_response()
        }
        None => (StatusCode::NOT_FOUND, "no such workspace").into_response(),
    }
}

async fn history(
    State(b): State<B>,
    Path((project, chat_id)): Path<(String, String)>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(w) = b.get(&project).await else {
        return (StatusCode::NOT_FOUND, "no such workspace").into_response();
    };
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    match w
        .store
        .t3_thread_before(&chat_id, q.get("before").map(String::as_str), limit)
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Pre-D70 clients ask `/history/{chat}` without a project: find the workspace that owns the chat.
async fn history_legacy(
    State(b): State<B>,
    Path(chat_id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(w) = b.find_chat(&chat_id).await else {
        return (StatusCode::NOT_FOUND, "no workspace knows this chat").into_response();
    };
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    match w
        .store
        .t3_thread_before(&chat_id, q.get("before").map(String::as_str), limit)
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn t3_search(
    State(b): State<B>,
    Path(project): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(w) = b.get(&project).await else {
        return (StatusCode::NOT_FOUND, "no such workspace").into_response();
    };
    let query = q.get("q").cloned().unwrap_or_default();
    match w.store.t3_search(&query, None, None, 20, 0) {
        Ok(hits) => Json(hits.into_iter().map(|(r, s, snip)| json!({ "id": r.id, "thread_id": r.thread_id, "kind": r.kind, "score": -s, "snippet": snip })).collect::<Vec<_>>()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// S16: proxy agent-browser's JPEG stream (`ws://127.0.0.1:9223`) on `/browser/stream`.
async fn browser_stream(ws: WebSocketUpgrade, State(b): State<B>) -> Response {
    let _ = b;
    ws.on_upgrade(|socket| async move {
        if let Err(e) = pipe_browser(socket).await {
            tracing::debug!(error = %e, "browser stream closed");
        }
    })
}

/// agent-browser binds its stream to `AGENT_BROWSER_STREAM_PORT` (9223 in the image); when that is
/// unset the port is OS-assigned and `agent-browser stream status` reports it.
async fn browser_stream_url() -> String {
    if let Ok(u) = std::env::var("ARBOS_BROWSER_STREAM_URL") {
        return u;
    }
    if let Ok(out) = tokio::process::Command::new("agent-browser")
        .args(["stream", "status"])
        .output()
        .await
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(idx) = text.find("ws://") {
            return text[idx..]
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
        }
    }
    format!(
        "ws://127.0.0.1:{}",
        std::env::var("AGENT_BROWSER_STREAM_PORT").unwrap_or_else(|_| "9223".into())
    )
}

async fn pipe_browser(client: WebSocket) -> anyhow::Result<()> {
    let upstream_url = browser_stream_url().await;
    let (upstream, _) = tokio_tungstenite::connect_async(&upstream_url).await?;
    let (mut up_tx, mut up_rx) = upstream.split();
    let (mut cl_tx, mut cl_rx) = client.split();
    let to_client = async {
        while let Some(Ok(m)) = up_rx.next().await {
            let out = match m {
                tokio_tungstenite::tungstenite::Message::Binary(b) => Message::Binary(b),
                tokio_tungstenite::tungstenite::Message::Text(t) => {
                    Message::Text(t.as_str().into())
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => break,
                _ => continue,
            };
            if cl_tx.send(out).await.is_err() {
                break;
            }
        }
    };
    let to_upstream = async {
        while let Some(Ok(m)) = cl_rx.next().await {
            let out = match m {
                Message::Binary(b) => tokio_tungstenite::tungstenite::Message::Binary(b),
                Message::Text(t) => {
                    tokio_tungstenite::tungstenite::Message::Text(t.as_str().into())
                }
                Message::Close(_) => break,
                _ => continue,
            };
            if up_tx.send(out).await.is_err() {
                break;
            }
        }
    };
    tokio::select! { _ = to_client => {}, _ = to_upstream => {} }
    Ok(())
}

/// `POST /w/{project}/hold` and `/release` (bearer = the box's MCP token, which the relay holds):
/// the relay stops a workspace while it copies a legacy box's files under it, then releases it.
async fn workspace_hold(
    State(b): State<B>,
    Path(project): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !box_admin_authed(&b, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    b.hold(&project).await;
    Json(json!({ "project": project, "held": true })).into_response()
}

async fn workspace_release(
    State(b): State<B>,
    Path(project): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !box_admin_authed(&b, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    b.release(&project).await;
    Json(json!({ "project": project, "held": false })).into_response()
}

fn box_admin_authed(b: &Supervisor, headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| !b.cfg.mcp_token.is_empty() && t.trim() == b.cfg.mcp_token)
        .unwrap_or(false)
}

pub fn box_router(sup: Arc<Supervisor>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/w/{project}/hold", axum::routing::post(workspace_hold))
        .route(
            "/w/{project}/release",
            axum::routing::post(workspace_release),
        )
        .route("/w/{project}/metrics", get(metrics))
        .route("/w/{project}/history/{chat_id}", get(history))
        .route("/w/{project}/t3/search", get(t3_search))
        .route("/history/{chat_id}", get(history_legacy))
        .route("/browser/stream", get(browser_stream))
        .with_state(sup)
}

// ---- loopback model stub (:7791) --------------------------------------------------------------

/// `/v1/messages` → relay Anthropic passthrough; everything else → relay OpenAI-compatible proxy.
async fn stub_forward(
    State(a): State<S>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // SDKs append their own `/v1/...` to a base URL that already ends in `/v1`; collapse it.
    let path = uri.path().replacen("/v1/v1/", "/v1/", 1);
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    tracing::debug!(%path, "model stub forward");
    let target = if path.starts_with("/v1/messages") || path.starts_with("/anthropic/") {
        format!(
            "{}/proxy/anthropic{}{}",
            a.cfg.relay_url,
            path.trim_start_matches("/anthropic"),
            query
        )
    } else {
        format!("{}/proxy{}{}", a.cfg.relay_url, path, query)
    };
    let client = reqwest::Client::new();
    // One box serves every project of the user: the relay meters usage per project by this header.
    let mut req = client
        .request(method, &target)
        .bearer_auth(&a.cfg.box_token)
        .header("x-arbos-project", &a.cfg.project_id)
        .body(body);
    for h in [
        "content-type",
        "accept",
        "anthropic-version",
        "anthropic-beta",
    ] {
        if let Some(v) = headers.get(h) {
            req = req.header(h, v);
        }
    }
    match req.send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mut out_headers = HeaderMap::new();
            for (k, v) in resp.headers() {
                if k == "content-type" || k == "cache-control" {
                    out_headers.insert(k.clone(), v.clone());
                }
            }
            let stream = resp
                .bytes_stream()
                .map(|r| r.map_err(std::io::Error::other));
            (status, out_headers, Body::from_stream(stream)).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("relay unreachable: {e}")).into_response(),
    }
}

pub fn stub_router(agent: Arc<Agent>) -> Router {
    Router::new()
        .route("/{*path}", any(stub_forward))
        .with_state(agent)
}
