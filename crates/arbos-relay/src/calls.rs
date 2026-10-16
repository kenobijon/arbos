//! Calls v1: voice/video between people, 1:1 and group, on a self-hosted LiveKit next to the
//! relay. The relay owns the call's life (who may join, who is in, when it ends) and mints
//! LiveKit join tokens; media never touches it.
//!
//! Endpoints (bearer = device access token): `POST /chats/{id}/calls` and
//! `POST /projects/{p}/chats/{c}/calls` (start, or join the live one), `GET /chats/{id}/call`
//! (+ projects form), `GET /calls/{id}`, `POST /calls/{id}/join`, `POST /calls/{id}/leave`,
//! `POST/DELETE /devices/{id}/voip`, `POST /livekit/webhook`, `GET /admin/calls`.
//!
//! Signalling: `call_started` / `call_ended` on `/me/pushes`, an APNs alert with category
//! `ARBOS_CALL`, and a PushKit VoIP push on start. System lines in the chat are written by the
//! clients (the starter on 201; the one client whose leave answered `ended: true`, or the device
//! named in `call_ended.writer_device`).

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};

use crate::apns::{Delivery, Outgoing};
use crate::auth::Principal;
use crate::db_calls::{CallRow, VoipToken};
use crate::notify::{self, ChatRef, Sender};
use crate::people;
use crate::signin::api_error;
use crate::state::AppState;

pub const CATEGORY: &str = "ARBOS_CALL";
/// Nobody joined (not even the starter's LiveKit connection) within this: the call is abandoned.
const EMPTY_START_MS: i64 = 90_000;
/// Everyone gone for this long without a `room_finished`: ended as abandoned.
const EMPTY_AFTER_MS: i64 = 60_000;
/// Hard cap on a call row that stays live with no activity at all (lost webhooks and leaves).
const STALE_MS: i64 = 8 * 3_600_000;

fn identity_of(user_id: &str) -> String {
    format!("u_{user_id}")
}

fn user_of_identity(identity: &str) -> Option<&str> {
    identity.strip_prefix("u_")
}

/// Live participants as `sender{}` objects.
fn participants_json(state: &AppState, call: &CallRow) -> Vec<Value> {
    state
        .db
        .call_participants(&call.id)
        .unwrap_or_default()
        .iter()
        .filter(|p| p.left_at.is_none())
        .map(|p| Sender::user(state, &p.user_id).json())
        .collect()
}

/// The call object; with a fresh token for `for_user` when the call is live.
fn call_json(state: &AppState, call: &CallRow, for_user: Option<&str>) -> Value {
    let mut v = json!({
        "call_id": call.id,
        "chat_id": call.chat_id,
        "project_id": call.project_id.clone().unwrap_or_else(|| people::PEOPLE_PROJECT.to_owned()),
        "kind": call.kind,
        "room": call.room,
        "livekit_url": state.livekit.as_ref().map(|l| l.url.clone()),
        "started_by": Sender::user(state, &call.started_by).json(),
        "started_at": call.started_at,
        "ended_at": call.ended_at,
        "reason": call.end_reason,
        "participants": participants_json(state, call),
        "max_participants": crate::livekit::MAX_PARTICIPANTS,
    });
    if let (Some(lk), Some(user), true) = (&state.livekit, for_user, call.active()) {
        let me = Sender::user(state, user);
        let (token, exp) = lk.join_token(&call.room, &identity_of(user), &me.name, &me.json());
        v["token"] = json!(token);
        v["token_expires_at"] = json!(exp);
    }
    v
}

fn chat_ref_of(call: &CallRow) -> ChatRef {
    match &call.project_id {
        Some(p) => ChatRef::project(p, &call.chat_id),
        None => ChatRef::people(&call.chat_id),
    }
}

/// Members of the chat the call lives in (humans only).
fn members(state: &AppState, chat: &ChatRef) -> Vec<String> {
    notify::recipients(state, chat)
}

fn is_member(state: &AppState, chat: &ChatRef, user_id: &str) -> bool {
    members(state, chat).iter().any(|u| u == user_id)
}

fn calls_off() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "calls_off",
        "this relay has no LiveKit (LIVEKIT_URL / LIVEKIT_API_KEY / LIVEKIT_API_SECRET)",
    )
}

fn device_user(principal: &Principal) -> Result<(String, String), Box<Response>> {
    match principal {
        Principal::Device { id, user_id, .. } => Ok((id.clone(), user_id.clone())),
        Principal::Box { .. } | Principal::Admin => Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a signed-in device is required",
        ))),
    }
}

// ---- signalling -----------------------------------------------------------------------------------

async fn deliver_voip(state: &AppState, t: &VoipToken, payload: Value) {
    let out = Outgoing {
        device_token: t.token.clone(),
        environment: t.environment.clone(),
        push_type: "voip",
        topic: Some(format!("{}.voip", t.bundle_id)),
        collapse_id: None,
        payload,
    };
    match state.push.send(out).await {
        Delivery::Sent => {}
        Delivery::TokenGone => {
            let _ = state.db.delete_voip_token_value(&t.token);
        }
        Delivery::Failed(e) => {
            tracing::warn!(device = %t.device_id, error = %e, "VoIP push failed")
        }
    }
}

async fn deliver_alert(
    state: &AppState,
    t: &crate::db_push::PushToken,
    payload: Value,
    collapse: String,
) {
    let out = Outgoing {
        device_token: t.token.clone(),
        environment: t.environment.clone(),
        push_type: "alert",
        topic: None,
        collapse_id: Some(collapse),
        payload,
    };
    match state.push.send(out).await {
        Delivery::Sent => {}
        Delivery::TokenGone => {
            let _ = state.db.delete_push_token_value(&t.token);
        }
        Delivery::Failed(e) => {
            tracing::warn!(device = %t.device_id, error = %e, "call alert failed")
        }
    }
}

/// A call began: `call_started` on `/me/pushes`, a VoIP push (CallKit) and an APNs alert to
/// every other member's devices.
async fn announce_started(state: &AppState, call: &CallRow, starter: &Sender) {
    let chat = chat_ref_of(call);
    let call_v = call_json(state, call, None);
    let title = starter.title();
    let what = if call.kind == "video" {
        "video call"
    } else {
        "voice call"
    };
    for user in members(state, &chat) {
        if user == call.started_by {
            continue;
        }
        let _ = state.db.push_user(
            &user,
            "call_started",
            &json!({ "call": call_v, "sender": starter.json() }),
            None,
        );
        if !state.push.is_on() {
            continue;
        }
        for t in state.db.voip_tokens_of_user(&user).unwrap_or_default() {
            deliver_voip(
                state,
                &t,
                json!({
                    "call_id": call.id, "chat_id": call.chat_id, "project_id": call_v["project_id"],
                    "kind": call.kind, "room": call.room, "livekit_url": call_v["livekit_url"],
                    "caller": starter.json(), "started_at": call.started_at,
                }),
            )
            .await;
        }
        for t in state.db.push_tokens_of_user(&user).unwrap_or_default() {
            let payload = json!({
                "aps": {
                    "alert": { "title": title, "body": format!("Incoming {what}") },
                    "sound": "default",
                    "thread-id": call.chat_id,
                    "category": CATEGORY,
                    "mutable-content": 1,
                },
                "event": "started",
                "chat_id": call.chat_id,
                "project_id": call_v["project_id"],
                "call": call_v,
                "sender": starter.json(),
            });
            deliver_alert(state, &t, payload, format!("call:{}", call.id)).await;
        }
    }
}

/// A call is over: `call_ended` to every member (one device is asked to write the system line)
/// and an APNs alert that replaces the ringing banner.
async fn announce_ended(
    state: &AppState,
    call: &CallRow,
    reason: &str,
    writer_device: Option<&str>,
) {
    let chat = chat_ref_of(call);
    let ended_at = call.ended_at.unwrap_or_else(now_ms);
    let duration_s = ((ended_at - call.started_at) / 1000).max(0);
    let project_label = call
        .project_id
        .clone()
        .unwrap_or_else(|| people::PEOPLE_PROJECT.to_owned());
    for user in members(state, &chat) {
        let _ = state.db.push_user(
            &user,
            "call_ended",
            &json!({
                "call_id": call.id, "chat_id": call.chat_id, "project_id": project_label,
                "duration_s": duration_s, "reason": reason, "writer_device": writer_device,
            }),
            None,
        );
        if !state.push.is_on() {
            continue;
        }
        for t in state.db.push_tokens_of_user(&user).unwrap_or_default() {
            let payload = json!({
                "aps": {
                    "alert": { "title": "Call ended", "body": format!("{} min", (duration_s + 30) / 60) },
                    "thread-id": call.chat_id,
                    "category": CATEGORY,
                    "content-available": 1,
                },
                "event": "ended",
                "chat_id": call.chat_id,
                "project_id": project_label,
                "call": call_json(state, call, None),
            });
            deliver_alert(state, &t, payload, format!("call:{}", call.id)).await;
        }
    }
}

/// End a call for `reason`; announces once, closes the LiveKit room. `writer_device` is the
/// device that should write the "Call ended" line when no client already knows it ended.
pub async fn end_call(
    state: &AppState,
    call_id: &str,
    reason: &str,
    writer_device: Option<String>,
) {
    if !state.db.end_call(call_id, reason).unwrap_or(false) {
        return;
    }
    let Ok(Some(call)) = state.db.call(call_id) else {
        return;
    };
    tracing::info!(call = %call.id, room = %call.room, reason, "call ended");
    let writer = match writer_device {
        Some(d) => Some(d),
        None => state.db.freshest_device_of(&call.started_by).ok().flatten(),
    };
    announce_ended(state, &call, reason, writer.as_deref()).await;
    if let Some(lk) = &state.livekit {
        lk.delete_room(&state.http, &call.room).await;
    }
}

// ---- handlers -------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct StartBody {
    #[serde(default)]
    pub kind: Option<String>,
}

async fn start_or_join(
    state: &AppState,
    principal: &Principal,
    chat: ChatRef,
    body: StartBody,
) -> Response {
    if state.livekit.is_none() {
        return calls_off();
    }
    let (_device, me) = match device_user(principal) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if !is_member(state, &chat, &me) {
        return api_error(StatusCode::FORBIDDEN, "not_member", "not in this chat");
    }
    let kind = match body.kind.as_deref().unwrap_or("voice") {
        "voice" => "voice",
        "video" => "video",
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "kind must be voice or video",
            )
        }
    };
    if let Ok(Some(existing)) = state.db.active_call_in_chat(&chat.chat_id) {
        let _ = state.db.call_join(&existing.id, &me);
        return Json(json!({ "call": call_json(state, &existing, Some(&me)), "joined": true }))
            .into_response();
    }
    let now = now_ms();
    let call = CallRow {
        id: ulid(),
        chat_id: chat.chat_id.clone(),
        project_id: chat.project_id.clone(),
        kind: kind.to_owned(),
        room: format!("call_{}", ulid()),
        started_by: me.clone(),
        started_at: now,
        ended_at: None,
        end_reason: None,
        last_activity_at: now,
    };
    if let Err(e) = state.db.insert_call(&call) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    let _ = state.db.call_join(&call.id, &me);
    tracing::info!(call = %call.id, chat = %call.chat_id, kind, by = %me, "call started");
    let starter = Sender::user(state, &me);
    announce_started(state, &call, &starter).await;
    (
        StatusCode::CREATED,
        Json(json!({ "call": call_json(state, &call, Some(&me)), "joined": false })),
    )
        .into_response()
}

/// `POST /chats/{id}/calls {"kind": "voice"|"video"}` — 201 new call, 200 joined the live one.
pub async fn start_people_call(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    body: Option<Json<StartBody>>,
) -> Response {
    if !state.allows(&principal, &format!("{}{id}", people::PEOPLE_ACL_PREFIX)) {
        return api_error(StatusCode::FORBIDDEN, "not_member", "not in this chat");
    }
    start_or_join(
        &state,
        &principal,
        ChatRef::people(&id),
        body.map(|b| b.0).unwrap_or_default(),
    )
    .await
}

/// `POST /projects/{project_id}/chats/{chat_id}/calls` — same for a shared project chat.
pub async fn start_project_call(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, chat_id)): Path<(String, String)>,
    body: Option<Json<StartBody>>,
) -> Response {
    let Some(project) = resolve_project_chat(&state, &principal, &project_id, &chat_id) else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such chat");
    };
    start_or_join(
        &state,
        &principal,
        ChatRef::project(&project, &chat_id),
        body.map(|b| b.0).unwrap_or_default(),
    )
    .await
}

/// The project's canonical id when the principal may use it and the chat exists.
fn resolve_project_chat(
    state: &AppState,
    principal: &Principal,
    project_id: &str,
    chat_id: &str,
) -> Option<String> {
    if !state.allows(principal, project_id) {
        return None;
    }
    let project = state.db.project(project_id).ok().flatten()?;
    state
        .db
        .chat_exists(&project.id, chat_id)
        .unwrap_or(false)
        .then_some(project.id)
}

fn active_call_response(state: &AppState, chat_id: &str, me: &str) -> Response {
    match state.db.active_call_in_chat(chat_id) {
        Ok(Some(call)) => {
            Json(json!({ "call": call_json(state, &call, Some(me)) })).into_response()
        }
        Ok(None) => api_error(StatusCode::NOT_FOUND, "no_call", "no call in this chat"),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `GET /chats/{id}/call` → the live call (with a join token) or 404.
pub async fn people_chat_call(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    if !state.allows(&principal, &format!("{}{id}", people::PEOPLE_ACL_PREFIX)) {
        return api_error(StatusCode::FORBIDDEN, "not_member", "not in this chat");
    }
    let (_, me) = match device_user(&principal) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    active_call_response(&state, &id, &me)
}

/// `GET /projects/{project_id}/chats/{chat_id}/call`
pub async fn project_chat_call(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, chat_id)): Path<(String, String)>,
) -> Response {
    if resolve_project_chat(&state, &principal, &project_id, &chat_id).is_none() {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such chat");
    }
    let (_, me) = match device_user(&principal) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    active_call_response(&state, &chat_id, &me)
}

/// The call when the caller is a member of its chat.
fn member_call(
    state: &AppState,
    principal: &Principal,
    call_id: &str,
) -> Result<(String, String, CallRow), Box<Response>> {
    let (device, me) = device_user(principal)?;
    let call = state
        .db
        .call(call_id)
        .map_err(|e| {
            Box::new(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "db",
                e.to_string(),
            ))
        })?
        .ok_or_else(|| {
            Box::new(api_error(
                StatusCode::NOT_FOUND,
                "not_found",
                "no such call",
            ))
        })?;
    if !is_member(state, &chat_ref_of(&call), &me) {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "not_member",
            "not in this call's chat",
        )));
    }
    Ok((device, me, call))
}

/// `GET /calls/{id}` → `{call}`; ended calls answer too (no token then).
pub async fn get_call(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    let (_, me, call) = match member_call(&state, &principal, &id) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    Json(json!({ "call": call_json(&state, &call, Some(&me)) })).into_response()
}

/// `POST /calls/{id}/join` → `{call}` with a fresh token; 410 when the call ended.
pub async fn join_call(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    if state.livekit.is_none() {
        return calls_off();
    }
    let (_, me, call) = match member_call(&state, &principal, &id) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if !call.active() {
        return api_error(StatusCode::GONE, "call_ended", "this call has ended");
    }
    let live = state
        .db
        .call_participants(&call.id)
        .unwrap_or_default()
        .iter()
        .filter(|p| p.left_at.is_none() && p.user_id != me)
        .count() as u32;
    if live >= crate::livekit::MAX_PARTICIPANTS {
        return api_error(StatusCode::CONFLICT, "call_full", "this call is full");
    }
    let _ = state.db.call_join(&call.id, &me);
    Json(json!({ "call": call_json(&state, &call, Some(&me)) })).into_response()
}

/// `POST /calls/{id}/leave` → `{ended, duration_s}`; the last one out ends the call.
pub async fn leave_call(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    let (device, me, call) = match member_call(&state, &principal, &id) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if !call.active() {
        let duration_s = (call.ended_at.unwrap_or(call.started_at) - call.started_at) / 1000;
        return Json(json!({ "ended": false, "already_ended": true, "duration_s": duration_s }))
            .into_response();
    }
    let remaining = state.db.call_leave(&call.id, &me).unwrap_or(1);
    if remaining > 0 {
        return Json(json!({ "ended": false, "duration_s": (now_ms() - call.started_at) / 1000 }))
            .into_response();
    }
    // This client knows the call ended (it gets `ended: true`), so nobody else writes the line.
    end_call(&state, &call.id, "last_left", Some(device)).await;
    let duration_s = state
        .db
        .call(&call.id)
        .ok()
        .flatten()
        .and_then(|c| c.ended_at)
        .map(|e| (e - call.started_at) / 1000)
        .unwrap_or(0);
    Json(json!({ "ended": true, "duration_s": duration_s })).into_response()
}

// ---- VoIP tokens ----------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct VoipBody {
    pub token: String,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub bundle_id: Option<String>,
}

/// `POST /devices/{id}/voip {token, environment, bundle_id}` → `{push}`
pub async fn register_voip(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<VoipBody>,
) -> Response {
    let (device, user_id) = match device_user(&principal) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if device != id {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a device registers only its own token",
        );
    }
    let token = b.token.trim().to_ascii_lowercase();
    if token.is_empty() || token.len() > 512 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "token must be the hex PushKit token",
        );
    }
    let environment = match b.environment.as_deref().unwrap_or("production") {
        "development" => "development",
        "production" => "production",
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "environment must be development or production",
            )
        }
    };
    let t = VoipToken {
        device_id: id,
        user_id,
        token,
        environment: environment.to_owned(),
        bundle_id: b
            .bundle_id
            .unwrap_or_else(|| crate::apns::DEFAULT_TOPIC.to_owned()),
        updated_at: now_ms(),
    };
    match state.db.upsert_voip_token(&t) {
        Ok(()) => {
            Json(json!({ "device_id": t.device_id, "push": state.push.is_on() })).into_response()
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `DELETE /devices/{id}/voip` → 204
pub async fn unregister_voip(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    let (device, _) = match device_user(&principal) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if device != id {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "not your device");
    }
    match state.db.delete_voip_token(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

// ---- LiveKit webhook ------------------------------------------------------------------------------

/// `POST /livekit/webhook` — LiveKit tells us who is really in the room and when it closed.
pub async fn livekit_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(lk) = &state.livekit else {
        return calls_off();
    };
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let Some(event) = lk.verify_webhook(auth, &body) else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "bad_signature",
            "webhook signature",
        );
    };
    handle_webhook(&state, &event).await;
    StatusCode::OK.into_response()
}

/// The verified event body. Unknown rooms (not ours) and unknown events are ignored.
pub async fn handle_webhook(state: &AppState, event: &Value) {
    let Some(room) = event["room"]["name"].as_str() else {
        return;
    };
    let Ok(Some(call)) = state.db.call_by_room(room) else {
        return;
    };
    let identity = event["participant"]["identity"]
        .as_str()
        .unwrap_or_default();
    match event["event"].as_str().unwrap_or_default() {
        "participant_joined" => {
            if let Some(u) = user_of_identity(identity) {
                let _ = state.db.call_join(&call.id, u);
            }
        }
        "participant_left" => {
            if let Some(u) = user_of_identity(identity) {
                let _ = state.db.call_leave(&call.id, u);
            }
        }
        "room_finished" => end_call(state, &call.id, "room_finished", None).await,
        _ => {
            let _ = state.db.touch_call(&call.id);
        }
    }
}

/// Every 15 s: end calls nobody is in any more (lost leaves or webhooks).
pub async fn spawn_sweeper(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            sweep(&state).await;
        }
    });
}

pub async fn sweep(state: &AppState) {
    let now = now_ms();
    for call in state.db.active_calls().unwrap_or_default() {
        let empty_since = state.db.call_empty_since(&call.id).ok().flatten();
        let abandoned = match empty_since {
            Some(at) => now - at > EMPTY_AFTER_MS,
            None => false,
        } || (now - call.last_activity_at > STALE_MS)
            || (state
                .db
                .call_participants(&call.id)
                .map(|p| p.is_empty())
                .unwrap_or(false)
                && now - call.started_at > EMPTY_START_MS);
        if abandoned {
            end_call(state, &call.id, "abandoned", None).await;
        }
    }
}

// ---- admin ------------------------------------------------------------------------------------------

/// `GET /admin/calls` → LiveKit status, live calls, recent ended calls, VoIP token count.
pub async fn admin_calls(State(state): State<AppState>, principal: Principal) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    let active: Vec<Value> = state
        .db
        .active_calls()
        .unwrap_or_default()
        .iter()
        .map(|c| call_json(&state, c, None))
        .collect();
    let recent: Vec<Value> = state
        .db
        .recent_calls(20)
        .unwrap_or_default()
        .iter()
        .map(|c| {
            json!({
                "call_id": c.id, "chat_id": c.chat_id, "kind": c.kind, "started_by": c.started_by,
                "started_at": c.started_at, "ended_at": c.ended_at, "reason": c.end_reason,
                "duration_s": c.ended_at.map(|e| (e - c.started_at) / 1000),
            })
        })
        .collect();
    Json(json!({
        "livekit": state.livekit.as_ref().map(|l| json!({ "url": l.url, "api_key": l.api_key })),
        "push": state.push.is_on(),
        "voip_tokens": state.db.all_voip_tokens().map(|t| t.len()).unwrap_or(0),
        "active": active,
        "recent": recent,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    //! Invariants: only members start/join/see a call; a second start joins the existing call;
    //! `room_finished` ends it exactly once with `call_ended` to every member; the last leave
    //! ends it and tells that client to write the line.

    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::Json;
    use base64::Engine;
    use sha2::Digest;

    use super::*;
    use crate::people::tests_support::{dm, person};
    use crate::state::test_support::test_state;
    use crate::state::AuthMode;

    async fn body_json(r: Response) -> Value {
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    fn with_livekit(state: &mut AppState) {
        state.livekit = Some(crate::livekit::LiveKit::new(
            "wss://lk.test",
            "APIkey",
            "secret123",
            None,
        ));
    }

    fn pushes_of(state: &AppState, p: &Principal) -> Vec<(String, Value)> {
        let Principal::Device { id, .. } = p else {
            unreachable!()
        };
        state
            .db
            .take_user_pushes(id)
            .unwrap()
            .into_iter()
            .map(|(_, kind, payload, _)| (kind, payload))
            .collect()
    }

    #[tokio::test]
    async fn members_only_and_join_existing() {
        let mut state = test_state(AuthMode::Github);
        with_livekit(&mut state);
        let (alice, alice_dev) = person(&state, 1);
        let (bob, bob_dev) = person(&state, 2);
        let (_carol, carol_dev) = person(&state, 3);
        let chat = dm(&state, &alice, &bob);

        // A stranger cannot start, see, or join.
        let r = start_people_call(
            State(state.clone()),
            carol_dev.clone(),
            Path(chat.clone()),
            Some(Json(StartBody {
                kind: Some("video".into()),
            })),
        )
        .await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);

        let r = start_people_call(
            State(state.clone()),
            alice_dev.clone(),
            Path(chat.clone()),
            Some(Json(StartBody {
                kind: Some("video".into()),
            })),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let v = body_json(r).await;
        let call_id = v["call"]["call_id"].as_str().unwrap().to_owned();
        assert_eq!(v["call"]["kind"], "video");
        assert_eq!(v["call"]["project_id"], "people");
        assert_eq!(v["call"]["livekit_url"], "wss://lk.test");
        assert_eq!(v["call"]["started_by"]["handle"], "user1");
        assert_eq!(v["call"]["participants"].as_array().unwrap().len(), 1);
        let claims = crate::livekit::claims_of(v["call"]["token"].as_str().unwrap()).unwrap();
        assert_eq!(claims["sub"], format!("u_{alice}"));
        assert_eq!(claims["video"]["room"], v["call"]["room"]);

        // Bob was told (not Alice); the push carries the call without a token.
        let bob_pushes = pushes_of(&state, &bob_dev);
        assert_eq!(bob_pushes.len(), 1);
        assert_eq!(bob_pushes[0].0, "call_started");
        assert_eq!(bob_pushes[0].1["call"]["call_id"], call_id);
        assert!(bob_pushes[0].1["call"]["token"].is_null());
        assert!(pushes_of(&state, &alice_dev).is_empty());

        // Bob starting "a call" in the same chat joins the live one.
        let r = start_people_call(
            State(state.clone()),
            bob_dev.clone(),
            Path(chat.clone()),
            None,
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let v = body_json(r).await;
        assert_eq!(v["call"]["call_id"], call_id);
        assert_eq!(v["joined"], true);
        assert_eq!(v["call"]["participants"].as_array().unwrap().len(), 2);
        let claims = crate::livekit::claims_of(v["call"]["token"].as_str().unwrap()).unwrap();
        assert_eq!(claims["sub"], format!("u_{bob}"));

        // Carol can neither join nor read it.
        let r = join_call(
            State(state.clone()),
            carol_dev.clone(),
            Path(call_id.clone()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = get_call(
            State(state.clone()),
            carol_dev.clone(),
            Path(call_id.clone()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = people_chat_call(State(state.clone()), carol_dev, Path(chat.clone())).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = people_chat_call(State(state.clone()), alice_dev.clone(), Path(chat.clone())).await;
        assert_eq!(r.status(), StatusCode::OK);

        // Alice leaves: Bob is still in, nothing ends. Bob leaves: it ends and he writes the line.
        let v = body_json(
            leave_call(
                State(state.clone()),
                alice_dev.clone(),
                Path(call_id.clone()),
            )
            .await,
        )
        .await;
        assert_eq!(v["ended"], false);
        assert!(state.db.call(&call_id).unwrap().unwrap().active());
        let v = body_json(
            leave_call(State(state.clone()), bob_dev.clone(), Path(call_id.clone())).await,
        )
        .await;
        assert_eq!(v["ended"], true);
        let call = state.db.call(&call_id).unwrap().unwrap();
        assert!(!call.active());
        assert_eq!(call.end_reason.as_deref(), Some("last_left"));
        let Principal::Device { id: bob_device, .. } = &bob_dev else {
            unreachable!()
        };
        for p in [&alice_dev, &bob_dev] {
            let ended: Vec<_> = pushes_of(&state, p)
                .into_iter()
                .filter(|(k, _)| k == "call_ended")
                .collect();
            assert_eq!(ended.len(), 1);
            assert_eq!(ended[0].1["writer_device"], *bob_device);
            assert_eq!(ended[0].1["reason"], "last_left");
        }
        let r = join_call(State(state.clone()), bob_dev.clone(), Path(call_id.clone())).await;
        assert_eq!(r.status(), StatusCode::GONE);
        let r = people_chat_call(State(state.clone()), alice_dev, Path(chat)).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn room_finished_ends_the_call_once() {
        let mut state = test_state(AuthMode::Github);
        with_livekit(&mut state);
        let (alice, alice_dev) = person(&state, 1);
        let (bob, bob_dev) = person(&state, 2);
        let chat = dm(&state, &alice, &bob);
        let v = body_json(
            start_people_call(
                State(state.clone()),
                alice_dev.clone(),
                Path(chat.clone()),
                None,
            )
            .await,
        )
        .await;
        let call = state
            .db
            .call(v["call"]["call_id"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let _ = pushes_of(&state, &bob_dev);

        // Webhooks: Bob joined, both left, the room closed.
        let ev = |event: &str, identity: &str| json!({ "event": event, "room": { "name": call.room }, "participant": { "identity": identity } });
        handle_webhook(&state, &ev("participant_joined", &format!("u_{bob}"))).await;
        assert_eq!(participants_json(&state, &call).len(), 2);
        handle_webhook(&state, &ev("participant_left", &format!("u_{alice}"))).await;
        handle_webhook(&state, &ev("participant_left", &format!("u_{bob}"))).await;
        assert!(
            state.db.call(&call.id).unwrap().unwrap().active(),
            "waits for room_finished"
        );
        let finished = json!({ "event": "room_finished", "room": { "name": call.room } });
        handle_webhook(&state, &finished).await;
        handle_webhook(&state, &finished).await;
        let after = state.db.call(&call.id).unwrap().unwrap();
        assert_eq!(after.end_reason.as_deref(), Some("room_finished"));
        for p in [&alice_dev, &bob_dev] {
            let ended: Vec<_> = pushes_of(&state, p)
                .into_iter()
                .filter(|(k, _)| k == "call_ended")
                .collect();
            assert_eq!(
                ended.len(),
                1,
                "announced once even when the webhook repeats"
            );
            assert!(
                ended[0].1["writer_device"].is_string(),
                "one device writes the line"
            );
        }

        // The signed HTTP form: a valid signature is accepted, a bad one is not.
        let lk = state.livekit.as_ref().unwrap();
        let body = serde_json::to_vec(
            &json!({ "event": "room_started", "room": { "name": "call_other" } }),
        )
        .unwrap();
        let digest = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(&body));
        let mut headers = HeaderMap::new();
        let token = {
            let claims = json!({ "iss": "APIkey", "sha256": digest, "exp": now_ms() / 1000 + 60 });
            let hdr = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(br#"{"alg":"HS256","typ":"JWT"}"#);
            let pl = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
            let input = format!("{hdr}.{pl}");
            let mut mac =
                <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(b"secret123").unwrap();
            hmac::Mac::update(&mut mac, input.as_bytes());
            let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(hmac::Mac::finalize(mac).into_bytes());
            format!("{input}.{sig}")
        };
        assert!(lk.verify_webhook(&token, &body).is_some());
        headers.insert(axum::http::header::AUTHORIZATION, token.parse().unwrap());
        let r = livekit_webhook(State(state.clone()), headers, body.clone().into()).await;
        assert_eq!(r.status(), StatusCode::OK);
        let mut bad = HeaderMap::new();
        bad.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer x.y.z".parse().unwrap(),
        );
        let r = livekit_webhook(State(state.clone()), bad, body.into()).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn online_member_looking_at_the_chat_still_gets_call_started() {
        let mut state = test_state(AuthMode::Github);
        with_livekit(&mut state);
        let (alice, alice_dev) = person(&state, 1);
        let (bob, bob_dev) = person(&state, 2);
        let chat = dm(&state, &alice, &bob);
        let Principal::Device { id: bob_device, .. } = &bob_dev else {
            unreachable!()
        };
        // Bob's app is open: fresh presence, focused on this very chat, device just seen.
        state.db.touch_device(bob_device).unwrap();
        notify::set_focus(&state, bob_device, Some(&chat)).await;
        let card = crate::people::with_presence(&state, json!({ "id": bob }));
        assert_eq!(card["online"], true);

        let r = start_people_call(State(state.clone()), alice_dev, Path(chat.clone()), None).await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let kinds: Vec<String> = pushes_of(&state, &bob_dev)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            kinds,
            vec!["call_started".to_owned()],
            "ringing is never suppressed by focus or presence"
        );
    }

    #[tokio::test]
    async fn calls_are_off_without_livekit() {
        let state = test_state(AuthMode::Github);
        let (alice, alice_dev) = person(&state, 1);
        let (bob, _) = person(&state, 2);
        let chat = dm(&state, &alice, &bob);
        let r = start_people_call(State(state.clone()), alice_dev, Path(chat), None).await;
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
