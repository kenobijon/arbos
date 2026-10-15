//! Blob mirror (crdt-sync §6.4): `PUT/GET /blob/{projectId}/{key}`. Files on disk,
//! keys are sha256 hex or ULIDs; path traversal is refused.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;

use crate::auth::Principal;
use crate::state::AppState;

fn safe_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 128
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn blob_path(state: &AppState, project_id: &str, key: &str) -> std::path::PathBuf {
    state.cfg.data_dir.join("blobs").join(project_id).join(key)
}

pub async fn put(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !state.allows(&principal, &project_id) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !safe_key(&key) || !safe_key(&project_id) {
        return (StatusCode::BAD_REQUEST, "bad key").into_response();
    }
    let p = blob_path(&state, &project_id, &key);
    if let Some(parent) = p.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&p, &body).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let mime = headers.get("content-type").and_then(|v| v.to_str().ok());
    let _ = state
        .db
        .record_blob(&project_id, &key, body.len() as i64, mime);
    if let Some(m) = mime {
        let _ = tokio::fs::write(p.with_extension("mime"), m).await;
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn get(
    State(state): State<AppState>,
    principal: Principal,
    Path((project_id, key)): Path<(String, String)>,
) -> Response {
    if !state.allows(&principal, &project_id) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !safe_key(&key) || !safe_key(&project_id) {
        return (StatusCode::BAD_REQUEST, "bad key").into_response();
    }
    let p = blob_path(&state, &project_id, &key);
    let Ok(f) = tokio::fs::File::open(&p).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = tokio::fs::read_to_string(p.with_extension("mime"))
        .await
        .unwrap_or_else(|_| "application/octet-stream".to_owned());
    let stream = tokio_util::io::ReaderStream::new(f);
    ([("content-type", mime)], Body::from_stream(stream)).into_response()
}

/// `GET /bootstrap/{file}` — dev-image bootstrap artifacts (box tokens only; see image/Dockerfile.dev-inline).
pub async fn bootstrap(
    State(state): State<AppState>,
    principal: Principal,
    Path(file): Path<String>,
) -> Response {
    if !principal.is_box() && !matches!(principal, Principal::Admin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(dir) = &state.cfg.bootstrap_dir else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !safe_key(&file) {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let Ok(f) = tokio::fs::File::open(dir.join(&file)).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let stream = tokio_util::io::ReaderStream::new(f);
    (
        [("content-type", "application/octet-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}
