//! Admin surface for self-serve accounts (admin token only): users with their credits, grants
//! and caps, and the global kill switch. `arbosctl admin …` is the CLI over these.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use arbos_core::ids::now_ms;

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
