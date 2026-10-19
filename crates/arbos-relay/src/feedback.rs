//! In-app feedback: the Mac sheet (⌘⇧F), the iPhone sheet (shake / Settings / chat menu) and the
//! agent's `submit_feedback` tool all land here. `POST /feedback` stores the optional image, the
//! note, the app version and the sender under `<data>/feedback/<id>/`; a note alone is enough.
//! A box (the user's agent) may submit too: the item is attributed to the box's owning user and
//! marked `submitted_by: "agent"`. The feedback loop on the server (`scripts/feedback`) lists new
//! items with `GET /admin/feedback?since=<ms>` and fetches each image with
//! `GET /admin/feedback/{id}/image` (admin token; 404 when none).
//!
//! Screen capture for agent-filed items: the box asks `POST /box/feedback/capture`; the relay
//! pushes `feedback_capture` to the user's signed-in devices (`/me/pushes`); the app in the
//! foreground grabs its own window and answers `POST /feedback/capture/{id}`; the box polls
//! `GET /box/feedback/capture/{id}` and names the capture in its `POST /feedback` (`capture_id` +
//! `capture_device_id`). Captures live under `<data>/feedback-captures/<id>/` for one hour.
//!
//! Issue link back to the chat: the loop posts `POST /admin/feedback/{id}/issue` once the
//! GitHub issue exists; the relay records it on the item and pushes `feedback_issue` to the
//! project's box so the agent can tell the user the number.

use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};

use crate::auth::Principal;
use crate::signin::api_error;
use crate::state::AppState;

/// 15 MB of image, the same ceiling the Cursor agent API accepts.
const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;
/// The agent's rollout (recent turns, tool calls, timings) attached to an item.
const MAX_ROLLOUT_CHARS: usize = 48_000;
/// A capture request the devices never answered is dropped after this.
const CAPTURE_TTL_MS: i64 = 60 * 60 * 1000;

#[derive(Deserialize)]
pub struct Submit {
    #[serde(default)]
    pub note: String,
    /// Base64 (standard alphabet) of the PNG/JPEG bytes. Empty = note-only feedback.
    #[serde(default)]
    pub image: String,
    #[serde(default = "default_mime")]
    pub mime: String,
    #[serde(default)]
    pub app_version: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// `"user"` (default) or `"agent"`; a box principal is always `"agent"`.
    #[serde(default)]
    pub submitted_by: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    /// The chat the report came from (agent-filed items).
    #[serde(default)]
    pub chat_id: Option<String>,
    /// Markdown: recent turns, tool calls, errors and timings, already scrubbed by the box.
    #[serde(default)]
    pub rollout: Option<String>,
    /// Platform, versions, device, agent and box identity collected by the box.
    #[serde(default)]
    pub context: Option<Value>,
    /// A device capture to attach instead of `image` (box principal; see module docs).
    #[serde(default)]
    pub capture_id: Option<String>,
    #[serde(default)]
    pub capture_device_id: Option<String>,
}

fn default_mime() -> String {
    "image/png".to_owned()
}

fn default_platform() -> String {
    "mac".to_owned()
}

fn default_submitted_by() -> String {
    "user".to_owned()
}

/// `meta.json` beside the image (when present); also what the admin list returns.
#[derive(Serialize, Deserialize, Clone)]
pub struct Item {
    pub id: String,
    pub created_at: i64,
    pub note: String,
    pub mime: String,
    /// Empty when the tester sent a note with no screenshot.
    pub file: String,
    pub bytes: usize,
    pub app_version: String,
    pub platform: String,
    pub os: String,
    pub project_id: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub user_id: String,
    pub user_login: Option<String>,
    pub device_id: Option<String>,
    #[serde(default = "default_submitted_by")]
    pub submitted_by: String,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub rollout: Option<String>,
    #[serde(default)]
    pub context: Option<Value>,
    /// `{ "number", "url" }` once the loop opened the GitHub issue.
    #[serde(default)]
    pub issue: Option<Value>,
}

fn root(state: &AppState) -> PathBuf {
    state.cfg.data_dir.join("feedback")
}

fn captures_root(state: &AppState) -> PathBuf {
    state.cfg.data_dir.join("feedback-captures")
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn ext_for(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn decode_image(raw: &str) -> Result<Option<Vec<u8>>, Box<Response>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    match base64::engine::general_purpose::STANDARD.decode(raw) {
        Ok(b) if !b.is_empty() && b.len() <= MAX_IMAGE_BYTES => Ok(Some(b)),
        Ok(_) => Err(Box::new(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "image",
            "image must be 1 byte to 15 MB",
        ))),
        Err(_) => Err(Box::new(api_error(
            StatusCode::BAD_REQUEST,
            "image",
            "image is not base64",
        ))),
    }
}

/// The project a box names in `X-Arbos-Project` (or the body), if it belongs to the box's user.
fn box_project(
    state: &AppState,
    user_id: &str,
    headers: &HeaderMap,
    body_project: Option<&str>,
) -> Option<crate::db::Project> {
    let id = headers
        .get("x-arbos-project")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .or(body_project)?;
    let p = state.db.project(id).ok().flatten()?;
    (p.user_id == user_id).then_some(p)
}

async fn read_meta(dir: &std::path::Path) -> Option<Item> {
    let bytes = tokio::fs::read(dir.join("meta.json")).await.ok()?;
    serde_json::from_slice::<Item>(&bytes).ok()
}

async fn write_meta(dir: &std::path::Path, item: &Item) -> Result<(), Box<Response>> {
    let meta = serde_json::to_vec_pretty(item).map_err(|e| {
        Box::new(api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "json",
            e.to_string(),
        ))
    })?;
    tokio::fs::write(dir.join("meta.json"), meta)
        .await
        .map_err(|e| {
            Box::new(api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "io",
                e.to_string(),
            ))
        })
}

/// `POST /feedback` (signed-in device, or the user's box): keep the optional screenshot and the note.
pub async fn submit(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Json(body): Json<Submit>,
) -> Response {
    let (user_id, device_id, from_box) = match &principal {
        Principal::Device { id, user_id, .. } => (user_id.clone(), Some(id.clone()), false),
        Principal::Box { user_id } => (user_id.clone(), None, true),
        Principal::Admin => {
            return api_error(
                StatusCode::FORBIDDEN,
                "forbidden",
                "a signed-in device or the user's box sends feedback",
            )
        }
    };
    // A box speaks for one of its user's projects; the item is attributed to that user.
    let box_project = if from_box {
        match box_project(&state, &user_id, &headers, body.project_id.as_deref()) {
            Some(p) => Some(p),
            None => {
                return api_error(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "name one of your user's projects in X-Arbos-Project",
                )
            }
        }
    } else {
        None
    };
    let note: String = body.note.chars().take(4000).collect();
    let mut image = match decode_image(&body.image) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };
    let mut mime = body.mime.clone();
    let mut width = body.width;
    let mut height = body.height;
    let mut platform: String = body.platform.chars().take(20).collect();
    let mut app_version: String = body.app_version.chars().take(80).collect();
    let mut os: String = body.os.chars().take(80).collect();
    let mut capture_device: Option<String> = None;
    // A device capture named by the box stands in for `image` and supplies the device facts.
    if let (true, Some(cap_id), Some(dev)) = (
        from_box,
        body.capture_id.as_deref(),
        body.capture_device_id.as_deref(),
    ) {
        if safe_id(cap_id) && safe_id(dev) {
            let dir = captures_root(&state).join(cap_id);
            if let Some(req) = read_capture_request(&dir).await {
                if req["user_id"].as_str() == Some(user_id.as_str()) {
                    if let Some(cap) = read_capture(&dir, dev).await {
                        if let Some(file) = cap["file"].as_str().filter(|f| !f.is_empty()) {
                            if let Ok(bytes) = tokio::fs::read(dir.join(file)).await {
                                image = Some(bytes);
                                mime = cap["mime"].as_str().unwrap_or("image/png").to_owned();
                                width = cap["width"].as_u64().map(|w| w as u32);
                                height = cap["height"].as_u64().map(|h| h as u32);
                            }
                        }
                        if let Some(p) = cap["platform"].as_str().filter(|s| !s.is_empty()) {
                            platform = p.chars().take(20).collect();
                        }
                        if let Some(v) = cap["app_version"].as_str().filter(|s| !s.is_empty()) {
                            app_version = v.chars().take(80).collect();
                        }
                        if let Some(o) = cap["os"].as_str().filter(|s| !s.is_empty()) {
                            os = o.chars().take(80).collect();
                        }
                        capture_device = Some(dev.to_owned());
                    }
                }
            }
        }
    }
    if image.is_none() && note.trim().is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "empty",
            "send a note, a screenshot, or both",
        );
    }
    let (file, mime, bytes) = if let Some(image) = image.as_ref() {
        let Some(ext) = ext_for(&mime) else {
            return api_error(
                StatusCode::BAD_REQUEST,
                "mime",
                format!("unsupported image type {mime}"),
            );
        };
        (format!("image.{ext}"), mime, image.len())
    } else {
        (String::new(), String::new(), 0)
    };
    if image.is_none() {
        width = None;
        height = None;
    }
    let id = ulid();
    let dir = root(&state).join(&id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string());
    }
    if let Some(image) = image {
        if let Err(e) = tokio::fs::write(dir.join(&file), &image).await {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string());
        }
    }
    let user_login = state.db.user(&user_id).ok().flatten().and_then(|u| u.login);
    let submitted_by = if from_box {
        "agent".to_owned()
    } else {
        match body.submitted_by.as_deref().map(str::trim) {
            Some("agent") => "agent".to_owned(),
            _ => "user".to_owned(),
        }
    };
    let agent_name = body
        .agent_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(80).collect::<String>())
        .or_else(|| box_project.as_ref().map(|p| p.name.clone()));
    let mut context = body.context.filter(Value::is_object);
    if let Some(dev) = capture_device {
        let ctx = context.get_or_insert_with(|| json!({}));
        if let Some(map) = ctx.as_object_mut() {
            map.insert("capture_device_id".into(), json!(dev));
        }
    }
    let item = Item {
        id: id.clone(),
        created_at: now_ms(),
        note,
        mime,
        file,
        bytes,
        app_version,
        platform,
        os,
        project_id: box_project
            .as_ref()
            .map(|p| p.id.clone())
            .or(body.project_id),
        width,
        height,
        user_id,
        user_login,
        device_id,
        submitted_by,
        agent_name,
        chat_id: body.chat_id.filter(|c| safe_id(c) || c.len() <= 80),
        rollout: body
            .rollout
            .map(|r| r.chars().take(MAX_ROLLOUT_CHARS).collect::<String>())
            .filter(|r| !r.trim().is_empty()),
        context,
        issue: None,
    };
    if let Err(resp) = write_meta(&dir, &item).await {
        return *resp;
    }
    (
        StatusCode::CREATED,
        Json(json!({ "id": item.id, "created_at": item.created_at, "has_image": !item.file.is_empty() })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// Only items created after this Unix ms.
    #[serde(default)]
    pub since: i64,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    50
}

/// `GET /admin/feedback?since=<ms>&limit=<n>`: items after `since`, oldest first.
pub async fn admin_list(
    State(state): State<AppState>,
    principal: Principal,
    Query(q): Query<ListQuery>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    let dir = root(&state);
    let mut items: Vec<Item> = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            if let Some(item) = read_meta(&entry.path()).await {
                if item.created_at > q.since {
                    items.push(item);
                }
            }
        }
    }
    items.sort_by_key(|i| i.created_at);
    items.truncate(q.limit.clamp(1, 500));
    Json(json!({ "items": items })).into_response()
}

/// `GET /admin/feedback/{id}/image`: the stored screenshot bytes.
pub async fn admin_image(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    if !safe_id(&id) {
        return api_error(StatusCode::BAD_REQUEST, "id", "bad id");
    }
    let dir = root(&state).join(&id);
    let Some(item) = read_meta(&dir).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such feedback item");
    };
    if item.file.is_empty() {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no screenshot on this item",
        );
    }
    match tokio::fs::read(dir.join(&item.file)).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, item.mime)], bytes).into_response(),
        Err(e) => api_error(StatusCode::NOT_FOUND, "not_found", e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct IssueBody {
    pub number: i64,
    #[serde(default)]
    pub url: String,
}

/// `POST /admin/feedback/{id}/issue`: the loop opened the GitHub issue; remember it on the item
/// and tell the project's box (`feedback_issue` push) so the agent can name it in the chat.
pub async fn admin_set_issue(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(body): Json<IssueBody>,
) -> Response {
    if !matches!(principal, Principal::Admin) {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "admin token required");
    }
    if !safe_id(&id) {
        return api_error(StatusCode::BAD_REQUEST, "id", "bad id");
    }
    let dir = root(&state).join(&id);
    let Some(mut item) = read_meta(&dir).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such feedback item");
    };
    let already = item.issue.is_some();
    let url: String = body.url.chars().take(300).collect();
    item.issue = Some(json!({ "number": body.number, "url": url }));
    if let Err(resp) = write_meta(&dir, &item).await {
        return *resp;
    }
    let mut pushed = false;
    if !already && item.submitted_by == "agent" {
        if let Some(project_id) = item.project_id.as_deref() {
            pushed = state
                .db
                .enqueue_push(
                    project_id,
                    "feedback_issue",
                    &json!({ "feedback_id": item.id, "number": body.number, "url": url, "chat_id": item.chat_id }),
                )
                .is_ok();
        }
    }
    Json(json!({ "id": item.id, "issue": item.issue, "pushed": pushed })).into_response()
}

// ---- screen capture round trip ----------------------------------------------------------------

#[derive(Deserialize)]
pub struct CaptureRequestBody {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    /// The device whose message started the report: its capture is preferred.
    #[serde(default)]
    pub preferred_device_id: Option<String>,
}

async fn read_capture_request(dir: &std::path::Path) -> Option<Value> {
    let bytes = tokio::fs::read(dir.join("request.json")).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn read_capture(dir: &std::path::Path, device_id: &str) -> Option<Value> {
    let bytes = tokio::fs::read(dir.join(format!("{device_id}.json")))
        .await
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Drop capture folders older than an hour (answered or not).
async fn sweep_captures(state: &AppState) {
    let root = captures_root(state);
    let Ok(mut rd) = tokio::fs::read_dir(&root).await else {
        return;
    };
    let cutoff = now_ms() - CAPTURE_TTL_MS;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let dir = entry.path();
        let stale = match read_capture_request(&dir).await {
            Some(req) => req["created_at"].as_i64().unwrap_or(0) < cutoff,
            None => true,
        };
        if stale {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }
}

/// `POST /box/feedback/capture` (box): ask the user's devices for a picture of their screen.
pub async fn box_capture_request(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Json(body): Json<CaptureRequestBody>,
) -> Response {
    let Principal::Box { user_id } = &principal else {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "box token required");
    };
    let Some(project) = box_project(&state, user_id, &headers, body.project_id.as_deref()) else {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "name one of your user's projects in X-Arbos-Project",
        );
    };
    sweep_captures(&state).await;
    let id = ulid();
    let dir = captures_root(&state).join(&id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string());
    }
    let preferred = body
        .preferred_device_id
        .filter(|d| safe_id(d) || d.len() <= 80);
    let created_at = now_ms();
    let request = json!({
        "id": id, "user_id": user_id, "project_id": project.id, "project_name": project.name,
        "chat_id": body.chat_id, "preferred_device_id": preferred, "created_at": created_at,
    });
    if let Err(e) = tokio::fs::write(dir.join("request.json"), request.to_string()).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string());
    }
    let payload = json!({
        "request_id": id, "project_id": project.id, "project_name": project.name,
        "chat_id": body.chat_id, "preferred_device_id": preferred,
        "expires_at": created_at + 45_000,
    });
    if let Err(e) = state
        .db
        .push_user(user_id, "feedback_capture", &payload, None)
    {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    let devices = state
        .db
        .devices_of_user(user_id)
        .map(|d| d.len())
        .unwrap_or(0);
    (
        StatusCode::CREATED,
        Json(json!({ "request_id": id, "devices": devices })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct CaptureBody {
    /// Base64 PNG/JPEG; empty with `skipped` set when the app could not capture.
    #[serde(default)]
    pub image: String,
    #[serde(default = "default_mime")]
    pub mime: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub os: String,
    /// Device model line, e.g. "iPhone17,1 · iOS 26.0" or "MacBookPro18,3".
    #[serde(default)]
    pub device: String,
    /// What was on screen, e.g. "chat" / "home" / "settings".
    #[serde(default)]
    pub screen: String,
    /// Why there is no image: "background", "denied", "failed: …".
    #[serde(default)]
    pub skipped: Option<String>,
}

/// `POST /feedback/capture/{id}` (device): the app's answer to a `feedback_capture` push.
pub async fn device_capture(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(body): Json<CaptureBody>,
) -> Response {
    let Principal::Device {
        id: device_id,
        user_id,
        ..
    } = &principal
    else {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a signed-in device answers a capture request",
        );
    };
    if !safe_id(&id) {
        return api_error(StatusCode::BAD_REQUEST, "id", "bad id");
    }
    let dir = captures_root(&state).join(&id);
    let Some(req) = read_capture_request(&dir).await else {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such capture request",
        );
    };
    if req["user_id"].as_str() != Some(user_id.as_str()) {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "not your capture request",
        );
    }
    let image = match decode_image(&body.image) {
        Ok(i) => i,
        Err(resp) => return *resp,
    };
    let dev_key: String = device_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(40)
        .collect();
    if dev_key.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "device", "bad device id");
    }
    let mut file = String::new();
    if let Some(bytes) = image.as_ref() {
        let Some(ext) = ext_for(&body.mime) else {
            return api_error(
                StatusCode::BAD_REQUEST,
                "mime",
                format!("unsupported image type {}", body.mime),
            );
        };
        file = format!("{dev_key}.{ext}");
        if let Err(e) = tokio::fs::write(dir.join(&file), bytes).await {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string());
        }
    }
    let capture = json!({
        "device_id": device_id, "file": file, "mime": if file.is_empty() { "" } else { body.mime.as_str() },
        "bytes": image.as_ref().map(Vec::len).unwrap_or(0),
        "width": if file.is_empty() { None } else { body.width }, "height": if file.is_empty() { None } else { body.height },
        "platform": body.platform.chars().take(20).collect::<String>(),
        "app_version": body.app_version.chars().take(80).collect::<String>(),
        "os": body.os.chars().take(80).collect::<String>(),
        "device": body.device.chars().take(120).collect::<String>(),
        "screen": body.screen.chars().take(80).collect::<String>(),
        "skipped": body.skipped.map(|s| s.chars().take(200).collect::<String>()),
        "at": now_ms(),
    });
    if let Err(e) = tokio::fs::write(dir.join(format!("{dev_key}.json")), capture.to_string()).await
    {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string());
    }
    Json(json!({ "request_id": id, "stored": !file.is_empty() })).into_response()
}

/// `GET /box/feedback/capture/{id}` (box): what the devices answered so far.
pub async fn box_capture_status(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    let Principal::Box { user_id } = &principal else {
        return api_error(StatusCode::FORBIDDEN, "forbidden", "box token required");
    };
    if !safe_id(&id) {
        return api_error(StatusCode::BAD_REQUEST, "id", "bad id");
    }
    let dir = captures_root(&state).join(&id);
    let Some(req) = read_capture_request(&dir).await else {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such capture request",
        );
    };
    if req["user_id"].as_str() != Some(user_id.as_str()) {
        return api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "not your capture request",
        );
    }
    let mut captures: Vec<Value> = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "request.json" || !name.ends_with(".json") {
                continue;
            }
            if let Ok(bytes) = tokio::fs::read(entry.path()).await {
                if let Ok(mut v) = serde_json::from_slice::<Value>(&bytes) {
                    if let Some(map) = v.as_object_mut() {
                        let has = map
                            .get("file")
                            .and_then(Value::as_str)
                            .map(|f| !f.is_empty())
                            .unwrap_or(false);
                        map.insert("has_image".into(), json!(has));
                    }
                    captures.push(v);
                }
            }
        }
    }
    captures.sort_by_key(|c| c["at"].as_i64().unwrap_or(0));
    Json(json!({
        "request_id": id, "preferred_device_id": req["preferred_device_id"],
        "created_at": req["created_at"], "captures": captures,
    }))
    .into_response()
}
