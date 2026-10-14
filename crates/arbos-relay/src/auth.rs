//! Bearer-token principals (D35): paired devices, boxes (one project each), the admin token.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;

use arbos_core::auth::hash;

use crate::state::AppState;

#[derive(Debug, Clone)]
pub enum Principal {
    Device { id: String },
    Box { project_id: String },
    Admin,
}

impl Principal {
    pub fn device_name(&self) -> String {
        match self {
            Principal::Device { id } => format!("dev:{id}"),
            Principal::Box { project_id } => format!("box:{project_id}"),
            Principal::Admin => "admin".to_owned(),
        }
    }

    pub fn can_access_project(&self, project_id: &str) -> bool {
        match self {
            Principal::Device { .. } | Principal::Admin => true,
            Principal::Box { project_id: p } => p == project_id,
        }
    }

    pub fn is_box(&self) -> bool {
        matches!(self, Principal::Box { .. })
    }
}

/// Resolve a raw token string to a principal.
pub fn resolve(state: &AppState, token: &str) -> Option<Principal> {
    if token.is_empty() {
        return None;
    }
    if let Some(admin) = &state.cfg.admin_token {
        if token == admin {
            return Some(Principal::Admin);
        }
    }
    let h = hash(token);
    if let Ok(Some(id)) = state.db.device_by_token_hash(&h) {
        return Some(Principal::Device { id });
    }
    if let Ok(Some(project_id)) = state.db.project_by_box_token_hash(&h) {
        let _ = state.db.touch_sandbox(&project_id);
        return Some(Principal::Box { project_id });
    }
    None
}

/// Token from `Authorization: Bearer …` or `?token=` (WebSocket dials, crdt-sync §10.2).
pub fn token_from_parts(parts: &Parts) -> Option<String> {
    if let Some(v) = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return Some(t.trim().to_owned());
        }
    }
    let q = parts.uri.query()?;
    q.split('&')
        .find_map(|kv| kv.strip_prefix("token=").map(|t| t.to_owned()))
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let tok = token_from_parts(parts).ok_or((StatusCode::UNAUTHORIZED, "missing token"))?;
        resolve(state, &tok).ok_or((StatusCode::UNAUTHORIZED, "invalid token"))
    }
}
