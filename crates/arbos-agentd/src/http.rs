//! The box's HTTP surfaces: `:7788` (`/healthz`, `/browser/stream`, `/history`, `/metrics`)
//! and the loopback model stub `:7791` that forwards to the relay proxy with the box token.

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

type S = Arc<Agent>;

async fn healthz(State(a): State<S>) -> Response {
    let ok = a.is_healthy();
    let body = json!({ "ok": ok, "pi": a.pi_ready.load(std::sync::atomic::Ordering::SeqCst), "extension": a.ext_loaded.load(std::sync::atomic::Ordering::SeqCst), "project": a.cfg.project_id, "device": a.cfg.device_id, "version": env!("CARGO_PKG_VERSION") });
    (
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(body),
    )
        .into_response()
}

async fn metrics(State(a): State<S>) -> Response {
    Json(json!({ "turns": a.store.turn_metrics(200).unwrap_or_default() })).into_response()
}

async fn history(
    State(a): State<S>,
    Path(chat_id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let limit = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50);
    match a
        .store
        .t3_thread(&chat_id, None, q.get("before").map(String::as_str), limit)
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn t3_search(
    State(a): State<S>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let query = q.get("q").cloned().unwrap_or_default();
    match a.store.t3_search(&query, None, None, 20, 0) {
        Ok(hits) => Json(hits.into_iter().map(|(r, s, snip)| json!({ "id": r.id, "thread_id": r.thread_id, "kind": r.kind, "score": -s, "snippet": snip })).collect::<Vec<_>>()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// S16: proxy agent-browser's JPEG stream (`ws://127.0.0.1:9223`) on `/browser/stream`.
async fn browser_stream(ws: WebSocketUpgrade, State(a): State<S>) -> Response {
    let _ = a;
    ws.on_upgrade(|socket| async move {
        if let Err(e) = pipe_browser(socket).await {
            tracing::debug!(error = %e, "browser stream closed");
        }
    })
}

async fn pipe_browser(client: WebSocket) -> anyhow::Result<()> {
    let upstream_url =
        std::env::var("ARBOS_BROWSER_STREAM_URL").unwrap_or_else(|_| "ws://127.0.0.1:9223".into());
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

pub fn router(agent: Arc<Agent>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route("/history/{chat_id}", get(history))
        .route("/t3/search", get(t3_search))
        .route("/browser/stream", get(browser_stream))
        .with_state(agent)
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
    let path = uri.path();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
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
    let mut req = client
        .request(method, &target)
        .bearer_auth(&a.cfg.box_token)
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
