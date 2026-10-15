//! Loopback hook API for `arbos-pi-ext` (sandbox-image.md §3.3): route validation,
//! ask_user, act limit, handles, working set, scratchpad.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::agent::Agent;

type S = Arc<Agent>;

fn authed(agent: &Agent, headers: &HeaderMap) -> bool {
    let tok = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    tok == agent.cfg.mcp_token
}

fn ok() -> Response {
    Json(json!({ "ok": true })).into_response()
}

fn err(e: impl std::fmt::Display) -> Response {
    Json(json!({ "ok": false, "error": e.to_string() })).into_response()
}

async fn route(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match a.hook_route(b.get("args").unwrap_or(&Value::Null)).await {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

async fn ask_user(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let q = b
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let sid = b
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    match a.hook_ask_user(&q, &sid).await {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

async fn act_limit(State(a): State<S>, headers: HeaderMap, Json(_b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    a.hook_act_limit().await;
    ok()
}

async fn deny(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let sid = b
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let _ = a.store.t3_append(
        &sid,
        None,
        Some(&sid),
        "deny",
        Some("system"),
        &format!("denied tool call: {b}"),
        None,
    );
    tracing::warn!(deny = %b, "deny-list hit");
    ok()
}

async fn handle(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let sid = b
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let h = b.get("handle").cloned().unwrap_or(Value::Null);
    let id = h.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
    let path = h
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if id.is_empty() || path.is_empty() {
        return err("handle needs id and path");
    }
    let blob_key = std::fs::read(&path)
        .ok()
        .map(|bytes| a.upload_blob(bytes, "text/plain"));
    let r = a.store.upsert_handle(
        &id,
        Some(&sid),
        Some(&sid),
        h.get("kind").and_then(Value::as_str).unwrap_or("output"),
        h.get("chars").and_then(Value::as_i64).unwrap_or(0),
        h.get("lines").and_then(Value::as_i64).unwrap_or(0),
        &path,
        h.get("note").and_then(Value::as_str).unwrap_or(""),
        blob_key.as_deref(),
    );
    match r {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

async fn handle_path(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let id = b.get("handle").and_then(Value::as_str).unwrap_or("");
    match a.store.handle(id) {
        Ok(Some(h)) => Json(json!({ "ok": true, "text": h.path })).into_response(),
        _ => err("unknown handle"),
    }
}

async fn working_set(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let sid = b
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let chat_id = a
        .turn
        .lock()
        .await
        .as_ref()
        .map(|t| t.chat_id.clone())
        .unwrap_or(sid);
    Json(json!({ "ok": true, "text": a.working_set_text(&chat_id) })).into_response()
}

async fn remember(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let sid = b
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let chat_id = a
        .turn
        .lock()
        .await
        .as_ref()
        .map(|t| t.chat_id.clone())
        .unwrap_or(sid);
    let name = b.get("name").and_then(Value::as_str).unwrap_or("");
    let value = b.get("value").and_then(Value::as_str);
    match a.store.scratch_set(&chat_id, name, value) {
        Ok(()) => ok(),
        Err(e) => err(e),
    }
}

async fn noop(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    tracing::debug!(hook = %b, "hook event");
    ok()
}

async fn turn_without_route(
    State(a): State<S>,
    headers: HeaderMap,
    Json(b): Json<Value>,
) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let corrected = b.get("corrected").and_then(Value::as_bool).unwrap_or(false);
    let sid = b
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    tracing::warn!(session = %sid, corrected, "turn ended without a route call");
    let _ = a.store.t3_append(
        &sid,
        None,
        Some(&sid),
        "route",
        Some("system"),
        &format!("turn without route; corrective continuation={corrected}"),
        None,
    );
    if let Some(t) = a.turn.lock().await.as_mut() {
        t.metrics.route_missing = true;
    }
    ok()
}

async fn route_corrected(State(a): State<S>, headers: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&a, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    tracing::info!(action = ?b.get("action"), "route recorded by the post-hoc guard");
    if let Some(t) = a.turn.lock().await.as_mut() {
        t.metrics.route_corrected = true;
    }
    ok()
}

pub fn router(agent: Arc<Agent>) -> Router {
    Router::new()
        .route("/hook/route", post(route))
        .route("/hook/ask_user", post(ask_user))
        .route("/hook/act-limit", post(act_limit))
        .route("/hook/deny", post(deny))
        .route("/hook/handle", post(handle))
        .route("/hook/handle-path", post(handle_path))
        .route("/hook/working-set", post(working_set))
        .route("/hook/remember", post(remember))
        .route("/hook/compacted", post(noop))
        .route("/hook/turn-without-route", post(turn_without_route))
        .route("/hook/route-corrected", post(route_corrected))
        .with_state(agent)
}
