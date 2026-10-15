//! Bearer-token principals (D35): paired devices (each belongs to a user), boxes (one per user,
//! serving every project of that user), the admin token.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;

use arbos_core::auth::hash;

use crate::state::AppState;

#[derive(Debug, Clone)]
pub enum Principal {
    Device {
        id: String,
        token: String,
        user_id: String,
    },
    Box {
        user_id: String,
    },
    Admin,
}

impl Principal {
    pub fn device_name(&self) -> String {
        match self {
            Principal::Device { id, .. } => format!("dev:{id}"),
            Principal::Box { user_id } => format!("box:{user_id}"),
            Principal::Admin => "admin".to_owned(),
        }
    }

    /// A project is reachable by the admin, by every device of its user, and by its user's box.
    /// (Devices of other users are refused once more than one user exists on a relay.)
    pub fn can_access_project(&self, project_user_id: &str) -> bool {
        match self {
            Principal::Admin => true,
            Principal::Device { user_id, .. } | Principal::Box { user_id } => {
                user_id == project_user_id
            }
        }
    }

    /// The user this principal acts for (devices and boxes); the admin has none.
    pub fn user_id(&self) -> Option<&str> {
        match self {
            Principal::Device { user_id, .. } | Principal::Box { user_id } => Some(user_id),
            Principal::Admin => None,
        }
    }

    pub fn is_box(&self) -> bool {
        matches!(self, Principal::Box { .. })
    }

    /// The device's own bearer token (for URLs the relay hands back to that device only).
    pub fn raw_token(&self) -> Option<&str> {
        match self {
            Principal::Device { token, .. } => Some(token),
            Principal::Box { .. } | Principal::Admin => None,
        }
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
        let user_id = state
            .db
            .device_user(&id)
            .ok()
            .flatten()
            .unwrap_or_else(|| "u_default".to_owned());
        return Some(Principal::Device {
            id,
            token: token.to_owned(),
            user_id,
        });
    }
    if let Ok(Some(user_id)) = state.db.user_by_box_token_hash(&h) {
        let _ = state.db.touch_box(&user_id);
        return Some(Principal::Box { user_id });
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
