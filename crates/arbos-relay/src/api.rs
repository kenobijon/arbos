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
use crate::state::{AppState, AuthMode};

pub(crate) type ApiResult = Result<Response, (StatusCode, String)>;

pub(crate) fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub async fn healthz(State(state): State<AppState>) -> Response {
    let devices = state.db.device_count().unwrap_or(0);
    Json(json!({ "ok": true, "proto": arbos_core::PROTO_VERSION, "devices": devices, "daytona": state.daytona.is_some(),
        "auth_mode": state.cfg.auth_mode.as_str(),
        "providers": { "openai_compatible": state.cfg.openrouter_key.is_some() || state.cfg.openai_key.is_some(), "anthropic": state.cfg.anthropic_key.is_some() || state.cfg.openrouter_key.is_some() } }))
    .into_response()
}

// ---- pairing (D35) ----------------------------------------------------------------------------

/// In GitHub mode the pairing-code path is closed (self-hosters keep it with
/// `ARBOS_AUTH_MODE=paircode`).
fn pair_enabled(state: &AppState) -> Result<(), (StatusCode, String)> {
    match state.cfg.auth_mode {
        AuthMode::Paircode => Ok(()),
        AuthMode::Github => Err((
            StatusCode::GONE,
            "pairing codes are off on this relay; sign in with GitHub".to_owned(),
        )),
    }
}

pub async fn pair_new(State(state): State<AppState>, principal: Principal) -> ApiResult {
    pair_enabled(&state)?;
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
    pair_enabled(&state)?;
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
    /// Tree avatar slug; default = the deterministic pick for the new id.
    #[serde(default)]
    pub avatar: Option<String>,
    /// Avatar colour slug; default = the deterministic pick, deduped with the tree.
    #[serde(default)]
    pub avatar_color: Option<String>,
    /// Short public blurb for the agent's profile (≤ 500 chars).
    #[serde(default)]
    pub description: Option<String>,
}

/// Profile blurbs are short and single-paragraph.
fn clean_description(d: &str) -> Result<String, (StatusCode, String)> {
    let d = d.trim();
    if d.chars().count() > 500 {
        return Err((
            StatusCode::BAD_REQUEST,
            "description must be 500 characters or fewer".into(),
        ));
    }
    Ok(d.to_owned())
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
    let id = ulid();
    let owner = principal.user_id().unwrap_or("u_default").to_owned();
    // Defaults are deduped within the user's (tree, colour) pairs so sibling tiles differ.
    let taken: Vec<(String, String)> = state
        .db
        .projects_of_user(&owner)
        .unwrap_or_default()
        .iter()
        .map(|p| {
            (
                p.avatar
                    .clone()
                    .unwrap_or_else(|| crate::avatar::default_for(&p.id, &state.cfg.avatar_slugs)),
                p.avatar_color.clone().unwrap_or_else(|| {
                    crate::avatar::default_color_for(&p.id, &state.cfg.avatar_colors)
                }),
            )
        })
        .collect();
    let (default_tree, default_color) = crate::avatar::default_pair(
        &id,
        &state.cfg.avatar_slugs,
        &state.cfg.avatar_colors,
        &taken,
    );
    let avatar = match b.avatar.as_deref().map(str::trim) {
        Some(a) if crate::avatar::valid_slug(a) => a.to_owned(),
        Some(_) => return Err((StatusCode::BAD_REQUEST, "avatar must be a slug".into())),
        None => default_tree,
    };
    let avatar_color = match b.avatar_color.as_deref().map(str::trim) {
        Some(c) if crate::avatar::valid_slug(c) => c.to_owned(),
        Some(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "avatar_color must be a slug".into(),
            ))
        }
        None => default_color,
    };
    let project = Project {
        id,
        name: name.clone(),
        icon: b.icon.unwrap_or_else(|| monogram(&name)),
        instructions: b.instructions.unwrap_or_default(),
        models: b.models.unwrap_or_else(default_models),
        state: "hibernated_light".into(),
        created_at: now,
        last_active_at: now,
        spend_cap_daily: 25.0,
        color: b.color.clone().unwrap_or_else(|| "#8b7cf6".into()),
        user_id: owner,
        workspace_state: "stopped".into(),
        avatar: Some(avatar),
        avatar_color: Some(avatar_color),
        description: clean_description(b.description.as_deref().unwrap_or_default())?,
    };
    if state
        .db
        .project_of_user_by_name(&project.user_id, &project.name)
        .map_err(internal)?
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            format!("you already have a project named \"{}\"", project.name),
        ));
    }
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
        kind: "chat".into(),
        participants: Vec::new(),
    };
    state.db.insert_chat(&pinned).map_err(internal)?;
    // One box per user, created lazily: the first message (room send → ensure_awake → wake job
    // → start_box) creates and starts it, so a project that is never used costs nothing. An
    // existing box is woken now so the new workspace is ready when the user types.
    let sb = state.db.user_box(&project.user_id).map_err(internal)?;
    if sb.is_some() {
        boxes::ensure_awake(&state, &project.user_id, "api").await;
    }
    Ok(Json(json!({
        "project": project_json(&state, &project).await,
        "pinned_chat": chat_json(&pinned),
        "sandbox": {
            "provider": sb.as_ref().map(|s| s.provider.clone()).unwrap_or_else(|| provider.as_str().to_owned()),
            "sandbox_id": sb.as_ref().and_then(|s| s.sandbox_id.clone()),
            "device_id": sb.as_ref().map(|s| s.device_id.clone()),
            "box_url": sb.as_ref().and_then(|s| s.box_url.clone()),
        },
        "rooms": { "project": format!("/room/{}/{}", project.id, PROJECT_DOC_CHAT_ID), "pinned_chat": format!("/room/{}/{}", project.id, pinned.id) }
    }))
    .into_response())
}

/// `GET /projects`: the caller's own projects, then the ones shared with them (`shared: true`,
/// with the owner's handle). The admin sees every project.
pub async fn list_projects(State(state): State<AppState>, principal: Principal) -> ApiResult {
    let mut out = Vec::new();
    match &principal {
        Principal::Admin => {
            for p in state.db.list_projects().map_err(internal)? {
                out.push(project_json(&state, &p).await);
            }
        }
        Principal::Device { user_id, .. } | Principal::Box { user_id } => {
            for p in state.db.projects_of_user(user_id).map_err(internal)? {
                let mut v = project_json(&state, &p).await;
                v["shared"] = json!(false);
                v["owner"] = json!(p.user_id);
                v["my_role"] = json!("owner");
                out.push(v);
            }
            if matches!(principal, Principal::Device { .. }) {
                for pid in state.db.shared_project_ids(user_id).map_err(internal)? {
                    if let Ok(Some(p)) = state.db.project(&pid) {
                        let mut v = project_json(&state, &p).await;
                        v["shared"] = json!(true);
                        v["owner"] = json!(p.user_id);
                        v["owner_handle"] = state
                            .db
                            .user_card(&p.user_id)
                            .ok()
                            .flatten()
                            .map(|c| c["handle"].clone())
                            .unwrap_or(Value::Null);
                        v["my_role"] = json!("member");
                        out.push(v);
                    }
                }
            }
        }
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
    if !state.allows(&principal, &p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let chats: Vec<Value> = state
        .db
        .list_chats(&p.id)
        .map_err(internal)?
        .iter()
        .map(chat_json)
        .collect();
    let (bs, since) = state.box_state(&p.user_id).await;
    let today = state
        .db
        .usage_since(&p.id, start_of_day())
        .map_err(internal)?;
    // The box polls this every 3 s; a changed marker tells it to refetch `/skills/index` (F5).
    let skills_rev = state.db.skills_rev(Some(&p.id)).map_err(internal)?;
    Ok(Json(json!({ "project": project_json(&state, &p).await, "chats": chats, "box": bs, "box_since": since, "spend_today": today, "skills_rev": skills_rev })).into_response())
}

/// A model id the proxy can serve: `vendor/name`, no spaces. Picker labels ("Claude Fable 5.1")
/// are refused here so they never reach a slot.
fn plausible_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 120
        && id.contains('/')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '.' | '_' | ':'))
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
    /// Tree avatar slug; `null` resets to the deterministic default.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub avatar: Option<Option<String>>,
    /// Avatar colour slug; `null` resets to the deterministic default.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub avatar_color: Option<Option<String>>,
    /// Profile blurb (≤ 500 chars; empty string clears it).
    #[serde(default)]
    pub description: Option<String>,
}

fn deserialize_double_option<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(d).map(Some)
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
    if !state.owns(&principal, &p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    if let Some(o) = b.models.as_ref().and_then(Value::as_object) {
        for (slot, v) in o {
            if !matches!(
                slot.as_str(),
                "talking_model" | "subagent_model" | "decision_model"
            ) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("unknown model slot {slot}"),
                ));
            }
            let id = v.get("id").and_then(Value::as_str).unwrap_or("");
            if !plausible_model_id(id) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("{slot}.id must be a provider model id like vendor/name, got {id:?}"),
                ));
            }
        }
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
    let avatar_patch: Option<Option<String>> = match &b.avatar {
        None => None,
        Some(None) => Some(Some(crate::avatar::default_for(
            &p.id,
            &state.cfg.avatar_slugs,
        ))),
        Some(Some(a)) => {
            let a = a.trim();
            if !crate::avatar::valid_slug(a) {
                return Err((StatusCode::BAD_REQUEST, "avatar must be a slug".into()));
            }
            Some(Some(a.to_owned()))
        }
    };
    let color_patch: Option<Option<String>> = match &b.avatar_color {
        None => None,
        Some(None) => Some(Some(crate::avatar::default_color_for(
            &p.id,
            &state.cfg.avatar_colors,
        ))),
        Some(Some(c)) => {
            let c = c.trim();
            if !crate::avatar::valid_slug(c) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "avatar_color must be a slug".into(),
                ));
            }
            Some(Some(c.to_owned()))
        }
    };
    let description = match &b.description {
        Some(d) => Some(clean_description(d)?),
        None => None,
    };
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
            avatar_patch.as_ref().map(|a| a.as_deref()),
            color_patch.as_ref().map(|a| a.as_deref()),
            description.as_deref(),
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
    if !state.allows(&principal, &id) {
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

/// `DELETE /projects/{id}/chats/{chat}`: close a tab. Rows and history stay (D55); the pinned chat refuses (409).
pub async fn delete_chat(
    State(state): State<AppState>,
    principal: Principal,
    Path((id, chat)): Path<(String, String)>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !state.allows(&principal, &id) || principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    match state.db.close_chat(&id, &chat).map_err(internal)? {
        None => Ok(StatusCode::NOT_FOUND.into_response()),
        Some(false) => Err((
            StatusCode::CONFLICT,
            "the pinned chat cannot be closed".into(),
        )),
        Some(true) => Ok(StatusCode::NO_CONTENT.into_response()),
    }
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
    if !state.allows(&principal, &id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let sb = state
        .db
        .box_for_project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no box".to_owned()))?;
    let Some(url) = sb.box_url else {
        return Err((StatusCode::CONFLICT, "box has no url yet".into()));
    };
    // One box per user: the workspace is named in the path.
    let mut req = state
        .http
        .get(format!("{url}/w/{id}/history/{chat}"))
        .query(&q);
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
pub(crate) fn resolve_project_id(
    state: &AppState,
    id_or_name: &str,
) -> Result<String, (StatusCode, String)> {
    match state.db.project(id_or_name) {
        Ok(Some(p)) => Ok(p.id),
        Ok(None) => Err((StatusCode::NOT_FOUND, "no such project".into())),
        Err(e) => Err(internal(e)),
    }
}

/// The project record as clients see it (additive; `arbos-mac` maps it onto its `Project`).
pub(crate) async fn project_json(state: &AppState, p: &Project) -> Value {
    let (bs, since) = state.box_state(&p.user_id).await;
    let today = state
        .db
        .usage_since(&p.id, start_of_day())
        .map(|u| u.usd)
        .unwrap_or(0.0);
    json!({
        "id": p.id, "name": p.name, "icon": p.icon, "color": p.color, "instructions": p.instructions,
        "avatar": p.avatar.clone().unwrap_or_else(|| crate::avatar::default_for(&p.id, &state.cfg.avatar_slugs)),
        "avatar_color": p.avatar_color.clone().unwrap_or_else(|| crate::avatar::default_color_for(&p.id, &state.cfg.avatar_colors)),
        "state": p.state, "box": bs, "box_since": since,
        "models": p.models,
        "spend_today_usd": today, "spend_cap_usd": p.spend_cap_daily,
        "created_at": p.created_at, "last_active_at": p.last_active_at,
        "description": p.description,
        "owner": crate::people::with_presence(state, crate::notify::Sender::user(state, &p.user_id).json()),
        "model": p.models.get("talking_model").and_then(|m| m.get("id")).cloned().unwrap_or(Value::Null),
        "machine": machine_profile(state, &p.user_id, bs, since),
        "activity": activity_json(state, &p.id).await,
    })
}

/// The agent's machine as a profile line: state, since when, and what kind of machine.
fn machine_profile(
    state: &AppState,
    user_id: &str,
    bs: arbos_core::wire::BoxState,
    since: i64,
) -> Value {
    let sb = state.db.user_box(user_id).ok().flatten();
    let class = match sb.as_ref().map(|s| s.provider.as_str()) {
        Some("daytona") => "daytona",
        Some("docker") => "docker",
        Some(_) => "process",
        None => "none",
    };
    json!({
        "state": bs,
        "since": since,
        "class": class,
        "snapshot": sb.as_ref().and_then(|s| s.snapshot.clone()),
        "image_outdated": sb.as_ref().is_some_and(|s| crate::upgrade::image_outdated(state, s)),
        "always_on": state.cfg.always_on,
    })
}

pub(crate) fn chat_json(c: &Chat) -> Value {
    json!({ "id": c.id, "project_id": c.project_id, "title": c.title, "is_pinned": c.is_pinned, "is_system": c.kind == "projects",
            "kind": c.kind, "participants": c.participants,
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
    if !state.allows(&principal, &p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let (bs, since) = state.box_state(&p.user_id).await;
    let mut provider = boxes::provider_status(&state, &p.user_id).await;
    // Laptops cannot reach a Daytona preview URL without its token: hand them the relay's proxy instead.
    if let Some(tok) = principal.raw_token() {
        let ws = state
            .cfg
            .public_url
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        provider["browser_stream_url"] =
            json!(format!("{ws}/projects/{}/browser/stream?token={tok}", p.id));
    }
    let mut timings = state.db.timings(&p.id).map_err(internal)?;
    timings.extend(
        state
            .db
            .timings(&boxes::timing_key(&p.user_id))
            .map_err(internal)?,
    );
    Ok(Json(json!({ "project_id": p.id, "state": p.state, "box": bs, "box_since": since, "provider": provider, "timings": timings })).into_response())
}

/// `GET /machine`: the caller's box (one per user), its class, spend, and the workspace state of
/// every project on it. Field names are the contract for the Mac and iOS apps.
pub async fn machine(State(state): State<AppState>, principal: Principal) -> ApiResult {
    let user_id = principal
        .user_id()
        .ok_or((
            StatusCode::FORBIDDEN,
            "device or box token required".to_owned(),
        ))?
        .to_owned();
    let (bs, since) = state.box_state(&user_id).await;
    let sb = state.db.user_box(&user_id).map_err(internal)?;
    let snapshot_name = sb
        .as_ref()
        .and_then(|s| s.snapshot.clone())
        .or(state.db.user_snapshot(&user_id).map_err(internal)?)
        .unwrap_or_else(|| state.cfg.daytona_snapshot.clone());
    // Class from the snapshot when Daytona answers; the known default otherwise.
    let (mut cpu, mut mem, mut disk) = (4_i64, 8_i64, 10_i64);
    if let Some(dt) = &state.daytona {
        if let Ok(snap) = dt.get_snapshot(&snapshot_name).await {
            cpu = snap.cpu.unwrap_or(cpu);
            mem = snap.mem.unwrap_or(mem);
            disk = snap.disk.unwrap_or(disk);
        }
    }
    let projects = state.db.projects_of_user(&user_id).map_err(internal)?;
    let workspaces: Vec<Value> = projects
        .iter()
        .map(|p| {
            json!({
                "project_id": p.id,
                "state": if bs == arbos_core::wire::BoxState::Awake { p.workspace_state.as_str() } else { "stopped" },
            })
        })
        .collect();
    let spend_today = state
        .db
        .user_usage_since(&user_id, start_of_day())
        .map_err(internal)?;
    Ok(Json(json!({
        "user_id": user_id,
        "state": bs,
        "since": since,
        "provider": sb.as_ref().map(|s| s.provider.clone()),
        "sandbox_id": sb.as_ref().and_then(|s| s.sandbox_id.clone()),
        "spec": { "cpu": cpu, "mem_gib": mem, "disk_gib": disk, "class": snapshot_name },
        "spend_today_usd": spend_today,
        "spend_cap_usd": state.db.user_spend_cap(&user_id).map_err(internal)?,
        "workspaces": workspaces,
        "migrated_from": sb.as_ref().map(|s| s.migrated_from.clone()).unwrap_or_default(),
        // Image: the snapshot this box was made from vs the relay's current default. An outdated
        // box lacks newer agent features until `POST /machine/upgrade` recreates it.
        "image": {
            "current": sb.as_ref().and_then(|s| s.snapshot.clone()),
            "default": state.cfg.daytona_snapshot,
        },
        "image_outdated": sb.as_ref().is_some_and(|s| crate::upgrade::image_outdated(&state, s)),
        "upgrade": crate::upgrade::status_json(&state, &user_id).await,
        // Always-on: the machine runs all the time (no idle stop, no waking to show); `held`
        // is a manual Stop that the next Start or message lifts.
        "always_on": state.cfg.always_on,
        "held": state.db.box_held(&user_id).unwrap_or(false),
        "machine_today_usd": state.db.credits(&user_id, start_of_day()).map(|c| c.machine_today_usd).unwrap_or(0.0),
    }))
    .into_response())
}

/// `POST /machine/start`: wake the caller's box (creating it if the user has none yet).
pub async fn machine_start(State(state): State<AppState>, principal: Principal) -> ApiResult {
    let user_id = principal
        .user_id()
        .ok_or((StatusCode::FORBIDDEN, "device token required".to_owned()))?
        .to_owned();
    if principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "devices only".into()));
    }
    if let Err(r) = crate::credits::check_read_only(&state) {
        return Ok(crate::credits::refused_response(&r));
    }
    let _ = state.db.set_box_held(&user_id, false);
    if state.db.user_box(&user_id).map_err(internal)?.is_none() {
        let provider = if state.daytona.is_some() {
            Provider::Daytona
        } else {
            Provider::Process
        };
        boxes::ensure_user_box(&state, &user_id, provider)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, format!("box create: {e}")))?;
    }
    boxes::ensure_awake(&state, &user_id, "api").await;
    let (bs, since) = state.box_state(&user_id).await;
    Ok(Json(json!({ "state": bs, "since": since })).into_response())
}

/// `POST /machine/stop`: hibernate the caller's box; every workspace stops with it.
pub async fn machine_stop(
    State(state): State<AppState>,
    principal: Principal,
    Query(q): Query<StopQuery>,
) -> ApiResult {
    let user_id = principal
        .user_id()
        .ok_or((StatusCode::FORBIDDEN, "device token required".to_owned()))?
        .to_owned();
    if principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "devices only".into()));
    }
    // A manual Stop holds the box down until the user starts it (or sends a message): the
    // always-on sweep leaves held boxes alone.
    let _ = state.db.set_box_held(&user_id, true);
    boxes::stop_box(&state, &user_id, q.force)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let (bs, since) = state.box_state(&user_id).await;
    Ok(Json(json!({ "state": bs, "since": since, "held": true })).into_response())
}

/// `DELETE /projects/{id}` — owner only. Soft delete: the project leaves every list and the box
/// drops the workspace on its next poll; the relay then moves `/work/<id>` to `/work/.archive/`
/// on the box. Rows, chats, rooms, blobs and the archived folder are purged after seven days.
pub async fn delete_project(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let p = state
        .db
        .project(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    if !state.owns(&principal, &p.id) || principal.is_box() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the owner can delete a project".into(),
        ));
    }
    if !state.db.soft_delete_project(&p.id).map_err(internal)? {
        return Err((StatusCode::NOT_FOUND, "no such project".to_owned()));
    }
    let _ = state.db.record_audit(
        "relay-admin",
        "project_deleted",
        &json!({ "ts": now_ms(), "project_id": p.id, "user_id": p.user_id, "by": principal.device_name() }),
    );
    // Archive the workspace folder on the box (best effort; the purge removes it later).
    let state2 = state.clone();
    let pid = p.id.clone();
    let owner = p.user_id.clone();
    tokio::spawn(async move {
        boxes::archive_workspace_dir(&state2, &owner, &pid).await;
    });
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `DELETE /admin/users/{id}/box` — stop and delete the user's sandbox and clear the row. A
/// signed-in (non-test) user gets a fresh box from the always-on sweep within a minute; test
/// users stay without one, so they can be deleted.
pub async fn admin_delete_user_box(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    if !matches!(principal, Principal::Admin) {
        return Err((StatusCode::FORBIDDEN, "admin only".into()));
    }
    let Some(sb) = state.db.user_box(&id).map_err(internal)? else {
        return Err((StatusCode::NOT_FOUND, "no box for that user".into()));
    };
    let mut deleted_sandbox = None;
    if let (Some(dt), Some(sid)) = (&state.daytona, &sb.sandbox_id) {
        if sb.provider == "daytona" {
            let _ = dt.stop(sid, true).await;
            match dt.delete(sid).await {
                Ok(_) => deleted_sandbox = Some(sid.clone()),
                Err(e) if e.to_string().contains("404") => deleted_sandbox = Some(sid.clone()),
                Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("delete sandbox: {e}"))),
            }
        }
    }
    if sb.provider != "daytona" {
        let _ = boxes::stop_box(&state, &id, true).await;
    }
    state.db.delete_box_row(&id).map_err(internal)?;
    state.box_states.write().await.remove(&id);
    state.box_conns.lock().await.remove(&id);
    let _ = state.db.record_audit(
        "relay-admin",
        "box_deleted",
        &json!({ "ts": now_ms(), "user_id": id, "sandbox": deleted_sandbox }),
    );
    Ok(Json(json!({ "user_id": id, "deleted_sandbox": deleted_sandbox })).into_response())
}

/// `POST /machine/restart`: stop and start the caller's box in one call (no `held`; the
/// always-on sweep would re-wake it anyway). 202 with the state right after the start was kicked.
pub async fn machine_restart(State(state): State<AppState>, principal: Principal) -> ApiResult {
    let user_id = principal
        .user_id()
        .ok_or((StatusCode::FORBIDDEN, "device token required".to_owned()))?
        .to_owned();
    if principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "devices only".into()));
    }
    if let Err(r) = crate::credits::check_read_only(&state) {
        return Ok(crate::credits::refused_response(&r));
    }
    let _ = state.db.set_box_held(&user_id, false);
    if state.db.user_box(&user_id).map_err(internal)?.is_some() {
        boxes::stop_box(&state, &user_id, true)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    }
    state
        .set_box_state(
            &user_id,
            arbos_core::wire::BoxState::Asleep,
            Some("restarting"),
        )
        .await;
    boxes::ensure_awake(&state, &user_id, "start").await;
    let (bs, since) = state.box_state(&user_id).await;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "state": bs, "since": since, "held": false })),
    )
        .into_response())
}

/// `POST /machine/migrate`: copy every legacy per-project sandbox of the caller's user into the
/// user box (D70). Idempotent; old sandboxes are stopped and marked, never deleted.
pub async fn machine_migrate(
    State(state): State<AppState>,
    principal: Principal,
    Query(q): Query<MigrateQuery>,
) -> ApiResult {
    let user_id = match &principal {
        Principal::Device { user_id, .. } => user_id.clone(),
        Principal::Admin => "u_default".to_owned(),
        Principal::Box { .. } => return Err((StatusCode::FORBIDDEN, "devices only".into())),
    };
    let reports = crate::migrate::migrate_user(&state, &user_id, q.project.as_deref(), q.force)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    Ok(Json(crate::migrate::report_json(&user_id, &reports)).into_response())
}

#[derive(Deserialize)]
pub struct MigrateQuery {
    /// Only this project (id).
    #[serde(default)]
    pub project: Option<String>,
    /// Copy again even when the legacy sandbox is already marked migrated.
    #[serde(default)]
    pub force: bool,
}

#[derive(Deserialize)]
pub struct UserBoxBody {
    /// Daytona snapshot name (fixes vCPU/RAM/disk); null clears the override.
    pub snapshot: Option<String>,
}

/// `PUT /admin/users/{id}/box`: per-user box class. Applies to the next box create (stop and
/// delete the current sandbox to re-create at the new class).
pub async fn admin_set_user_box(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<UserBoxBody>,
) -> ApiResult {
    if !matches!(principal, Principal::Admin) {
        return Err((StatusCode::FORBIDDEN, "admin only".into()));
    }
    state
        .db
        .set_user_snapshot(&id, b.snapshot.as_deref())
        .map_err(internal)?;
    Ok(Json(json!({ "user_id": id, "snapshot": b.snapshot })).into_response())
}

#[derive(Deserialize)]
pub struct CreateChatBody {
    #[serde(default)]
    pub title: Option<String>,
    /// `chat` (default) or `projects` (the box creates its S15 "Projects ⇄" tab on the first
    /// cross-project message). Group chats go through `POST /projects/{id}/groups`.
    #[serde(default)]
    pub kind: Option<String>,
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
    if !state.allows(&principal, &p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let kind = match b.kind.as_deref() {
        None | Some("chat") => "chat",
        Some("projects") => "projects",
        Some(other) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("kind must be chat|projects, got {other}"),
            ))
        }
    };
    let existing = state.db.list_chats(&p.id).map_err(internal)?;
    // One "Projects ⇄" tab per project: a second request returns the first.
    if kind == "projects" {
        if let Some(c) = existing.iter().find(|c| c.kind == "projects") {
            return Ok(Json(
                json!({ "chat": chat_json(c), "room": format!("/room/{}/{}", p.id, c.id) }),
            )
            .into_response());
        }
    }
    let chat = Chat {
        id: ulid(),
        project_id: p.id.clone(),
        title: b.title.unwrap_or_else(|| {
            if kind == "projects" {
                "Projects ⇄".into()
            } else {
                "New chat".into()
            }
        }),
        is_pinned: false,
        position: existing.len() as i64,
        created_at: now_ms(),
        doc_epoch: 1,
        kind: kind.into(),
        participants: Vec::new(),
    };
    state.db.insert_chat(&chat).map_err(internal)?;
    Ok(
        Json(json!({ "chat": chat_json(&chat), "room": format!("/room/{}/{}", p.id, chat.id) }))
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
    if !state.allows(&principal, &p.id) {
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
    if !state.allows(&principal, &p.id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    boxes::ensure_awake(&state, &p.user_id, "api").await;
    let (bs, since) = state.box_state(&p.user_id).await;
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
    if !state.allows(&principal, &p.id) || principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    boxes::start_box(&state, &p.user_id)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let (bs, since) = state.box_state(&p.user_id).await;
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
    if !state.owns(&principal, &p.id) || principal.is_box() {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    boxes::stop_box(&state, &p.user_id, q.force)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let (bs, since) = state.box_state(&p.user_id).await;
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
    if !state.allows(&principal, &id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let owner = state
        .user_of(&id)
        .ok_or((StatusCode::NOT_FOUND, "no such project".to_owned()))?;
    boxes::keepalive(&state, &owner, b.turn_running)
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

/// One project as the box sees it: settings, chats, and the pushes queued for it.
fn workspace_json(state: &AppState, p: &Project) -> Result<Value, (StatusCode, String)> {
    let chats = state.db.list_chats(&p.id).map_err(internal)?;
    let pushes = state.db.take_pushes(&p.id).map_err(internal)?;
    let mut project = serde_json::to_value(p).unwrap_or(Value::Null);
    project["skills_rev"] = json!(state.db.skills_rev(Some(&p.id)).map_err(internal)?);
    Ok(json!({
        "project": project,
        "chats": chats,
        "pushes": pushes.into_iter().map(|(i, k, v)| json!({ "id": i, "kind": k, "payload": v })).collect::<Vec<_>>(),
    }))
}

/// The box announces itself after boot (`POST /box/hello`): one box per user, so the answer lists
/// every project of that user as a workspace, with settings, chats, and queued pushes.
pub async fn box_hello(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<BoxHelloBody>,
) -> ApiResult {
    let Principal::Box { user_id } = &principal else {
        return Err((StatusCode::FORBIDDEN, "box only".into()));
    };
    tracing::info!(user = %user_id, version = ?b.version, "box hello");
    if b.proto.unwrap_or(arbos_core::PROTO_VERSION) != arbos_core::PROTO_VERSION {
        return Err((
            StatusCode::CONFLICT,
            "incompatible agentd protocol; rebuild box (D44)".into(),
        ));
    }
    boxes::box_seen_http(&state, user_id).await;
    if let Some(url) = &b.url {
        if let Ok(Some(mut sb)) = state.db.user_box(user_id) {
            if sb.provider != "daytona" {
                sb.box_url = Some(url.clone());
                let _ = state.db.upsert_box(&sb);
            }
        }
    }
    let projects = state.db.projects_of_user(user_id).map_err(internal)?;
    let mut workspaces = Vec::new();
    for p in &projects {
        workspaces.push(workspace_json(&state, p)?);
    }
    Ok(Json(json!({
        "user_id": user_id,
        "workspaces": workspaces,
        "relay_version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response())
}

/// `GET /box/projects`: the box's 3 s poll — every project of its user (settings + chats), so new
/// projects become workspaces and settings changes land without a restart.
pub async fn box_projects(State(state): State<AppState>, principal: Principal) -> ApiResult {
    let Principal::Box { user_id } = &principal else {
        return Err((StatusCode::FORBIDDEN, "box only".into()));
    };
    boxes::box_seen_http(&state, user_id).await;
    let projects = state.db.projects_of_user(user_id).map_err(internal)?;
    let mut out = Vec::new();
    for p in &projects {
        let chats = state.db.list_chats(&p.id).map_err(internal)?;
        let mut project = serde_json::to_value(p).unwrap_or(Value::Null);
        project["avatar"] = json!(p
            .avatar
            .clone()
            .unwrap_or_else(|| crate::avatar::default_for(&p.id, &state.cfg.avatar_slugs)));
        project["skills_rev"] = json!(state.db.skills_rev(Some(&p.id)).map_err(internal)?);
        let people_chats = crate::people::people_chats_of_project(&state, &p.id, user_id);
        out.push(json!({ "project": project, "chats": chats, "people_chats": people_chats }));
    }
    Ok(Json(json!({ "user_id": user_id, "workspaces": out })).into_response())
}

/// `POST /box/keepalive`: box-level keep-alive (any workspace mid-turn keeps the box awake).
pub async fn box_keepalive(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<KeepaliveBody>,
) -> ApiResult {
    let Principal::Box { user_id } = &principal else {
        return Err((StatusCode::FORBIDDEN, "box only".into()));
    };
    boxes::box_seen_http(&state, user_id).await;
    boxes::keepalive(&state, user_id, b.turn_running)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct WorkspaceStateBody {
    pub state: String,
}

/// `POST /projects/{id}/workspace`: the box reports a workspace's state (`running` | `stopped`).
pub async fn set_workspace_state(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<WorkspaceStateBody>,
) -> ApiResult {
    if !principal.is_box() || !state.allows(&principal, &id) {
        return Err((StatusCode::FORBIDDEN, "box of this project only".into()));
    }
    let st = match b.state.as_str() {
        "running" | "starting" | "stopped" | "error" => b.state.as_str(),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "state must be running|starting|stopped|error".into(),
            ))
        }
    };
    state.db.set_workspace_state(&id, st).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT.into_response())
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
        boxes::ensure_awake(&state, &p.user_id, "cross-project").await;
        crate::routines::fire_event(&state, &p.id, "cross_project_message", b.payload).await;
    }
    Ok(StatusCode::ACCEPTED.into_response())
}

pub async fn take_pushes(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !state.allows(&principal, &id) {
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
    if !state.allows(&principal, &id) {
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
    if !state.allows(&principal, &id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    state
        .db
        .record_timing(&id, &b.kind, b.ms, b.detail.as_deref())
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct ActivityBody {
    pub chat_id: String,
    #[serde(default)]
    pub text: Option<String>,
}

/// `POST /projects/{id}/activity` (box): what the agent is doing right now, in plain words
/// (agent-flow contract §3). `text: null` clears it. Kept in memory; shown on the project JSON.
pub async fn set_activity(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<ActivityBody>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !state.allows(&principal, &id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let mut map = state.activity.lock().await;
    match b.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => {
            map.insert(
                id,
                crate::state::ProjectActivity {
                    chat_id: b.chat_id,
                    text: text.chars().take(80).collect(),
                    at: arbos_core::ids::now_ms(),
                },
            );
        }
        None => {
            map.remove(&id);
        }
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The project's current activity, or null when none or stale.
async fn activity_json(state: &AppState, project_id: &str) -> Value {
    let map = state.activity.lock().await;
    match map.get(project_id) {
        Some(a) if arbos_core::ids::now_ms() - a.at <= crate::state::ACTIVITY_TTL_MS => {
            serde_json::to_value(a).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

/// Audit mirror (feature `approve`): the box posts each `audit.jsonl` line (`approval_requested`,
/// `approval_decided`, …) so clients can read them while the box is asleep.
pub async fn record_audit(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(entry): Json<Value>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !state.allows(&principal, &id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let kind = entry
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    state
        .db
        .record_audit(&id, &kind, &entry)
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn list_audit(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Query(q): Query<AuditQuery>,
) -> ApiResult {
    let id = resolve_project_id(&state, &id)?;
    if !state.allows(&principal, &id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let entries = state.db.audit(&id, limit).map_err(internal)?;
    Ok(Json(json!({ "project_id": id, "entries": entries })).into_response())
}
