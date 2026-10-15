//! People chats (D72 v1, permissive): find a person by GitHub handle, open a DM, list chats,
//! per-device read marks, blocks, per-user notifications, and the room ACL. A people chat is a
//! Loro chat doc in the room `people/<chat_id>` (`WS /room/people/{chat_id}`); the relay holds
//! membership and enforces access by **user**, so every device of a member is a peer.
//!
//! No request gate: the first DM opens for both people at once. Backstops only: block, 200 row
//! pushes per person per minute across people rooms, 50 new chats per person per day (both
//! halved for accounts under seven days old).

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};
use arbos_core::wire;

use crate::auth::Principal;
use crate::db_people::{ChatWithMembers, Membership, PeopleChat};
use crate::signin::api_error;
use crate::state::AppState;

/// Project slot used for people rooms and blobs: `/room/people/{chat}`, `/blob/people:{chat}/…`.
pub const PEOPLE_PROJECT: &str = "people";
pub const PEOPLE_ACL_PREFIX: &str = "people:";

const SEARCH_LIMIT: i64 = 10;
const SEARCH_PER_MIN: i64 = 60;
const MSGS_PER_MIN: i64 = 200;
const NEW_CHATS_PER_DAY: i64 = 50;
const YOUNG_ACCOUNT_MS: i64 = 7 * 24 * 3_600_000;
const GITHUB_MISS_TTL_MS: i64 = 3_600_000;

pub fn room_of(chat_id: &str) -> String {
    wire::room_name(PEOPLE_PROJECT, chat_id)
}

// ---- ACL ----------------------------------------------------------------------------------------

/// May this principal read and write the room of a people chat? Devices: their user must be a
/// live member. Boxes: one of the chat's agents must be a project of the box's user. Admin: yes.
pub fn room_access(state: &AppState, principal: &Principal, chat_id: &str) -> bool {
    if state.db.people_chat(chat_id).ok().flatten().is_none() {
        return false;
    }
    match principal {
        Principal::Admin => true,
        Principal::Device { user_id, .. } => state
            .db
            .membership(chat_id, &format!("user:{user_id}"))
            .ok()
            .flatten()
            .is_some(),
        Principal::Box { user_id } => state
            .db
            .members_of(chat_id)
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.project_id().map(str::to_owned))
            .any(|pid| {
                state
                    .db
                    .project(&pid)
                    .ok()
                    .flatten()
                    .is_some_and(|p| p.user_id == *user_id)
            }),
    }
}

/// The user behind a principal, or a 403.
fn user_of(principal: &Principal) -> Result<String, Box<Response>> {
    match principal {
        Principal::Device { user_id, .. } => Ok(user_id.clone()),
        Principal::Box { .. } | Principal::Admin => Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a signed-in device is required",
        ))),
    }
}

fn device_of(principal: &Principal) -> String {
    match principal {
        Principal::Device { id, .. } => id.clone(),
        Principal::Box { user_id } => format!("box:{user_id}"),
        Principal::Admin => "admin".to_owned(),
    }
}

fn young_account(state: &AppState, user_id: &str) -> bool {
    state
        .db
        .user(user_id)
        .ok()
        .flatten()
        .is_some_and(|u| now_ms() - u.created_at < YOUNG_ACCOUNT_MS)
}

fn rate_limited(retry_s: i64) -> Response {
    let mut r = api_error(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "slow down");
    r.headers_mut().insert(
        header::RETRY_AFTER,
        retry_s.to_string().parse().expect("retry-after"),
    );
    r
}

fn normalize_handle(h: &str) -> Option<String> {
    let h = h.trim().trim_start_matches('@').trim();
    let ok = !h.is_empty()
        && h.len() <= 39
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !h.starts_with('-')
        && !h.ends_with('-');
    ok.then(|| h.to_owned())
}

// ---- GitHub lookup for people who have not signed in ----------------------------------------

/// Resolve a GitHub login that is not in `users` yet by asking GitHub. With `create` the person
/// becomes an `invited` placeholder keyed by the GitHub id (a DM is about to open); without it
/// the card is returned as `status: "github"` and nothing is stored. Misses are remembered for
/// an hour in `meta`.
async fn resolve_via_github(state: &AppState, login: &str, create: bool) -> Option<Value> {
    let miss_key = format!("gh_miss:{}", login.to_lowercase());
    if let Ok(Some(at)) = state.db.meta_get(&miss_key) {
        if at
            .parse::<i64>()
            .is_ok_and(|t| now_ms() - t < GITHUB_MISS_TTL_MS)
        {
            return None;
        }
    }
    let mut req = state
        .http
        .get(format!("https://api.github.com/users/{login}"))
        .header(header::ACCEPT, "application/vnd.github+json")
        .header(header::USER_AGENT, "arbos-relay");
    if let Some(tok) = std::env::var("ARBOS_GITHUB_TOKEN")
        .ok()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .filter(|s| !s.trim().is_empty())
    {
        req = req.bearer_auth(tok.trim());
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, login, "github user lookup");
            return None;
        }
    };
    if resp.status() == StatusCode::NOT_FOUND {
        let _ = state.db.meta_set(&miss_key, &now_ms().to_string());
        return None;
    }
    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), login, "github user lookup");
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    if v.get("type").and_then(Value::as_str) == Some("Organization") {
        let _ = state.db.meta_set(&miss_key, &now_ms().to_string());
        return None;
    }
    let id = v.get("id").and_then(Value::as_i64)?;
    let gh_login = v.get("login").and_then(Value::as_str)?.to_owned();
    let avatar = v.get("avatar_url").and_then(Value::as_str);
    let name = v.get("name").and_then(Value::as_str);
    if !create {
        return Some(json!({
            "id": format!("gh_{id}"), "handle": gh_login, "avatar_url": avatar, "name": name, "status": "github",
        }));
    }
    let user_id = state
        .db
        .upsert_invited_user(id, &gh_login, avatar, name)
        .ok()?;
    state.db.user_card(&user_id).ok().flatten()
}

/// A person by handle for a DM or a block: local first, GitHub second (stored as `invited`).
async fn find_person(state: &AppState, handle: &str) -> Option<Value> {
    if let Ok(Some(u)) = state.db.user_by_handle(handle) {
        return Some(u);
    }
    resolve_via_github(state, handle, true).await
}

// ---- JSON shapes --------------------------------------------------------------------------------

fn member_json(state: &AppState, m: &Membership) -> Value {
    let mut v = json!({
        "member": m.member, "role": m.role, "added_by": m.added_by, "joined_at": m.joined_at,
    });
    if let Some(uid) = m.user_id() {
        if let Ok(Some(card)) = state.db.user_card(uid) {
            v["handle"] = card["handle"].clone();
            v["avatar_url"] = card["avatar_url"].clone();
            v["name"] = card["name"].clone();
            v["status"] = card["status"].clone();
        }
    }
    if let Some(pid) = m.project_id() {
        if let Ok(Some(p)) = state.db.project(pid) {
            v["handle"] = json!(agent_slug(&p.name));
            v["project"] = json!({ "id": p.id, "name": p.name, "icon": p.icon, "color": p.color });
            v["owner"] = json!(p.user_id);
            v["owner_handle"] = state
                .db
                .user_card(&p.user_id)
                .ok()
                .flatten()
                .map(|c| c["handle"].clone())
                .unwrap_or(Value::Null);
        }
        v["settings"] = if m.settings.is_object() {
            m.settings.clone()
        } else {
            json!({ "reply_policy": "mention", "restrict_guests": false })
        };
    }
    v
}

/// The `@slug` a project agent answers to: its name lower-cased, spaces to hyphens.
pub fn agent_slug(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

pub fn chat_json(state: &AppState, cm: &ChatWithMembers, me: &str, device_id: &str) -> Value {
    let (head_seq, last_row_at) = state
        .db
        .room_activity(&room_of(&cm.chat.id))
        .unwrap_or((0, 0));
    let (read_seq, _) = state
        .db
        .read_mark(&cm.chat.id, device_id)
        .unwrap_or((0, false));
    let members: Vec<Value> = cm.members.iter().map(|m| member_json(state, m)).collect();
    let others: Vec<&Value> = members
        .iter()
        .filter(|m| {
            m["member"]
                .as_str()
                .is_some_and(|s| s.starts_with("user:") && s != format!("user:{me}"))
        })
        .collect();
    let title = others
        .iter()
        .filter_map(|m| m["handle"].as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let pending = others
        .iter()
        .any(|m| m["status"].as_str() == Some("invited"));
    let has_agent = cm.members.iter().any(|m| m.project_id().is_some());
    let mine = cm.members.iter().find(|m| m.user_id() == Some(me));
    json!({
        "id": cm.chat.id,
        "kind": cm.chat.kind,
        "created_by": cm.chat.created_by,
        "created_at": cm.chat.created_at,
        "title": title,
        "pending": pending,
        "members": members,
        "room": format!("/room/{PEOPLE_PROJECT}/{}", cm.chat.id),
        "head_seq": head_seq,
        "last_activity_at": if last_row_at > 0 { last_row_at } else { cm.chat.created_at },
        "read_seq": read_seq,
        "unread": head_seq > read_seq,
        "lock_state": if has_agent { "visible_to_agents" } else { "people_only" },
        "me": { "role": mine.map(|m| m.role.clone()), "muted": mine.is_some_and(|m| m.muted) },
    })
}

fn load_chat(state: &AppState, chat_id: &str) -> Result<ChatWithMembers, Box<Response>> {
    let chat = state
        .db
        .people_chat(chat_id)
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
                "no such chat",
            ))
        })?;
    let members = state.db.members_of(chat_id).map_err(|e| {
        Box::new(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "db",
            e.to_string(),
        ))
    })?;
    Ok(ChatWithMembers { chat, members })
}

/// The chat, if the caller's user is a live member.
fn member_chat(
    state: &AppState,
    principal: &Principal,
    chat_id: &str,
) -> Result<(String, ChatWithMembers), Box<Response>> {
    let me = user_of(principal)?;
    let cm = load_chat(state, chat_id)?;
    if !cm.members.iter().any(|m| m.user_id() == Some(me.as_str())) {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "not_member",
            "you are not in this chat",
        )));
    }
    Ok((me, cm))
}

fn handle_of(state: &AppState, user_id: &str) -> String {
    state
        .db
        .user_card(user_id)
        .ok()
        .flatten()
        .and_then(|c| c["handle"].as_str().map(str::to_owned))
        .unwrap_or_else(|| user_id.to_owned())
}

pub fn human_user_ids(cm: &ChatWithMembers) -> Vec<String> {
    cm.members
        .iter()
        .filter_map(|m| m.user_id().map(str::to_owned))
        .collect()
}

// ---- handlers -----------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
}

/// `GET /users/search?q=@jac`
pub async fn users_search(
    State(state): State<AppState>,
    principal: Principal,
    Query(q): Query<SearchQuery>,
) -> Response {
    let me = match user_of(&principal) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let Some(prefix) = normalize_handle(&q.q) else {
        return Json(Value::Array(Vec::new())).into_response();
    };
    match state.db.rate_event(&me, "search", 60_000) {
        Ok(n) if n > SEARCH_PER_MIN => return rate_limited(60),
        _ => {}
    }
    let mut found = state
        .db
        .search_users(&prefix, SEARCH_LIMIT)
        .unwrap_or_default();
    found.retain(|u| u["id"].as_str() != Some(me.as_str()));
    let exact = found.iter().any(|u| {
        u["handle"]
            .as_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(&prefix))
    });
    if !exact {
        if let Some(card) = resolve_via_github(&state, &prefix, false).await {
            if card["id"].as_str() != Some(me.as_str()) {
                found.insert(0, card);
                found.truncate(SEARCH_LIMIT as usize);
            }
        }
    }
    Json(found).into_response()
}

#[derive(Deserialize)]
pub struct DmBody {
    pub handle: String,
}

/// `POST /chats/dm {"handle"}` → the DM with that person (existing or new; opens for both).
pub async fn dm_create(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<DmBody>,
) -> Response {
    let me = match user_of(&principal) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let Some(handle) = normalize_handle(&b.handle) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_handle",
            "that is not a GitHub login",
        );
    };
    let Some(other) = find_person(&state, &handle).await else {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no GitHub account with that login",
        );
    };
    let other_id = other["id"].as_str().unwrap_or_default().to_owned();
    if other_id == me {
        return api_error(StatusCode::BAD_REQUEST, "bad_handle", "that is you");
    }
    if state.db.blocked_either_way(&me, &other_id).unwrap_or(false) {
        return api_error(
            StatusCode::FORBIDDEN,
            "not_available",
            "this person is not available",
        );
    }
    let device = device_of(&principal);
    if let Ok(Some(existing)) = state.db.dm_between(&me, &other_id) {
        return match load_chat(&state, &existing.id) {
            Ok(cm) => Json(chat_json(&state, &cm, &me, &device)).into_response(),
            Err(r) => *r,
        };
    }
    let limit = if young_account(&state, &me) {
        NEW_CHATS_PER_DAY / 2
    } else {
        NEW_CHATS_PER_DAY
    };
    let today = crate::proxy::start_of_utc_day_ms();
    if state.db.people_chats_created_since(&me, today).unwrap_or(0) >= limit {
        return rate_limited(((today + 86_400_000 - now_ms()) / 1000).max(1));
    }
    let chat = PeopleChat {
        id: ulid(),
        kind: "dm".into(),
        created_by: me.clone(),
        created_at: now_ms(),
    };
    if let Err(e) = state.db.insert_people_chat(&chat) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    for uid in [&me, &other_id] {
        if let Err(e) =
            state
                .db
                .add_member(&chat.id, &format!("user:{uid}"), "owner", &me, &json!({}))
        {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
        }
    }
    let _ = state.db.push_user(
        &other_id,
        "people_chat_created",
        &json!({ "chat_id": chat.id, "by": format!("user:{me}"), "by_handle": handle_of(&state, &me) }),
        Some(&chat.id),
    );
    tracing::info!(chat = %chat.id, %me, other = %other_id, "dm opened");
    match load_chat(&state, &chat.id) {
        Ok(cm) => Json(chat_json(&state, &cm, &me, &device)).into_response(),
        Err(r) => *r,
    }
}

/// `GET /chats` — my people chats, newest activity first; hidden-on-this-device ones excluded.
pub async fn chats_list(State(state): State<AppState>, principal: Principal) -> Response {
    let me = match user_of(&principal) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let device = device_of(&principal);
    let chats = match state.db.chats_of_member(&format!("user:{me}")) {
        Ok(c) => c,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    };
    let mut out: Vec<Value> = chats
        .iter()
        .filter(|cm| {
            !state
                .db
                .read_mark(&cm.chat.id, &device)
                .map(|(_, hidden)| hidden)
                .unwrap_or(false)
        })
        .map(|cm| chat_json(&state, cm, &me, &device))
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c["last_activity_at"].as_i64().unwrap_or(0)));
    Json(out).into_response()
}

/// `GET /chats/{id}`
pub async fn chat_get(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    match member_chat(&state, &principal, &id) {
        Ok((me, cm)) => Json(chat_json(&state, &cm, &me, &device_of(&principal))).into_response(),
        Err(r) => *r,
    }
}

#[derive(Deserialize)]
pub struct ReadBody {
    pub seq: i64,
}

/// `PUT /chats/{id}/read {"seq"}` — this device has seen rows up to `seq`.
pub async fn chat_read(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<ReadBody>,
) -> Response {
    if let Err(r) = member_chat(&state, &principal, &id) {
        return *r;
    }
    match state
        .db
        .set_read_mark(&id, &device_of(&principal), b.seq.max(0))
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `POST /chats/{id}/hide` — delete-for-me on this device (a new message un-hides it).
pub async fn chat_hide(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = member_chat(&state, &principal, &id) {
        return *r;
    }
    match state.db.set_hidden(&id, &device_of(&principal), true) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `POST /chats/{id}/leave`
pub async fn chat_leave(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    let (me, cm) = match member_chat(&state, &principal, &id) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if let Err(e) = state.db.remove_member(&id, &format!("user:{me}")) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    notify_members(
        &state,
        &cm,
        &me,
        "people_member_removed",
        json!({ "chat_id": id, "user_id": me, "handle": handle_of(&state, &me), "by_handle": handle_of(&state, &me) }),
    );
    StatusCode::NO_CONTENT.into_response()
}

/// `DELETE /chats/{id}` — chat owner; rows, checkpoints, and blobs go.
pub async fn chat_delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    let (me, cm) = match member_chat(&state, &principal, &id) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if !cm
        .members
        .iter()
        .any(|m| m.user_id() == Some(me.as_str()) && m.role == "owner")
    {
        return api_error(
            StatusCode::FORBIDDEN,
            "owner_only",
            "only an owner can delete the chat",
        );
    }
    if let Err(e) = state.db.delete_people_chat(&id) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    let _ = state.db.delete_room(&room_of(&id));
    let _ = std::fs::remove_dir_all(
        state
            .cfg
            .data_dir
            .join("blobs")
            .join(format!("{PEOPLE_ACL_PREFIX}{id}")),
    );
    state
        .room(&room_of(&id))
        .await
        .tx
        .send(crate::state::RoomBroadcast {
            from_conn: 0,
            bytes: wire::Frame::new(
                wire::FrameType::Error,
                json!({ "code": "chat_deleted", "message": "this chat was deleted" }),
                bytes::Bytes::new(),
            )
            .encode(),
        })
        .ok();
    notify_members(
        &state,
        &cm,
        &me,
        "people_chat_deleted",
        json!({ "chat_id": id, "by_handle": handle_of(&state, &me) }),
    );
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /me/pushes` — this device's pending notifications, delivered once.
pub async fn me_pushes(State(state): State<AppState>, principal: Principal) -> Response {
    let Principal::Device { id, .. } = &principal else {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "devices only");
    };
    match state.db.take_user_pushes(id) {
        Ok(items) => Json(
            items
                .into_iter()
                .map(|(id, kind, payload, at)| json!({ "id": id, "kind": kind, "payload": payload, "created_at": at }))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct BlockBody {
    pub handle: String,
}

/// `POST /blocks {"handle"}`
pub async fn block_create(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<BlockBody>,
) -> Response {
    let me = match user_of(&principal) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let Some(handle) = normalize_handle(&b.handle) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_handle",
            "that is not a GitHub login",
        );
    };
    let Some(other) = find_person(&state, &handle).await else {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no GitHub account with that login",
        );
    };
    let other_id = other["id"].as_str().unwrap_or_default();
    if other_id == me {
        return api_error(StatusCode::BAD_REQUEST, "bad_handle", "that is you");
    }
    match state.db.block(&me, other_id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `DELETE /blocks/{user_id}`
pub async fn block_delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(user_id): Path<String>,
) -> Response {
    let me = match user_of(&principal) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    match state.db.unblock(&me, &user_id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(StatusCode::NOT_FOUND, "not_found", "not blocked"),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `GET /blocks`
pub async fn blocks_list(State(state): State<AppState>, principal: Principal) -> Response {
    let me = match user_of(&principal) {
        Ok(u) => u,
        Err(r) => return *r,
    };
    let rows = state.db.blocks_of(&me).unwrap_or_default();
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(uid, at)| {
            let mut v = state
                .db
                .user_card(&uid)
                .ok()
                .flatten()
                .unwrap_or_else(|| json!({ "id": uid }));
            v["created_at"] = json!(at);
            v
        })
        .collect();
    Json(out).into_response()
}

// ---- room hooks ---------------------------------------------------------------------------------

/// Why a row push into a people room was refused (sent back as an `error` frame).
pub enum PushRefused {
    RateLimited,
    NotAvailable,
}

impl PushRefused {
    pub fn code(&self) -> &'static str {
        match self {
            PushRefused::RateLimited => "rate_limited",
            PushRefused::NotAvailable => "not_available",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            PushRefused::RateLimited => "slow down",
            PushRefused::NotAvailable => "this chat is not available",
        }
    }
}

/// Called on every row a device pushes into `people/<chat>`: rate limit, block check, and a
/// coalesced `people_message` push to the other people in the chat.
pub fn on_room_push(
    state: &AppState,
    principal: &Principal,
    chat_id: &str,
) -> Result<(), PushRefused> {
    let Principal::Device { user_id, .. } = principal else {
        return Ok(()); // boxes (agents) are metered by their owner's credits, not here
    };
    let limit = if young_account(state, user_id) {
        MSGS_PER_MIN / 2
    } else {
        MSGS_PER_MIN
    };
    if state
        .db
        .rate_event(user_id, "msg", 60_000)
        .is_ok_and(|n| n > limit)
    {
        return Err(PushRefused::RateLimited);
    }
    let Ok(cm) = load_chat(state, chat_id) else {
        return Err(PushRefused::NotAvailable);
    };
    let humans = human_user_ids(&cm);
    if state.db.blocked_by_any(user_id, &humans).unwrap_or(false) {
        return Err(PushRefused::NotAvailable);
    }
    let (head, _) = state.db.room_activity(&room_of(chat_id)).unwrap_or((0, 0));
    let payload = json!({
        "chat_id": chat_id, "from": format!("user:{user_id}"), "from_handle": handle_of(state, user_id), "seq": head + 1,
    });
    for other in humans.iter().filter(|u| *u != user_id) {
        let _ = state
            .db
            .push_user(other, "people_message", &payload, Some(chat_id));
        // A message un-hides a chat that was "deleted for me" on any of their devices.
        if let Ok(devices) = state.db.devices_of_user(other) {
            for d in devices {
                if let Some(id) = d["id"].as_str() {
                    let _ = state.db.set_hidden(chat_id, id, false);
                }
            }
        }
    }
    Ok(())
}

/// Notify every human member except `skip`.
pub fn notify_members(
    state: &AppState,
    cm: &ChatWithMembers,
    skip: &str,
    kind: &str,
    payload: Value,
) {
    for uid in human_user_ids(cm) {
        if uid != skip {
            let _ = state.db.push_user(&uid, kind, &payload, None);
        }
    }
}

#[cfg(test)]
mod tests {
    //! ACL invariant: only a member's devices (and the box of a project whose agent is in the
    //! chat) may touch a people room.

    use arbos_core::ids::ulid;

    use super::*;
    use crate::db::{Chat, Project};
    use crate::db_accounts::GithubIdentity;
    use crate::state::test_support::test_state;
    use crate::state::AuthMode;

    fn person(state: &AppState, github_id: i64) -> (String, Principal) {
        let (user, _) = state
            .db
            .upsert_github_user(
                &GithubIdentity {
                    id: github_id,
                    login: format!("user{github_id}"),
                    avatar_url: None,
                    name: None,
                },
                20.0,
            )
            .expect("user");
        let device_id = ulid();
        state
            .db
            .insert_device(&device_id, "d", &ulid(), &user.id, "github")
            .expect("device");
        (
            user.id.clone(),
            Principal::Device {
                id: device_id,
                token: String::new(),
                user_id: user.id,
            },
        )
    }

    fn dm(state: &AppState, a: &str, b: &str) -> String {
        let chat = PeopleChat {
            id: ulid(),
            kind: "dm".into(),
            created_by: a.to_owned(),
            created_at: now_ms(),
        };
        state.db.insert_people_chat(&chat).expect("chat");
        for u in [a, b] {
            state
                .db
                .add_member(&chat.id, &format!("user:{u}"), "owner", a, &json!({}))
                .expect("member");
        }
        chat.id
    }

    #[test]
    fn non_member_cannot_read_a_people_room() {
        let state = test_state(AuthMode::Github);
        let (alice, alice_dev) = person(&state, 1);
        let (bob, bob_dev) = person(&state, 2);
        let (_carol, carol_dev) = person(&state, 3);
        let chat = dm(&state, &alice, &bob);
        let key = format!("{PEOPLE_ACL_PREFIX}{chat}");

        assert!(state.allows(&alice_dev, &key));
        assert!(state.allows(&bob_dev, &key));
        assert!(!state.allows(&carol_dev, &key), "a third person is refused");
        assert!(state.allows(&Principal::Admin, &key));
        assert!(
            !state.allows(&alice_dev, &format!("{PEOPLE_ACL_PREFIX}{}", ulid())),
            "unknown chat"
        );

        // A box is admitted only through one of its own projects being an agent in the chat.
        let carol_box = Principal::Box {
            user_id: "gh_3".into(),
        };
        let alice_box = Principal::Box {
            user_id: alice.clone(),
        };
        assert!(!state.allows(&alice_box, &key), "no agent in the chat yet");
        let project = Project {
            id: ulid(),
            name: "Research".into(),
            icon: "R".into(),
            instructions: String::new(),
            models: json!({}),
            state: "hibernated_light".into(),
            created_at: now_ms(),
            last_active_at: now_ms(),
            spend_cap_daily: 25.0,
            color: "#000".into(),
            user_id: alice.clone(),
            workspace_state: "stopped".into(),
        };
        state.db.insert_project(&project).expect("project");
        state
            .db
            .insert_chat(&Chat {
                id: ulid(),
                project_id: project.id.clone(),
                title: "Research".into(),
                is_pinned: true,
                position: 0,
                created_at: now_ms(),
                doc_epoch: 1,
                kind: "chat".into(),
                participants: Vec::new(),
            })
            .expect("pinned chat");
        state
            .db
            .add_member(
                &chat,
                &format!("agent:{}", project.id),
                "agent",
                &alice,
                &json!({}),
            )
            .expect("agent member");
        assert!(
            state.allows(&alice_box, &key),
            "the owner's box joins for its agent"
        );
        assert!(
            !state.allows(&carol_box, &key),
            "another user's box is still refused"
        );

        // Leaving drops access at once.
        state
            .db
            .remove_member(&chat, &format!("user:{bob}"))
            .expect("leave");
        assert!(!state.allows(&bob_dev, &key));
    }
}
