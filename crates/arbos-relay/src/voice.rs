//! Agent voice calls (Calls Phase 2, D75). The relay sits between three parties:
//!
//! * the **clients**, which start a call in a chat exactly as in calls v1 — when the chat has a
//!   project agent (its own project chat, or a people chat the agent was added to) the relay also
//!   asks LiveKit to **dispatch the voice worker** (`arbos-voice`, on the relay host) into the
//!   room; it joins as `a_<project_id>`;
//! * the **voice worker**, which talks to the relay with the admin token over loopback:
//!   `GET /voice/calls/{id}` (context pack + caps), `POST /voice/calls/{id}/tool` (hand a request
//!   to the agent on the user's machine), `POST /voice/calls/{id}/events` (transcript lines,
//!   judge decisions, joined/left) and `POST /voice/calls/{id}/usage` (minutes, tokens, $ —
//!   charged to the owner's ledger as `voice` debits with `ref = call:<id>`; the answer carries
//!   the cost stops);
//! * the **box** (agentd), which keeps one WebSocket `WS /box/voice` open per user box; the relay
//!   forwards tool calls and transcript rows over it and waits for results, the same shape as
//!   the `arbos-local` helper link (`local.rs`).
//!
//! Cost rails: per-call cap (`ARBOS_VOICE_CALL_CAP_USD`, default $1.50), per-user daily voice cap
//! (`ARBOS_VOICE_DAILY_CAP_USD`, default $5, `PUT /admin/users/{id}/voice` per user), the user's
//! own credits/daily cap and the global kill switch. Owner setting per agent: chattiness
//! (`quiet` | `balanced` | `chatty`) and wake name, `GET/PUT /projects/{id}/voice`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};

use arbos_core::ids::{now_ms, ulid};

use crate::auth::Principal;
use crate::db::Project;
use crate::db_calls::CallRow;
use crate::db_voice::{VoiceDecision, VoiceUsage, CHATTINESS_LEVELS};
use crate::notify::{self, ChatRef, Sender};
use crate::proxy::start_of_utc_day_ms;
use crate::signin::api_error;
use crate::state::AppState;

const PING_EVERY: Duration = Duration::from_secs(20);
/// How long a tool call may take on the box before the worker is told to carry on without it.
const TOOL_WAIT: Duration = Duration::from_secs(240);
/// The context pack is cheap; a slow box must not delay the agent's first words.
const CONTEXT_WAIT: Duration = Duration::from_secs(4);
const FRAME_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Hard stop on one call with an agent in it (spec §6).
pub const CALL_MAX_MINUTES: i64 = 60;

/// Voice configuration from the environment.
#[derive(Debug, Clone)]
pub struct VoiceCfg {
    /// LiveKit agent name the worker registers with (`ARBOS_VOICE_AGENT_NAME`).
    pub agent_name: String,
    /// `ARBOS_VOICE_DAILY_CAP_USD` (default 5.0): per user, over every agent they own.
    pub daily_cap_usd: f64,
    /// `ARBOS_VOICE_CALL_CAP_USD` (default 1.50): per agent per call.
    pub call_cap_usd: f64,
    /// `ARBOS_VOICE=0` turns dispatch off while leaving people calls alone.
    pub enabled: bool,
    /// Speech model the worker is expected to use (for the admin view and the docs).
    pub model: String,
}

impl Default for VoiceCfg {
    fn default() -> Self {
        Self {
            agent_name: "arbos-voice".to_owned(),
            daily_cap_usd: 5.0,
            call_cap_usd: 1.5,
            enabled: true,
            model: "gpt-realtime-2.1".to_owned(),
        }
    }
}

fn env_f64(k: &str, default: f64) -> f64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(default)
}

pub fn cfg_from_env() -> VoiceCfg {
    let d = VoiceCfg::default();
    let cfg = VoiceCfg {
        agent_name: std::env::var("ARBOS_VOICE_AGENT_NAME")
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or(d.agent_name),
        daily_cap_usd: env_f64("ARBOS_VOICE_DAILY_CAP_USD", d.daily_cap_usd),
        call_cap_usd: env_f64("ARBOS_VOICE_CALL_CAP_USD", d.call_cap_usd),
        enabled: std::env::var("ARBOS_VOICE")
            .map(|v| v != "0")
            .unwrap_or(true),
        model: std::env::var("ARBOS_VOICE_MODEL")
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or(d.model),
    };
    tracing::info!(
        agent = %cfg.agent_name, model = %cfg.model, daily_cap = cfg.daily_cap_usd,
        call_cap = cfg.call_cap_usd, enabled = cfg.enabled, "agent voice calls"
    );
    cfg
}

// ---- box link ------------------------------------------------------------------------------------

/// One live box socket.
pub struct VoiceConn {
    pub tx: mpsc::Sender<String>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
}

pub type VoiceConns = Arc<Mutex<HashMap<String, Arc<VoiceConn>>>>;

/// `WS /box/voice` — the user's box keeps this open; one per box.
pub async fn ws_box_voice(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    principal: Principal,
) -> Response {
    let Principal::Box { user_id } = principal else {
        return (StatusCode::FORBIDDEN, "box token required").into_response();
    };
    ws.max_message_size(FRAME_MAX_BYTES)
        .on_upgrade(move |socket| run_box(socket, state, user_id))
}

async fn run_box(socket: WebSocket, state: AppState, user_id: String) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<String>(256);
    let conn = Arc::new(VoiceConn {
        tx,
        pending: Mutex::new(HashMap::new()),
    });
    let previous = state
        .voice_conns
        .lock()
        .await
        .insert(user_id.clone(), conn.clone());
    if let Some(prev) = previous {
        fail_pending(&prev, "box reconnected").await;
    }
    tracing::info!(user = %user_id, "voice link: box connected");
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
                let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                match frame["type"].as_str().unwrap_or_default() {
                    "result" => {
                        if let Some(id) = frame["id"].as_str() {
                            if let Some(waiter) = conn.pending.lock().await.remove(id) {
                                let _ = waiter.send(frame.clone());
                            }
                        }
                    }
                    "hello" | "pong" => {}
                    other => tracing::debug!(user = %user_id, kind = other, "voice link: unknown frame"),
                }
            }
            out = rx.recv() => {
                let Some(text) = out else { break };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            _ = ping.tick() => {
                if sink.send(Message::Text(json!({ "type": "ping" }).to_string().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    let mut conns = state.voice_conns.lock().await;
    if conns.get(&user_id).is_some_and(|c| Arc::ptr_eq(c, &conn)) {
        conns.remove(&user_id);
    }
    drop(conns);
    fail_pending(&conn, "box disconnected").await;
    tracing::info!(user = %user_id, "voice link: box disconnected");
}

async fn fail_pending(conn: &VoiceConn, why: &str) {
    for (_, waiter) in conn.pending.lock().await.drain() {
        let _ = waiter.send(json!({ "type": "result", "ok": false, "error": why }));
    }
}

/// Is the user's box on the voice link right now?
pub async fn box_online(state: &AppState, user_id: &str) -> bool {
    state.voice_conns.lock().await.contains_key(user_id)
}

/// Send `frame` to the box and wait for its `result` with the same id.
pub async fn box_request(
    state: &AppState,
    user_id: &str,
    mut frame: Value,
    wait: Duration,
) -> Result<Value, String> {
    let conn = state.voice_conns.lock().await.get(user_id).cloned();
    let Some(conn) = conn else {
        return Err("the agent's machine is not connected".to_owned());
    };
    let id = ulid();
    frame["id"] = json!(id);
    let (tx, rx) = oneshot::channel();
    conn.pending.lock().await.insert(id.clone(), tx);
    if conn.tx.send(frame.to_string()).await.is_err() {
        conn.pending.lock().await.remove(&id);
        return Err("the agent's machine link closed".to_owned());
    }
    match tokio::time::timeout(wait, rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err("the agent's machine disconnected".to_owned()),
        Err(_) => {
            conn.pending.lock().await.remove(&id);
            Err(format!(
                "no answer from the agent within {} s",
                wait.as_secs()
            ))
        }
    }
}

/// Send `frame` to the box without waiting.
pub async fn box_notify(state: &AppState, user_id: &str, frame: Value) -> Result<(), String> {
    let conn = state.voice_conns.lock().await.get(user_id).cloned();
    let Some(conn) = conn else {
        return Err("the agent's machine is not connected".to_owned());
    };
    conn.tx
        .send(frame.to_string())
        .await
        .map_err(|_| "the agent's machine link closed".to_owned())
}

// ---- who is in the chat ---------------------------------------------------------------------------

fn chat_ref_of(call: &CallRow) -> ChatRef {
    match &call.project_id {
        Some(p) => ChatRef::project(p, &call.chat_id),
        None => ChatRef::people(&call.chat_id),
    }
}

/// The project agents that belong to a chat: its own project for a project chat, the
/// `agent:<project>` members of a people chat.
pub fn agents_in_chat(state: &AppState, chat: &ChatRef) -> Vec<Project> {
    let ids: Vec<String> = match &chat.project_id {
        Some(pid) => vec![pid.clone()],
        None => state
            .db
            .members_of(&chat.chat_id)
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.project_id().map(str::to_owned))
            .collect(),
    };
    ids.iter()
        .filter_map(|id| state.db.project(id).ok().flatten())
        .collect()
}

/// Humans of the chat as `sender{}` + role (`owner` of the agent's project, `guest`, `member`).
fn humans_json(state: &AppState, chat: &ChatRef, agent: &Project) -> Vec<Value> {
    notify::recipients(state, chat)
        .iter()
        .map(|u| {
            let mut v = Sender::user(state, u).json();
            v["role"] = json!(if *u == agent.user_id {
                "owner"
            } else if chat.project_id.is_some() {
                "guest"
            } else {
                "member"
            });
            v
        })
        .collect()
}

/// Identity the worker joins with.
pub fn agent_identity(project_id: &str) -> String {
    format!("a_{project_id}")
}

pub fn project_of_identity(identity: &str) -> Option<&str> {
    identity.strip_prefix("a_")
}

/// Live agents of a call as `sender{}` (for `participants[]`).
pub fn agent_participants_json(state: &AppState, call_id: &str) -> Vec<Value> {
    state
        .db
        .call_agents_live(call_id)
        .unwrap_or_default()
        .iter()
        .filter_map(|pid| state.db.project(pid).ok().flatten())
        .map(|p| Sender::agent(state, &p).json())
        .collect()
}

// ---- dispatch -----------------------------------------------------------------------------------

/// A dispatch younger than this still counts as "on its way" (the worker joins within ~1 s).
const DISPATCH_FRESH_MS: i64 = 30_000;

/// A call began in a chat with agents: ask LiveKit to send the voice worker for each of them.
/// Returns the agents dispatched (as `sender{}`), for the start response.
pub async fn dispatch_agents(state: &AppState, call: &CallRow) -> Vec<Value> {
    dispatch_missing(state, call).await
}

/// Someone joined a live call: any agent of the chat that is not in the room (it left when it
/// was alone, or its job died) is dispatched again.
pub async fn ensure_agents(state: &AppState, call: &CallRow) {
    if call.active() {
        let _ = dispatch_missing(state, call).await;
    }
}

async fn dispatch_missing(state: &AppState, call: &CallRow) -> Vec<Value> {
    let (Some(lk), true) = (&state.livekit, state.voice.enabled) else {
        return Vec::new();
    };
    let chat = chat_ref_of(call);
    let live = state.db.call_agents_live(&call.id).unwrap_or_default();
    let now = now_ms();
    let mut out = Vec::new();
    for project in agents_in_chat(state, &chat) {
        if live.contains(&project.id) {
            continue;
        }
        let key = format!("{}:{}", call.id, project.id);
        {
            let mut recent = state.voice_dispatched.lock().await;
            recent.retain(|_, at| now - *at < DISPATCH_FRESH_MS);
            if recent.contains_key(&key) {
                continue;
            }
            recent.insert(key, now);
        }
        let payer = project.user_id.clone();
        if let Err(stop) = stops_for(state, &payer, &call.id, &project.id) {
            tracing::info!(call = %call.id, project = %project.id, stop, "voice: agent not dispatched (cost stop)");
            continue;
        }
        let settings = state.db.voice_agent_settings(&project.id).ok();
        let meta = json!({
            "call_id": call.id,
            "chat_id": call.chat_id,
            "chat_project": chat.project_label(),
            "project_id": project.id,
            "room": call.room,
            "agent": Sender::agent(state, &project).json(),
            "identity": agent_identity(&project.id),
            "chattiness": settings.as_ref().map(|s| s.chattiness.clone()).unwrap_or_else(|| "balanced".to_owned()),
            "wake_name": settings.and_then(|s| s.wake_name),
            "started_at": call.started_at,
        });
        match lk
            .create_dispatch(&state.http, &call.room, &state.voice.agent_name, &meta)
            .await
        {
            Ok(id) => {
                tracing::info!(call = %call.id, project = %project.id, dispatch = %id, "voice: agent dispatched");
                out.push(Sender::agent(state, &project).json());
            }
            Err(e) => {
                tracing::warn!(call = %call.id, project = %project.id, error = %e, "voice: dispatch failed")
            }
        }
    }
    out
}

// ---- cost stops ----------------------------------------------------------------------------------

/// Why the agent must not (keep) talk(ing): `kill_switch`, `credits`, `daily_cap`,
/// `voice_daily_cap`, `call_cap`. `Ok` while every rail has room.
pub fn stops_for(
    state: &AppState,
    payer: &str,
    call_id: &str,
    project_id: &str,
) -> Result<(), &'static str> {
    if crate::credits::check_read_only(state).is_err() {
        return Err("kill_switch");
    }
    match crate::credits::check(state, payer) {
        Ok(()) => {}
        Err(crate::db_accounts::DebitRefused::DailyCap { .. }) => return Err("daily_cap"),
        Err(crate::db_accounts::DebitRefused::ReadOnly { .. }) => return Err("kill_switch"),
        Err(crate::db_accounts::DebitRefused::Insufficient { .. }) => return Err("credits"),
    }
    if crate::credits::metered(state) {
        let cap = state
            .db
            .voice_user_cap(payer)
            .ok()
            .flatten()
            .unwrap_or(state.voice.daily_cap_usd);
        let spent = state
            .db
            .voice_spent_since(payer, start_of_utc_day_ms())
            .unwrap_or(0.0);
        if spent >= cap {
            return Err("voice_daily_cap");
        }
    }
    if let Ok(Some(row)) = state.db.voice_call(call_id, project_id) {
        if row.usage.usd >= state.voice.call_cap_usd {
            return Err("call_cap");
        }
    }
    Ok(())
}

fn caps_json(state: &AppState, payer: &str, call_id: &str, project_id: &str) -> Value {
    let cap = state
        .db
        .voice_user_cap(payer)
        .ok()
        .flatten()
        .unwrap_or(state.voice.daily_cap_usd);
    let spent = state
        .db
        .voice_spent_since(payer, start_of_utc_day_ms())
        .unwrap_or(0.0);
    let call_usd = state
        .db
        .voice_call(call_id, project_id)
        .ok()
        .flatten()
        .map(|r| r.usage.usd)
        .unwrap_or(0.0);
    json!({
        "call_cap_usd": state.voice.call_cap_usd,
        "call_spent_usd": call_usd,
        "daily_cap_usd": cap,
        "daily_spent_usd": spent,
        "call_max_minutes": CALL_MAX_MINUTES,
        "metered": crate::credits::metered(state),
        "stop": stops_for(state, payer, call_id, project_id).err(),
    })
}

// ---- worker endpoints (admin token, loopback) ------------------------------------------------------

fn worker_only(principal: &Principal) -> Result<(), Box<Response>> {
    if matches!(principal, Principal::Admin) {
        Ok(())
    } else {
        Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "voice worker token required",
        )))
    }
}

#[derive(Deserialize, Default)]
pub struct ProjectQuery {
    #[serde(default)]
    pub project_id: Option<String>,
}

/// The call, its agent and the context pack for the worker.
fn load(
    state: &AppState,
    call_id: &str,
    project_id: Option<&str>,
) -> Result<(CallRow, Project, ChatRef), Box<Response>> {
    let call = state.db.call(call_id).ok().flatten().ok_or_else(|| {
        Box::new(api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such call",
        ))
    })?;
    let chat = chat_ref_of(&call);
    let agents = agents_in_chat(state, &chat);
    let project = match project_id {
        Some(pid) => agents.into_iter().find(|p| p.id == pid),
        None if agents.len() == 1 => agents.into_iter().next(),
        None => None,
    }
    .ok_or_else(|| {
        Box::new(api_error(
            StatusCode::NOT_FOUND,
            "no_agent",
            "this call's chat has no such agent",
        ))
    })?;
    Ok((call, project, chat))
}

/// `GET /voice/calls/{id}?project_id=` → `{ call, project, chat, humans, agents, settings, caps,
/// context, box_online }`. `context` is what the box composed for this call (voice system prompt,
/// recent lines); null when the box is offline.
pub async fn call_info(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Query(q): Query<ProjectQuery>,
) -> Response {
    if let Err(r) = worker_only(&principal) {
        return *r;
    }
    let (call, project, chat) = match load(&state, &id, q.project_id.as_deref()) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let humans = humans_json(&state, &chat, &project);
    let settings = state
        .db
        .voice_agent_settings(&project.id)
        .map(|s| json!({ "chattiness": s.chattiness, "wake_name": s.wake_name }))
        .unwrap_or_else(|_| json!({ "chattiness": "balanced", "wake_name": null }));
    let online = box_online(&state, &project.user_id).await;
    let context = if online {
        box_request(
            &state,
            &project.user_id,
            json!({
                "type": "voice_context", "call_id": call.id, "chat_id": call.chat_id,
                "chat_project": chat.project_label(), "project_id": project.id, "humans": humans,
            }),
            CONTEXT_WAIT,
        )
        .await
        .ok()
        .filter(|v| v["ok"].as_bool().unwrap_or(false))
        .map(|v| v["context"].clone())
    } else {
        None
    };
    let chat_kind = match &chat.project_id {
        Some(pid) => state
            .db
            .chat(pid, &chat.chat_id)
            .ok()
            .flatten()
            .map(|c| c.kind)
            .unwrap_or_else(|| "chat".to_owned()),
        None => state
            .db
            .people_chat(&chat.chat_id)
            .ok()
            .flatten()
            .map(|c| c.kind)
            .unwrap_or_else(|| "dm".to_owned()),
    };
    let agents: Vec<Value> = agents_in_chat(&state, &chat)
        .iter()
        .map(|p| Sender::agent(&state, p).json())
        .collect();
    Json(json!({
        "call": crate::calls::call_json_public(&state, &call),
        "project": {
            "id": project.id, "name": project.name, "description": project.description,
            "instructions": project.instructions, "user_id": project.user_id,
            "model": project.models.get("talking_model").and_then(|m| m.get("id")).cloned().unwrap_or(Value::Null),
            "sender": Sender::agent(&state, &project).json(),
            "identity": agent_identity(&project.id),
        },
        "chat": { "chat_id": chat.chat_id, "project_id": chat.project_label(), "kind": chat_kind },
        "humans": humans,
        "agents": agents,
        "settings": settings,
        "caps": caps_json(&state, &project.user_id, &call.id, &project.id),
        "context": context,
        "box_online": online,
        "model": state.voice.model,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct ToolBody {
    #[serde(default)]
    pub project_id: Option<String>,
    /// Who asked (`sender{}` + `role`), as the worker heard it.
    #[serde(default)]
    pub speaker: Value,
    /// What they asked for, in the agent's words.
    pub request: String,
    /// `duplex` (1:1) or `group`.
    #[serde(default)]
    pub mode: Option<String>,
    /// The worker's id for this request (returned as is).
    #[serde(default)]
    pub request_id: Option<String>,
}

/// `POST /voice/calls/{id}/tool` → runs the request as a turn of the agent on the user's machine
/// (pi main session + local MCP, the same limits as text) and answers `{ ok, text, request_id }`.
/// 504 when the box did not finish within `TOOL_WAIT` (the worker says it will report later);
/// 503 when the box is offline.
pub async fn call_tool(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<ToolBody>,
) -> Response {
    if let Err(r) = worker_only(&principal) {
        return *r;
    }
    let (call, project, chat) = match load(&state, &id, b.project_id.as_deref()) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if b.request.trim().is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "request is empty");
    }
    if !call.active() {
        return api_error(StatusCode::GONE, "call_ended", "this call has ended");
    }
    if let Err(stop) = stops_for(&state, &project.user_id, &call.id, &project.id) {
        return api_error(
            StatusCode::PAYMENT_REQUIRED,
            stop,
            "voice cost stop reached",
        );
    }
    let frame = json!({
        "type": "voice_tool", "call_id": call.id, "chat_id": call.chat_id,
        "chat_project": chat.project_label(), "project_id": project.id,
        "speaker": b.speaker, "request": b.request, "mode": b.mode,
    });
    let t0 = std::time::Instant::now();
    match box_request(&state, &project.user_id, frame, TOOL_WAIT).await {
        Ok(v) => {
            let ok = v["ok"].as_bool().unwrap_or(false);
            tracing::info!(call = %call.id, project = %project.id, ok, ms = t0.elapsed().as_millis() as i64, "voice: tool call answered");
            let mut out = json!({
                "ok": ok, "text": v["text"], "error": v["error"], "request_id": b.request_id,
                "ms": t0.elapsed().as_millis() as i64,
            });
            if let Some(msg) = v.get("message_id") {
                out["message_id"] = msg.clone();
            }
            Json(out).into_response()
        }
        Err(e) if e.starts_with("no answer") => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({ "ok": false, "error": e, "request_id": b.request_id, "pending": true })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ok": false, "error": e, "request_id": b.request_id })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct EventsBody {
    #[serde(default)]
    pub project_id: Option<String>,
    pub events: Vec<Value>,
}

/// `POST /voice/calls/{id}/events` — transcript lines and system lines go to the box as chat
/// rows; judge decisions are kept for the admin view and the owner's Details drawer; joined/left
/// keep the participant list honest. Answers `{ accepted, forwarded }`.
pub async fn call_events(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<EventsBody>,
) -> Response {
    if let Err(r) = worker_only(&principal) {
        return *r;
    }
    let (call, project, chat) = match load(&state, &id, b.project_id.as_deref()) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let mut rows = Vec::new();
    let mut accepted = 0usize;
    for ev in &b.events {
        accepted += 1;
        let at = ev["at"].as_i64().unwrap_or_else(now_ms);
        match ev["kind"].as_str().unwrap_or_default() {
            "transcript" | "system" => rows.push(ev.clone()),
            "decision" => {
                let d = VoiceDecision {
                    speaker: ev["speaker"].as_str().map(str::to_owned),
                    text: ev["text"].as_str().map(str::to_owned),
                    decision: ev["decision"].as_str().unwrap_or("silent").to_owned(),
                    forced: ev["forced"].as_bool().unwrap_or(false),
                    p_speak: ev["p_speak"].as_f64(),
                    p_act: ev["p_act"].as_f64(),
                    p_silent: ev["p_silent"].as_f64(),
                    provider: ev["provider"].as_str().map(str::to_owned),
                    latency_ms: ev["latency_ms"].as_i64(),
                    cost_usd: ev["cost_usd"].as_f64(),
                };
                let _ = state
                    .db
                    .voice_decision_insert(&call.id, &project.id, at, &d);
            }
            "joined" => {
                let _ = state.db.call_agent_join(&call.id, &project.id);
                let _ = state.db.touch_call(&call.id);
            }
            "left" => {
                let _ = state.db.call_agent_leave(&call.id, &project.id);
            }
            "metric" | "tool" => {
                tracing::info!(call = %call.id, project = %project.id, event = %ev, "voice event");
            }
            other => tracing::debug!(call = %call.id, kind = other, "voice: unknown event"),
        }
    }
    let mut forwarded = false;
    if !rows.is_empty() {
        forwarded = box_notify(
            &state,
            &project.user_id,
            json!({
                "type": "voice_rows", "call_id": call.id, "chat_id": call.chat_id,
                "chat_project": chat.project_label(), "project_id": project.id, "rows": rows,
            }),
        )
        .await
        .is_ok();
    }
    Json(json!({ "accepted": accepted, "forwarded": forwarded })).into_response()
}

#[derive(Deserialize)]
pub struct UsageBody {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(flatten)]
    pub usage: VoiceUsage,
}

/// `POST /voice/calls/{id}/usage` — cumulative usage for this agent in this call. The relay
/// debits the difference since the last report from the owner's ledger (`voice`,
/// `ref = call:<id>`), mirrors it into the project's `usage` table (so the project's spend cap
/// counts it), and answers the caps: `{ caps: { stop: null | reason, … }, usd_charged }`.
pub async fn call_usage(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<UsageBody>,
) -> Response {
    if let Err(r) = worker_only(&principal) {
        return *r;
    }
    let (call, project, _) = match load(&state, &id, b.project_id.as_deref()) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let payer = project.user_id.clone();
    let row =
        match state
            .db
            .voice_usage_upsert(&call.id, &project.id, &payer, call.started_at, &b.usage)
        {
            Ok(r) => r,
            Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
        };
    let delta = row.usage.usd - row.usd_charged;
    if delta > 0.000_001 {
        let reference = format!("call:{}", call.id);
        crate::credits::charge(&state, &payer, delta, "voice", Some(&reference));
        let _ = state.db.voice_charged(&call.id, &project.id, delta);
        let _ = state.db.record_usage(
            &project.id,
            "voice",
            &state.voice.model,
            b.usage.audio_in_tokens + b.usage.text_in_tokens,
            b.usage.audio_out_tokens + b.usage.text_out_tokens,
            b.usage.cached_tokens,
            0,
            delta,
            (b.usage.seconds * 1000.0) as i64,
            b.usage.ttfa_ms_p50,
        );
    }
    if b.usage.ended {
        tracing::info!(
            call = %call.id, project = %project.id, seconds = b.usage.seconds, usd = b.usage.usd,
            tool_calls = b.usage.tool_calls, decisions = b.usage.decisions, turns = b.usage.turns_spoken,
            ttfa_p50 = ?b.usage.ttfa_ms_p50, barge_p50 = ?b.usage.barge_ms_p50, stop = ?b.usage.stop_reason,
            "voice: call usage final"
        );
        let _ = state.db.call_agent_leave(&call.id, &project.id);
    }
    Json(json!({
        "caps": caps_json(&state, &payer, &call.id, &project.id),
        "usd_charged": row.usd_charged + delta.max(0.0),
    }))
    .into_response()
}

// ---- owner settings -------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct AgentSettingsBody {
    #[serde(default)]
    pub chattiness: Option<String>,
    /// `Some(None)` clears it (JSON `null`).
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub wake_name: Option<Option<String>>,
}

fn deserialize_double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(d).map(Some)
}

fn settings_json(s: &crate::db_voice::VoiceAgentSettings) -> Value {
    json!({ "project_id": s.project_id, "chattiness": s.chattiness, "wake_name": s.wake_name, "levels": CHATTINESS_LEVELS })
}

/// `GET /projects/{id}/voice` → `{ chattiness, wake_name }` (owner, guests, the box, the admin).
pub async fn get_agent_settings(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    if !state.allows(&principal, &id) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "not your project");
    }
    let Some(p) = state.db.project(&id).ok().flatten() else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such project");
    };
    match state.db.voice_agent_settings(&p.id) {
        Ok(s) => Json(settings_json(&s)).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `PUT /projects/{id}/voice {chattiness?, wake_name?}` (owner or admin).
pub async fn put_agent_settings(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<AgentSettingsBody>,
) -> Response {
    let Some(p) = state.db.project(&id).ok().flatten() else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such project");
    };
    let owner = match &principal {
        Principal::Admin => true,
        Principal::Device { user_id, .. } => *user_id == p.user_id,
        Principal::Box { .. } => false,
    };
    if !owner {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "only the agent's owner changes its voice settings",
        );
    }
    if let Some(c) = b.chattiness.as_deref() {
        if !CHATTINESS_LEVELS.contains(&c) {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "chattiness must be quiet, balanced or chatty",
            );
        }
    }
    let wake = b.wake_name.as_ref().map(|w| {
        w.as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(|w| w.chars().take(32).collect::<String>())
    });
    match state.db.set_voice_agent_settings(
        &p.id,
        b.chattiness.as_deref(),
        wake.as_ref().map(|w| w.as_deref()),
    ) {
        Ok(s) => Json(settings_json(&s)).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct UserCapBody {
    /// `null` returns the user to the relay default.
    pub daily_cap_usd: Option<f64>,
}

/// `PUT /admin/users/{id}/voice {daily_cap_usd}` → `{ user_id, daily_cap_usd, default }`.
pub async fn put_user_cap(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<UserCapBody>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    if let Some(c) = b.daily_cap_usd {
        if !c.is_finite() || c < 0.0 {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "daily_cap_usd must be a non-negative number",
            );
        }
    }
    if let Err(e) = state.db.set_voice_user_cap(&id, b.daily_cap_usd) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    Json(json!({
        "user_id": id, "daily_cap_usd": b.daily_cap_usd.unwrap_or(state.voice.daily_cap_usd),
        "default": b.daily_cap_usd.is_none(),
        "spent_today_usd": state.db.voice_spent_since(&id, start_of_utc_day_ms()).unwrap_or(0.0),
    }))
    .into_response()
}

/// The voice part of `GET /admin/calls`: config and the last agent calls with their cost log.
pub fn admin_json(state: &AppState) -> Value {
    let recent: Vec<Value> = state
        .db
        .voice_calls_recent(20)
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let minutes = r.usage.seconds / 60.0;
            json!({
                "call_id": r.call_id, "project_id": r.project_id, "payer": r.payer, "mode": r.usage.mode,
                "started_at": r.started_at, "ended_at": r.ended_at, "minutes": (minutes * 100.0).round() / 100.0,
                "audio_in_tokens": r.usage.audio_in_tokens, "audio_out_tokens": r.usage.audio_out_tokens,
                "text_in_tokens": r.usage.text_in_tokens, "text_out_tokens": r.usage.text_out_tokens,
                "cached_tokens": r.usage.cached_tokens, "stt_seconds": r.usage.stt_seconds,
                "decisions": r.usage.decisions, "tool_calls": r.usage.tool_calls, "turns_spoken": r.usage.turns_spoken,
                "usd": r.usage.usd, "usd_charged": r.usd_charged,
                "usd_per_minute": if minutes > 0.0 { Some(r.usage.usd / minutes) } else { None },
                "ttfa_ms_p50": r.usage.ttfa_ms_p50, "ttfa_ms_p95": r.usage.ttfa_ms_p95,
                "barge_ms_p50": r.usage.barge_ms_p50, "barge_ms_max": r.usage.barge_ms_max,
                "stop_reason": r.usage.stop_reason,
                "decisions_log": state.db.voice_decisions_of(&r.call_id, 50).unwrap_or_default(),
            })
        })
        .collect();
    json!({
        "enabled": state.voice.enabled && state.livekit.is_some(),
        "agent_name": state.voice.agent_name,
        "model": state.voice.model,
        "daily_cap_usd": state.voice.daily_cap_usd,
        "call_cap_usd": state.voice.call_cap_usd,
        "call_max_minutes": CALL_MAX_MINUTES,
        "boxes_linked": state.voice_conns.try_lock().map(|c| c.len()).unwrap_or(0),
        "recent": recent,
    })
}
