//! Screenshot feedback from the Mac app ("Send feedback", ⌘⇧F), the desktop twin of
//! TestFlight's screenshot feedback. `POST /feedback` stores the optional image, the note, the
//! app version and the sender under `<data>/feedback/<id>/`; a note alone is enough. The
//! feedback loop on the server (`scripts/feedback`) lists new items with
//! `GET /admin/feedback?since=<ms>` and fetches each image with
//! `GET /admin/feedback/{id}/image` (admin token; 404 when none). Nothing here reaches a box.

use std::path::PathBuf;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

use arbos_core::ids::{now_ms, ulid};

use crate::auth::Principal;
use crate::signin::api_error;
use crate::state::AppState;

/// 15 MB of image, the same ceiling the Cursor agent API accepts.
const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;

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
}

fn default_mime() -> String {
    "image/png".to_owned()
}

fn default_platform() -> String {
    "mac".to_owned()
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
}

fn root(state: &AppState) -> PathBuf {
    state.cfg.data_dir.join("feedback")
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `POST /feedback` (signed-in device): keep the optional screenshot and the note.
pub async fn submit(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<Submit>,
) -> Response {
    let (user_id, device_id) = match &principal {
        Principal::Device { id, user_id, .. } => (user_id.clone(), Some(id.clone())),
        Principal::Box { .. } | Principal::Admin => {
            return api_error(
                StatusCode::FORBIDDEN,
                "forbidden",
                "a signed-in device sends feedback",
            )
        }
    };
    let note: String = body.note.chars().take(4000).collect();
    let image_raw = body.image.trim();
    let image = if image_raw.is_empty() {
        if note.trim().is_empty() {
            return api_error(
                StatusCode::BAD_REQUEST,
                "empty",
                "send a note, a screenshot, or both",
            );
        }
        None
    } else {
        match base64::engine::general_purpose::STANDARD.decode(image_raw) {
            Ok(b) if !b.is_empty() && b.len() <= MAX_IMAGE_BYTES => Some(b),
            Ok(_) => {
                return api_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "image",
                    "image must be 1 byte to 15 MB",
                )
            }
            Err(_) => return api_error(StatusCode::BAD_REQUEST, "image", "image is not base64"),
        }
    };
    let (file, mime, bytes, width, height) = if let Some(image) = image.as_ref() {
        let ext = match body.mime.as_str() {
            "image/jpeg" => "jpg",
            "image/png" => "png",
            "image/webp" => "webp",
            "image/gif" => "gif",
            other => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "mime",
                    format!("unsupported image type {other}"),
                )
            }
        };
        (
            format!("image.{ext}"),
            body.mime,
            image.len(),
            body.width,
            body.height,
        )
    } else {
        (String::new(), String::new(), 0, None, None)
    };
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
    let item = Item {
        id: id.clone(),
        created_at: now_ms(),
        note,
        mime,
        file,
        bytes,
        app_version: body.app_version.chars().take(80).collect(),
        platform: body.platform.chars().take(20).collect(),
        os: body.os.chars().take(80).collect(),
        project_id: body.project_id,
        width,
        height,
        user_id,
        user_login,
        device_id,
    };
    let meta = match serde_json::to_vec_pretty(&item) {
        Ok(m) => m,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "json", e.to_string()),
    };
    if let Err(e) = tokio::fs::write(dir.join("meta.json"), meta).await {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string());
    }
    (
        StatusCode::CREATED,
        Json(json!({ "id": item.id, "created_at": item.created_at })),
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
            let Ok(bytes) = tokio::fs::read(entry.path().join("meta.json")).await else {
                continue;
            };
            if let Ok(item) = serde_json::from_slice::<Item>(&bytes) {
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
    let Ok(meta) = tokio::fs::read(dir.join("meta.json")).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "no such feedback item");
    };
    let Ok(item) = serde_json::from_slice::<Item>(&meta) else {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "json", "bad meta.json");
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
