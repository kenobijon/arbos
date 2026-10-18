//! Connected vaults (D83 §3): secrets that live elsewhere and are used by reference. Phase 1
//! ships the hook points only; Phase 2 adds the 1Password provider (`op://` resolved on the
//! relay with the pinned `op` CLI, vault-level Access, per-item host pinning).

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{json, Value};

use crate::db_secrets::{Access, SecretRequest};
use crate::secrets::{err, ApiErr};
use crate::state::AppState;

/// `GET /secrets` → `vaults[]`.
pub(crate) fn list_json(_state: &AppState, _user_id: &str) -> Vec<Value> {
    Vec::new()
}

/// One vault-level access as an item of the agent profile page.
pub(crate) fn access_item_json(_state: &AppState, _owner: &str, _a: &Access) -> Option<Value> {
    None
}

/// Items of connected vaults this project may use (names only).
pub(crate) fn box_items(_state: &AppState, _user_id: &str, _project_id: &str) -> Vec<Value> {
    Vec::new()
}

/// A card decision on an `op://` request.
pub(crate) async fn decide_vault_request(
    _state: &AppState,
    _user_id: &str,
    _rq: &SecretRequest,
    _status: &str,
) -> Result<Option<Access>, ApiErr> {
    Ok(None)
}

/// `POST /box/secrets/request` with an `op://` reference.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn box_request(
    _state: &AppState,
    _user_id: &str,
    _project_id: &str,
    reference: &str,
    _purpose: &str,
    _use_kind: &str,
    _host: Option<String>,
    _chat_id: &str,
) -> Result<Response, ApiErr> {
    Err(err(
        StatusCode::NOT_IMPLEMENTED,
        "provider_unavailable",
        format!("{reference}: connected vaults are not available on this relay yet"),
    ))
}

/// Providers this relay can connect (`GET /secrets` → `providers[]`).
pub(crate) fn providers(_state: &AppState) -> Vec<&'static str> {
    Vec::new()
}

pub(crate) fn admin_status(_state: &AppState) -> Value {
    json!({ "onepassword": { "available": false } })
}
