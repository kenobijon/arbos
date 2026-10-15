//! REST surface (crdt-sync §9): pairing, devices, projects, chats, box lifecycle, usage.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arbos_core::auth::{hash, mint, pairing_code, TokenKind};
use arbos_core::ids::{now_ms, ulid};
use arbos_core::wire::PROJECT_DOC_CHAT_ID;

use crate::auth::Principal;
use crate::boxes::{self, Provider};
use crate::db::{Chat, Project};
use crate::state::AppState;

type ApiResult = Result<Response, (StatusCode, String)>;

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn healthz(State(state): State<AppState>) -> Response {
    let devices = state.db.device_count().unwrap_or(0);
    Json(json!({ "ok": true, "proto": arbos_core::PROTO_VERSION, "devices": devices, "daytona": state.daytona.is_some(),
        "providers": { "openai_compatible": state.cfg.openrouter_key.is_some() || state.cfg.openai_key.is_some(), "anthropic": state.cfg.anthropic_key.is_some() || state.cfg.openrouter_key.is_some() } }))
    .into_response()
}

// ---- pairing (D35) ----------------------------------------------------------------------------

pub async fn pair_new(State(state): State<AppState>, principal: Principal) -> ApiResult {
    if !matches!(principal, Principal::Admin) {
        return Err((StatusCode::FORBIDDEN, "admin only".into()));
    }
    let code = pairing_code();
    state
        .db
        .new_pairing_code(&code, 10 * 60 * 1000)
        .map_err(internal)?;
    Ok(Json(json!({ "code": code, "expires_in_s": 600 })).into_response())
}

#[derive(Deserialize)]
pub struct PairBody {
    pub code: String,
    #[serde(default)]
    pub name: Option<String>,
}

pub async fn pair(State(state): State<AppState>, Json(b): Json<PairBody>) -> ApiResult {
    let token = mint(TokenKind::Device);
    let device_id = ulid();
    let ok = state
        .db
        .redeem_pairing_code(
            b.code.trim(),
            &device_id,
            b.name.as_deref().unwrap_or("device"),
            &hash(&token),
        )
        .map_err(internal)?;
    if !ok {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid or expired pairing code".into(),
        ));
    }
    Ok(Json(json!({ "device_id": device_id, "device_token": token })).into_response())
}

pub async fn list_devices(State(state): State<AppState>, principal: Principal) -> ApiResult {
    if principal.is_box() {
        return Err((
            StatusCode::FORBIDDEN,
            "box tokens cannot list devices".into(),
        ));
    }
    Ok(Json(state.db.list_devices().map_err(internal)?).into_response())
}

pub async fn revoke_device(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    if principal.is_box() {
        return Err((
            StatusCode::FORBIDDEN,
            "box tokens cannot revoke devices".into(),
        ));
    }
    let ok = state.db.revoke_device(&id).map_err(internal)?;
    Ok(if ok {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
    .into_response())
}

// ---- projects ---------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateProjectBody {
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub models: Option<Value>,
    /// `daytona` (default when a key exists), `process`, or `docker`.
    #[serde(default)]
    pub provider: Option<String>,
}

fn default_models() -> Value {
    json!({
        "talking_model": { "id": "anthropic/claude-haiku-5.5", "effort": "off" },
        "subagent_model": { "id": "anthropic/claude-sonnet-5.5", "effort": "medium" },
        "decision_model": { "id": "anthropic/claude-haiku-5.5", "effort": "off" }
    })
}

fn monogram(name: &str) -> String {
    let mut it = name.split_whitespace().filter_map(|w| w.chars().next());
    let a = it.next().unwrap_or('A');
    let b = it
        .next()
        .unwrap_or_else(|| name.chars().nth(1).unwrap_or('r'));
    format!("{}{}", a.to_ascii_uppercase(), b.to_ascii_lowercase())
}

pub async fn create_project(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<CreateProjectBody>,
) -> ApiResult {
    if principal.is_box() {
        return Err((
            StatusCode::FORBIDDEN,
            "box tokens cannot create projects".into(),
        ));
    }
    let name = b.name.trim().to_owned();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name required".into()));
    }
    let provider = match b.provider.as_deref() {
        Some(p) => Provider::parse(p).ok_or((
            StatusCode::BAD_REQUEST,
            "provider must be daytona|process|docker".to_owned(),
        ))?,
        None if state.daytona.is_some() => Provider::Daytona,
        None => Provider::Process,
    };
    let now = now_ms();
    let project = Project {
        id: ulid(),
        name: name.clone(),
        icon: b.icon.unwrap_or_else(|| monogram(&name)),
        instructions: b.instructions.unwrap_or_default(),
        models: b.models.unwrap_or_else(default_models),
        state: "waking".into(),
        created_at: now,
        last_active_at: now,
        spend_cap_daily: 25.0,
        color: b.color.clone().unwrap_or_else(|| "#8b7cf6".into()),
    };
    state
        .db
        .insert_project(&project)
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    let pinned = Chat {
        id: ulid(),
        project_id: project.id.clone(),
        title: name.clone(),
        is_pinned: true,
        position: 0,
        created_at: now,
        doc_epoch: 1,
    };
    state.db.insert_chat(&pinned).map_err(internal)?;
    let sb = boxes::create_box(&state, &project, provider)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("box create: {e}")))?;
    Ok(Json(json!({
        "project": project_json(&state, &project).await,
        "pinned_chat": chat_json(&pinned),
        "sandbox": { "provider": sb.provider, "sandbox_id": sb.sandbox_id, "device_id": sb.device_id, "box_url": sb.box_url },
        "rooms": { "project": format!("/room/{}/{}", project.id, PROJECT_DOC_CHAT_ID), "pinned_chat": format!("/room/{}/{}", project.id, pinned.id) }
    }))
    .into_response())
}

pub async fn list_projects(State(state): State<AppState>, _principal: Principal) -> ApiResult {
    let mut out = Vec::new();
    for p in state.db.list_projects().map_err(internal)? {
        out.push(project_json(&state, &p).await);
    }
    Ok(Json(out).into_response())
}

pub async fn get_project(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !principal.can_access_project(&p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let chats: Vec<Value> = state
        .db
        .list_chats(&p.id)
        .map_err(internal)?
        .iter()
        .map(chat_json)
        .collect();
    let (bs, since) = state.box_state(&p.id).await;
    let today = state
        .db
        .usage_since(&p.id, start_of_day())
        .map_err(internal)?;
    Ok(Json(json!({ "project": project_json(&state, &p).await, "chats": chats, "box": bs, "box_since": since, "spend_today": today })).into_response())
}

#[derive(Deserialize)]
pub struct PatchProjectBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    /// Merged slot by slot: `{ "talking_model": { "id": "...", "effort": "low" } }` leaves the other slots alone.
    #[serde(default)]
    pub models: Option<Value>,
    #[serde(default)]
    pub spend_cap_daily: Option<f64>,
}

/// `PATCH /projects/{id}`: settings writes (SetModel, rename, icon, instructions, cap). Devices and the
/// project's own box may call it; the box mirrors the result into the project doc `settings` map.
pub async fn patch_project(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<PatchProjectBody>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !principal.can_access_project(&p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let models = b.models.map(|patch| {
        let mut merged = p.models.clone();
        if let (Some(m), Some(o)) = (merged.as_object_mut(), patch.as_object()) {
            for (k, v) in o {
                m.insert(k.clone(), v.clone());
            }
        }
        merged
    });
    state
        .db
        .update_project(
            &p.id,
            b.name.as_deref(),
            b.icon.as_deref(),
            b.color.as_deref(),
            b.instructions.as_deref(),
            models.as_ref(),
            b.spend_cap_daily,
        )
        .map_err(internal)?;
    let p = state
        .db
        .project(&p.id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    Ok(Json(project_json(&state, &p).await).into_response())
}

#[derive(Deserialize)]
pub struct PatchChatBody {
    pub title: String,
}

/// `PATCH /projects/{id}/chats/{chat}`: retitle.
pub async fn patch_chat(
    State(state): State<AppState>,
    principal: Principal,
    Path((id, chat)): Path<(String, String)>,
    Json(b): Json<PatchChatBody>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !principal.can_access_project(&id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let ok = state
        .db
        .update_chat_title(&id, &chat, b.title.trim())
        .map_err(internal)?;
    Ok(if ok {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
    .into_response())
}

/// `GET /projects/{id}/history/{chat}?before=&limit=`: older T3 turns of a chat, proxied from the box
/// (`crdt-sync.md` §6.4). Needs the box awake; 409 otherwise.
pub async fn chat_history(
    State(state): State<AppState>,
    principal: Principal,
    Path((id, chat)): Path<(String, String)>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !principal.can_access_project(&id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let sb = state
        .db
        .sandbox(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no sandbox".to_owned()))?;
    let Some(url) = sb.box_url else {
        return Err((StatusCode::CONFLICT, "box has no url yet".into()));
    };
    let mut req = state.http.get(format!("{url}/history/{chat}")).query(&q);
    if let Some(t) = &sb.preview_token {
        req = req.header("x-daytona-preview-token", t);
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => Ok((
            [("content-type", "application/json")],
            r.bytes().await.unwrap_or_default(),
        )
            .into_response()),
        Ok(r) => Err((
            StatusCode::BAD_GATEWAY,
            format!("box answered {}", r.status()),
        )),
        Err(e) => Err((
            StatusCode::CONFLICT,
            format!("box unreachable (asleep?): {e}"),
        )),
    }
}

/// Accept a project id or name in the path and return the id.
fn resolve_project_id(state: &AppState, id_or_name: &str) -> Result<String, (StatusCode, String)> {
    match state.db.project(id_or_name) {
        Ok(Some(p)) => Ok(p.id),
        Ok(None) => Err((StatusCode::NOT_FOUND, "no such project".into())),
        Err(e) => Err(internal(e)),
    }
}

/// The project record as clients see it (additive; `arbos-mac` maps it onto its `Project`).
async fn project_json(state: &AppState, p: &Project) -> Value {
    let (bs, since) = state.box_state(&p.id).await;
    let today = state
        .db
        .usage_since(&p.id, start_of_day())
        .map(|u| u.usd)
        .unwrap_or(0.0);
    json!({
        "id": p.id, "name": p.name, "icon": p.icon, "color": p.color, "instructions": p.instructions,
        "state": p.state, "box": bs, "box_since": since,
        "models": p.models,
        "spend_today_usd": today, "spend_cap_usd": p.spend_cap_daily,
        "created_at": p.created_at, "last_active_at": p.last_active_at,
    })
}

fn chat_json(c: &Chat) -> Value {
    json!({ "id": c.id, "project_id": c.project_id, "title": c.title, "is_pinned": c.is_pinned, "is_system": false,
            "position": c.position, "created_at": c.created_at, "doc_epoch": c.doc_epoch, "room": format!("/room/{}/{}", c.project_id, c.id) })
}

fn start_of_day() -> i64 {
    let now = now_ms();
    now - now.rem_euclid(86_400_000)
}

pub async fn project_status(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !principal.can_access_project(&p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let (bs, since) = state.box_state(&p.id).await;
    let provider = boxes::provider_status(&state, &p.id).await;
    let timings = state.db.timings(&p.id).map_err(internal)?;
    Ok(Json(json!({ "project_id": p.id, "state": p.state, "box": bs, "box_since": since, "provider": provider, "timings": timings })).into_response())
}

#[derive(Deserialize)]
pub struct CreateChatBody {
    #[serde(default)]
    pub title: Option<String>,
}

pub async fn create_chat(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<CreateChatBody>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !principal.can_access_project(&p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let n = state.db.list_chats(&p.id).map_err(internal)?.len() as i64;
    let chat = Chat {
        id: ulid(),
        project_id: p.id.clone(),
        title: b.title.unwrap_or_else(|| "New chat".into()),
        is_pinned: false,
        position: n,
        created_at: now_ms(),
        doc_epoch: 1,
    };
    state.db.insert_chat(&chat).map_err(internal)?;
    Ok(
        Json(json!({ "chat": chat, "room": format!("/room/{}/{}", p.id, chat.id) }))
            .into_response(),
    )
}

pub async fn list_chats(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !principal.can_access_project(&p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    Ok(Json(
        state
            .db
            .list_chats(&p.id)
            .map_err(internal)?
            .iter()
            .map(chat_json)
            .collect::<Vec<_>>(),
    )
    .into_response())
}

// ---- box lifecycle ----------------------------------------------------------------------------

pub async fn wake(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !principal.can_access_project(&p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    boxes::ensure_awake(&state, &p.id, "api").await;
    let (bs, since) = state.box_state(&p.id).await;
    Ok(Json(json!({ "box": bs, "since": since })).into_response())
}

pub async fn start(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !principal.can_access_project(&p.id) || principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    boxes::start_box(&state, &p.id)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let (bs, since) = state.box_state(&p.id).await;
    Ok(Json(json!({ "box": bs, "since": since })).into_response())
}

#[derive(Deserialize)]
pub struct StopQuery {
    #[serde(default)]
    pub force: bool,
}

pub async fn stop(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Query(q): Query<StopQuery>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !principal.can_access_project(&p.id) || principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    boxes::stop_box(&state, &p.id, q.force)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let (bs, since) = state.box_state(&p.id).await;
    Ok(Json(json!({ "box": bs, "since": since })).into_response())
}

#[derive(Deserialize)]
pub struct KeepaliveBody {
    #[serde(default)]
    pub turn_running: bool,
}

pub async fn keepalive(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<KeepaliveBody>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !principal.can_access_project(&id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    boxes::keepalive(&state, &id, b.turn_running)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct BoxHelloBody {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub proto: Option<u32>,
}

/// The box announces itself after boot; the relay returns its settings and the queued pushes.
pub async fn box_hello(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<BoxHelloBody>,
) -> ApiResult {
    let Principal::Box { project_id } = &principal else {
        return Err((StatusCode::FORBIDDEN, "box only".into()));
    };
    if project_id != &id {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    tracing::info!(project = %id, version = ?b.version, "box hello");
    if b.proto.unwrap_or(arbos_core::PROTO_VERSION) != arbos_core::PROTO_VERSION {
        return Err((
            StatusCode::CONFLICT,
            "incompatible agentd protocol; rebuild box (D44)".into(),
        ));
    }
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if let Some(url) = &b.url {
        if let Ok(Some(mut sb)) = state.db.sandbox(&id) {
            if sb.provider != "daytona" {
                sb.box_url = Some(url.clone());
                let _ = state.db.upsert_sandbox(&sb);
            }
        }
    }
    let chats = state.db.list_chats(&id).map_err(internal)?;
    let pushes = state.db.take_pushes(&id).map_err(internal)?;
    Ok(Json(json!({
        "project": p,
        "chats": chats,
        "pushes": pushes.into_iter().map(|(i, k, v)| json!({ "id": i, "kind": k, "payload": v })).collect::<Vec<_>>(),
        "relay_version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response())
}

#[derive(Deserialize)]
pub struct PushBody {
    pub kind: String,
    pub payload: Value,
}

/// Queue a push for a project (box→relay "turn finished", cross-project messages, notices).
pub async fn push(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<PushBody>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    let _ = principal;
    state
        .db
        .enqueue_push(&p.id, &b.kind, &b.payload)
        .map_err(internal)?;
    if b.kind == "message" {
        boxes::ensure_awake(&state, &p.id, "cross-project").await;
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

pub async fn take_pushes(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !principal.can_access_project(&id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let pushes = state.db.take_pushes(&id).map_err(internal)?;
    Ok(Json(
        pushes
            .into_iter()
            .map(|(i, k, v)| json!({ "id": i, "kind": k, "payload": v }))
            .collect::<Vec<_>>(),
    )
    .into_response())
}

pub async fn usage(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !principal.can_access_project(&id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let today = state
        .db
        .usage_since(&id, start_of_day())
        .map_err(internal)?;
    let all = state.db.usage_since(&id, 0).map_err(internal)?;
    Ok(Json(json!({ "today": today, "all": all })).into_response())
}

#[derive(Deserialize)]
pub struct TimingBody {
    pub kind: String,
    pub ms: i64,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Boxes and the headless client record timings (TTFT, boot, wake) in one table for the M0 checklist.
pub async fn record_timing(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<TimingBody>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !principal.can_access_project(&id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    state
        .db
        .record_timing(&id, &b.kind, b.ms, b.detail.as_deref())
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
