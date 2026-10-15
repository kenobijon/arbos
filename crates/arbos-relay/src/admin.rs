//! Admin surface for self-serve accounts (admin token only): users with their credits, grants
//! and caps, and the global kill switch. `arbosctl admin …` is the CLI over these.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use arbos_core::ids::{now_ms, ulid};

use crate::auth::Principal;
use crate::db_accounts::KillSwitch;
use crate::signin::api_error;
use crate::state::AppState;

fn admin_only(principal: &Principal) -> Result<(), Box<Response>> {
    match principal {
        Principal::Admin => Ok(()),
        Principal::Device { .. } | Principal::Box { .. } => Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin token required",
        ))),
    }
}

/// `GET /admin/users`: every account with balance, caps, today's spend, and its machine.
pub async fn list_users(State(state): State<AppState>, principal: Principal) -> Response {
    if let Err(r) = admin_only(&principal) {
        return *r;
    }
    let users = match state.db.list_users() {
        Ok(u) => u,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    };
    let mut out = Vec::with_capacity(users.len());
    for u in users {
        let (bs, since) = state.box_state(&u.id).await;
        let sb = state.db.user_box(&u.id).ok().flatten();
        let mut v = crate::credits::summary(&state, &u.id);
        v["id"] = json!(u.id);
        v["login"] = json!(u.login);
        v["github_id"] = json!(u.github_id);
        v["test"] = json!(u.test);
        v["created_at"] = json!(u.created_at);
        v["last_login_at"] = json!(u.last_login_at);
        v["machine"] = json!({
            "exists": sb.is_some(),
            "state": bs,
            "since": since,
            "sandbox_id": sb.as_ref().and_then(|s| s.sandbox_id.clone()),
        });
        out.push(v);
    }
    Json(out).into_response()
}

#[derive(Deserialize)]
pub struct CreditsBody {
    /// Add this many USD to the balance.
    #[serde(default)]
    pub grant_usd: Option<f64>,
    /// Daily cap; present-and-null means unlimited.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub daily_cap_usd: Option<Option<f64>>,
    #[serde(default)]
    pub allow_negative: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
}

fn deserialize_double_option<'de, D>(d: D) -> Result<Option<Option<f64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<f64>::deserialize(d).map(Some)
}

/// `PUT /admin/users/{id}/credits {grant_usd?, daily_cap_usd?: number|null, allow_negative?}`.
pub async fn set_credits(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<CreditsBody>,
) -> Response {
    if let Err(r) = admin_only(&principal) {
        return *r;
    }
    if let Some(usd) = b.grant_usd {
        if let Err(e) = state.db.grant_credits(
            &id,
            usd,
            b.note.as_deref().unwrap_or("admin grant"),
            "admin",
        ) {
            return api_error(StatusCode::BAD_REQUEST, "bad_grant", e.to_string());
        }
    }
    if b.daily_cap_usd.is_some() || b.allow_negative.is_some() {
        if let Err(e) = state
            .db
            .set_user_limits(&id, b.daily_cap_usd, b.allow_negative)
        {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
        }
    }
    let mut v = crate::credits::summary(&state, &id);
    v["id"] = json!(id);
    Json(v).into_response()
}

#[derive(Deserialize)]
pub struct TestUserBody {
    pub handle: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// `POST /admin/users/test {handle, name?, avatar_url?}`: a user with no GitHub account behind
/// it (`test_<ulid>`), default credits, and a signed-in device with its token pair — for
/// recordings and two-account tests in GitHub mode.
pub async fn create_test_user(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<TestUserBody>,
) -> Response {
    if let Err(r) = admin_only(&principal) {
        return *r;
    }
    let handle = b.handle.trim().trim_start_matches('@').to_owned();
    if handle.is_empty()
        || handle.len() > 39
        || !handle
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_handle",
            "handle must look like a GitHub login",
        );
    }
    if let Ok(Some(_)) = state.db.user_by_handle(&handle) {
        return api_error(
            StatusCode::CONFLICT,
            "handle_taken",
            "a user with that handle exists",
        );
    }
    let id = format!("test_{}", ulid());
    if let Err(e) = state.db.insert_test_user(
        &id,
        &handle,
        b.name.as_deref(),
        b.avatar_url.as_deref(),
        state.cfg.default_daily_cap_usd,
    ) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    let _ = state
        .db
        .grant_credits(&id, state.cfg.default_grant_usd, "test user grant", "admin");
    let device_id = ulid();
    if let Err(e) = state.db.insert_device(
        &device_id,
        &format!("{handle} (test device)"),
        &arbos_core::auth::hash(&format!("jwt:{device_id}:{}", ulid())),
        &id,
        "github",
    ) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    match crate::signin::issue_tokens(&state, &id, &device_id) {
        Ok(mut tokens) => {
            tokens["test"] = json!(true);
            (StatusCode::CREATED, Json(tokens)).into_response()
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `DELETE /admin/users/{id}` — test users only; refused while the user still owns projects or a
/// box (delete those first so nothing is orphaned).
pub async fn delete_test_user(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = admin_only(&principal) {
        return *r;
    }
    match state.db.user(&id) {
        Ok(Some(u)) if u.test => {}
        Ok(Some(_)) => {
            return api_error(
                StatusCode::FORBIDDEN,
                "not_test_user",
                "only test users can be deleted",
            )
        }
        Ok(None) => return api_error(StatusCode::NOT_FOUND, "not_found", "no such user"),
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
    if !state
        .db
        .projects_of_user(&id)
        .unwrap_or_default()
        .is_empty()
    {
        return api_error(
            StatusCode::CONFLICT,
            "has_projects",
            "delete the user's projects first",
        );
    }
    if state.db.user_box(&id).ok().flatten().is_some() {
        return api_error(
            StatusCode::CONFLICT,
            "has_box",
            "stop and delete the user's box first",
        );
    }
    match state.db.delete_test_user(&id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(
            StatusCode::FORBIDDEN,
            "not_test_user",
            "only test users can be deleted",
        ),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `GET /admin/killswitch`.
pub async fn get_kill_switch(State(state): State<AppState>, principal: Principal) -> Response {
    if let Err(r) = admin_only(&principal) {
        return *r;
    }
    Json(json!({
        "kill_switch": crate::credits::kill_switch(&state),
        "global_daily_cap_usd": state.cfg.global_daily_cap_usd,
        "spent_today_usd": state.db.debited_since_all(crate::proxy::start_of_utc_day_ms()).unwrap_or(0.0),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct KillSwitchBody {
    pub on: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `PUT /admin/killswitch {on, reason?}`: on = the relay refuses every spend (503 `read_only`).
pub async fn set_kill_switch(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<KillSwitchBody>,
) -> Response {
    if let Err(r) = admin_only(&principal) {
        return *r;
    }
    let ks = KillSwitch {
        on: b.on,
        reason: if b.on {
            Some(
                b.reason
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "switched off by the operator".to_owned()),
            )
        } else {
            None
        },
        by: Some("admin".to_owned()),
        at: now_ms(),
        auto: false,
    };
    if let Err(e) = state.db.set_kill_switch(&ks) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    tracing::warn!(on = b.on, reason = ?ks.reason, "kill switch set by admin");
    Json(json!({ "kill_switch": ks })).into_response()
}
