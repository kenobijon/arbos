//! Sign in with GitHub (self-serve Arbos): OAuth authorization code + PKCE, short-lived JWT
//! access tokens, rotating refresh tokens, `/me`, and the download redirect.
//!
//! Flow for a Mac or iPhone app (ASWebAuthenticationSession-compatible):
//!   1. open `GET /auth/github/start?redirect=arbos://auth/callback&device_name=…`;
//!   2. GitHub sends the browser to `GET /auth/github/callback?code&state`; the relay signs
//!      the user in and redirects to `<redirect>?code=<one-time grant>`;
//!   3. the app posts the grant to `POST /auth/token` and gets `{access_token, refresh_token,
//!      expires_in, device_id, user}`; `POST /auth/refresh` rotates both later.
//!
//! The access token is an HS256 JWT `{sub: user_id, dev: device_id, jti, iat, exp}` and goes
//! wherever the old `adv_` device token went (`Authorization: Bearer`, `?token=` on WebSockets).
//! Provider config is env only: `ARBOS_GITHUB_CLIENT_ID` / `ARBOS_GITHUB_CLIENT_SECRET` (the
//! `ARBOS_GITHUB_OAUTH_CLIENT_*` spellings work too), `ARBOS_PUBLIC_URL`, `ARBOS_JWT_KEY` (optional).

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;

use arbos_core::auth::hash;
use arbos_core::ids::{now_ms, ulid};

use crate::auth::Principal;
use crate::db_accounts::{AuthFlow, GithubIdentity};
use crate::state::{AppState, AuthMode};

pub const ACCESS_TTL_S: i64 = 3600;
const REFRESH_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
const FLOW_TTL_MS: i64 = 10 * 60 * 1000;
const GRANT_TTL_MS: i64 = 60 * 1000;
const REFRESH_PREFIX: &str = "arf_";
const DOWNLOAD_CACHE_MS: i64 = 5 * 60 * 1000;

type HmacSha256 = Hmac<Sha256>;

/// `{"error":{"type":…,"message":…}}` with a status; the shape every client branches on.
pub(crate) fn api_error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({ "error": { "type": kind, "message": message.into() } })),
    )
        .into_response()
}

/// 302 Found: the redirect browsers and ASWebAuthenticationSession expect from OAuth hops.
fn found(url: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, url.to_owned())]).into_response()
}

// ---- JWT (HS256) --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub user_id: String,
    pub device_id: String,
    pub exp: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessError {
    Expired,
    Invalid,
}

fn sign(key: &[u8; 32], data: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(data.as_bytes());
    B64URL.encode(mac.finalize().into_bytes())
}

pub fn mint_access(key: &[u8; 32], user_id: &str, device_id: &str, ttl_s: i64) -> String {
    let now = now_ms() / 1000;
    let header = B64URL.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = B64URL.encode(
        json!({ "sub": user_id, "dev": device_id, "jti": ulid(), "iat": now, "exp": now + ttl_s })
            .to_string(),
    );
    let signing_input = format!("{header}.{payload}");
    let sig = sign(key, &signing_input);
    format!("{signing_input}.{sig}")
}

/// Does this bearer look like one of our JWTs (three base64url parts)?
pub fn looks_like_jwt(token: &str) -> bool {
    token.starts_with("eyJ") && token.split('.').count() == 3
}

pub fn verify_access(key: &[u8; 32], token: &str) -> Result<Claims, AccessError> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(AccessError::Invalid);
    };
    let expected = sign(key, &format!("{h}.{p}"));
    let given = B64URL.decode(s).map_err(|_| AccessError::Invalid)?;
    let expected_bytes = B64URL.decode(&expected).map_err(|_| AccessError::Invalid)?;
    if given.len() != expected_bytes.len()
        || given
            .iter()
            .zip(expected_bytes.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            != 0
    {
        return Err(AccessError::Invalid);
    }
    let payload: Value =
        serde_json::from_slice(&B64URL.decode(p).map_err(|_| AccessError::Invalid)?)
            .map_err(|_| AccessError::Invalid)?;
    let exp = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or(AccessError::Invalid)?;
    let claims = Claims {
        user_id: payload
            .get("sub")
            .and_then(Value::as_str)
            .ok_or(AccessError::Invalid)?
            .to_owned(),
        device_id: payload
            .get("dev")
            .and_then(Value::as_str)
            .ok_or(AccessError::Invalid)?
            .to_owned(),
        exp,
    };
    if exp * 1000 <= now_ms() {
        return Err(AccessError::Expired);
    }
    Ok(claims)
}

// ---- token issue --------------------------------------------------------------------------------

fn user_json(state: &AppState, user_id: &str) -> Value {
    match state.db.user(user_id).ok().flatten() {
        Some(u) => json!({
            "id": u.id, "github_id": u.github_id, "login": u.login, "avatar_url": u.avatar_url,
            "name": u.name, "created_at": u.created_at, "last_login_at": u.last_login_at,
        }),
        None => json!({ "id": user_id }),
    }
}

/// Access + refresh pair for a device. The refresh token is stored hashed, bound to the device.
pub(crate) fn issue_tokens(
    state: &AppState,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<Value> {
    let access = mint_access(&state.jwt_key, user_id, device_id, ACCESS_TTL_S);
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let refresh = format!("{REFRESH_PREFIX}{}", hex::encode(raw));
    state
        .db
        .insert_refresh(&hash(&refresh), device_id, user_id, REFRESH_TTL_MS)?;
    let _ = state.db.touch_device(device_id);
    Ok(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS_TTL_S,
        "refresh_token": refresh,
        "device_id": device_id,
        "user": user_json(state, user_id),
    }))
}

// ---- GitHub OAuth -------------------------------------------------------------------------------

fn github_mode(state: &AppState) -> Result<(String, String), Box<Response>> {
    if state.cfg.auth_mode != AuthMode::Github {
        return Err(Box::new(api_error(
            StatusCode::CONFLICT,
            "auth_mode",
            "this relay uses pairing codes (ARBOS_AUTH_MODE=paircode); GitHub sign-in is off",
        )));
    }
    match (&state.cfg.github_client_id, &state.cfg.github_client_secret) {
        (Some(id), Some(secret)) => Ok((id.clone(), secret.clone())),
        _ => Err(Box::new(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "oauth_unconfigured",
            "ARBOS_GITHUB_CLIENT_ID / ARBOS_GITHUB_CLIENT_SECRET are not set",
        ))),
    }
}

fn callback_url(state: &AppState) -> String {
    format!(
        "{}/auth/github/callback",
        state.cfg.public_url.trim_end_matches('/')
    )
}

/// Where the relay may send the browser after sign-in: the apps' custom scheme, a loopback
/// listener, the relay's own origin, or an operator-listed prefix.
pub(crate) fn redirect_allowed(state: &AppState, redirect: &str) -> bool {
    if redirect == "arbos://auth/callback" || redirect.starts_with("arbos://auth/callback?") {
        return true;
    }
    let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|p| redirect.starts_with(p));
    if loopback {
        return true;
    }
    let own = state.cfg.public_url.trim_end_matches('/');
    if redirect == own || redirect.starts_with(&format!("{own}/")) {
        return true;
    }
    state
        .cfg
        .auth_redirects
        .iter()
        .any(|p| !p.is_empty() && redirect.starts_with(p))
}

fn with_query(redirect: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = redirect.to_owned();
    for (i, (k, v)) in pairs.iter().enumerate() {
        out.push(if i == 0 && !redirect.contains('?') {
            '?'
        } else {
            '&'
        });
        out.push_str(k);
        out.push('=');
        out.push_str(&urlencode(v));
    }
    out
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize)]
pub struct StartQuery {
    pub redirect: String,
    #[serde(default)]
    pub device_name: Option<String>,
}

/// `GET /auth/github/start?redirect=…&device_name=…` → 302 to GitHub.
pub async fn github_start(State(state): State<AppState>, Query(q): Query<StartQuery>) -> Response {
    let (client_id, _) = match github_mode(&state) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    if !redirect_allowed(&state, &q.redirect) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "bad_redirect",
            "redirect must be arbos://auth/callback, a loopback URL, or this relay's origin",
        );
    }
    let (verifier, challenge) = crate::connectors::pkce_pair();
    let mut raw = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut raw);
    let flow = AuthFlow {
        state: B64URL.encode(raw),
        code_verifier: verifier,
        redirect: q.redirect,
        device_name: q
            .device_name
            .map(|s| s.trim().chars().take(80).collect::<String>())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "device".to_owned()),
        invite: None,
    };
    if let Err(e) = state.db.insert_auth_flow(&flow, FLOW_TTL_MS) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    let url = format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=read%3Auser&state={}&code_challenge={}&code_challenge_method=S256",
        urlencode(&client_id),
        urlencode(&callback_url(&state)),
        urlencode(&flow.state),
        urlencode(&challenge),
    );
    found(&url)
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

async fn github_identity(
    state: &AppState,
    client_id: &str,
    client_secret: &str,
    code: &str,
    verifier: &str,
) -> anyhow::Result<GithubIdentity> {
    let token: Value = state
        .http
        .post("https://github.com/login/oauth/access_token")
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("redirect_uri", &callback_url(state)),
            ("code_verifier", verifier),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let access = token
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "github token exchange: {}",
                token
                    .get("error_description")
                    .or(token.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or("no access_token")
            )
        })?;
    let user: Value = state
        .http
        .get("https://api.github.com/user")
        .bearer_auth(access)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header(header::USER_AGENT, "arbos-relay")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(GithubIdentity {
        id: user
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("github user has no id"))?,
        login: user
            .get("login")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        avatar_url: user
            .get("avatar_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        name: user.get("name").and_then(Value::as_str).map(str::to_owned),
    })
}

/// `GET /auth/github/callback?code&state` (GitHub → relay). Ends with a redirect to the app:
/// `<redirect>?code=<grant>` or `<redirect>?error=…&message=…`.
pub async fn github_callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let (client_id, client_secret) = match github_mode(&state) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let Some(flow_state) = q.state.as_deref() else {
        return api_error(StatusCode::BAD_REQUEST, "bad_state", "missing state");
    };
    let flow = match state.db.take_auth_flow(flow_state) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "bad_state",
                "unknown or expired sign-in attempt; start again",
            )
        }
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    };
    let back = |kind: &str, message: &str| -> Response {
        found(&with_query(
            &flow.redirect,
            &[("error", kind), ("message", message)],
        ))
    };
    if let Some(err) = q.error.as_deref() {
        let msg = q.error_description.as_deref().unwrap_or(err);
        return back(
            if err == "access_denied" {
                "oauth_denied"
            } else {
                "oauth_failed"
            },
            msg,
        );
    }
    let Some(code) = q.code.as_deref() else {
        return back("oauth_failed", "GitHub sent no code");
    };
    let gh = match github_identity(
        &state,
        &client_id,
        &client_secret,
        code,
        &flow.code_verifier,
    )
    .await
    {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "github sign-in failed");
            return back("oauth_failed", &e.to_string());
        }
    };
    let (user, created) = match state
        .db
        .upsert_github_user(&gh, state.cfg.default_daily_cap_usd)
    {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    };
    if created {
        if let Err(e) = state.db.grant_credits(
            &user.id,
            state.cfg.default_grant_usd,
            "welcome grant",
            "relay",
        ) {
            tracing::warn!(error = %e, user = %user.id, "welcome grant failed");
        }
        tracing::info!(user = %user.id, login = %gh.login, "new account");
    }
    let device_id = ulid();
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    if let Err(e) = state.db.insert_device(
        &device_id,
        &flow.device_name,
        &hash(&format!("jwt:{device_id}:{}", hex::encode(salt))),
        &user.id,
        "github",
    ) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    let mut raw = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut raw);
    let grant = B64URL.encode(raw);
    if let Err(e) = state
        .db
        .insert_auth_grant(&hash(&grant), &device_id, &user.id, GRANT_TTL_MS)
    {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    found(&with_query(&flow.redirect, &[("code", &grant)]))
}

#[derive(Deserialize)]
pub struct TokenBody {
    pub grant_code: String,
}

/// `POST /auth/token {"grant_code"}` → tokens. One use, 60 s.
pub async fn token(State(state): State<AppState>, Json(b): Json<TokenBody>) -> Response {
    match state.db.take_auth_grant(&hash(b.grant_code.trim())) {
        Ok(Some((device_id, user_id))) => match issue_tokens(&state, &user_id, &device_id) {
            Ok(v) => Json(v).into_response(),
            Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
        },
        Ok(None) => api_error(
            StatusCode::UNAUTHORIZED,
            "bad_grant",
            "unknown, used, or expired grant code",
        ),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct RefreshBody {
    pub refresh_token: String,
}

/// `POST /auth/refresh {"refresh_token"}` → new pair; the old refresh token is dead. A token
/// that was already rotated is a replay: the whole device is revoked.
pub async fn refresh(State(state): State<AppState>, Json(b): Json<RefreshBody>) -> Response {
    let h = hash(b.refresh_token.trim());
    let row = match state.db.refresh_row(&h) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "bad_refresh",
                "unknown refresh token",
            )
        }
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    };
    if row.revoked_at.is_some() {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "bad_refresh",
            "refresh token revoked",
        );
    }
    if row.rotated_at.is_some() {
        tracing::warn!(device = %row.device_id, "refresh token replayed; revoking the device");
        let _ = state.db.revoke_device_fully(&row.device_id);
        return api_error(
            StatusCode::UNAUTHORIZED,
            "bad_refresh",
            "refresh token was already used; sign in again",
        );
    }
    if row.expires_at <= now_ms() {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "bad_refresh",
            "refresh token expired",
        );
    }
    match state.db.device_status(&row.device_id) {
        Ok(Some((_, false))) => {}
        _ => return api_error(StatusCode::UNAUTHORIZED, "bad_refresh", "device revoked"),
    }
    if let Err(e) = state.db.mark_refresh_rotated(&h) {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string());
    }
    match issue_tokens(&state, &row.user_id, &row.device_id) {
        Ok(v) => Json(v).into_response(),
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    }
}

/// `POST /auth/logout` (Bearer): this device and its refresh tokens are revoked.
pub async fn logout(State(state): State<AppState>, principal: Principal) -> Response {
    match principal {
        Principal::Device { id, .. } => match state.db.revoke_device_fully(&id) {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
        },
        Principal::Box { .. } | Principal::Admin => {
            api_error(StatusCode::FORBIDDEN, "forbidden", "devices only")
        }
    }
}

/// `GET /me`: who am I, what can I spend, where is my machine.
pub async fn me(State(state): State<AppState>, principal: Principal) -> Response {
    let (user_id, device) = match &principal {
        Principal::Device { id, user_id, .. } => (user_id.clone(), Some(id.clone())),
        Principal::Box { user_id } => (user_id.clone(), None),
        Principal::Admin => {
            return api_error(StatusCode::FORBIDDEN, "forbidden", "device token required")
        }
    };
    let credits = match state
        .db
        .credits(&user_id, crate::proxy::start_of_utc_day_ms())
    {
        Ok(c) => c,
        Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, "db", e.to_string()),
    };
    let ks = state.db.kill_switch().unwrap_or_default();
    let (bs, since) = state.box_state(&user_id).await;
    let sb = state.db.user_box(&user_id).ok().flatten();
    let device_json = device.and_then(|id| {
        state
            .db
            .devices_of_user(&user_id)
            .ok()?
            .into_iter()
            .find(|d| d.get("id").and_then(Value::as_str) == Some(id.as_str()))
    });
    Json(json!({
        "user": user_json(&state, &user_id),
        "device": device_json,
        "credits": credits,
        "limits": {
            "read_only": ks.on,
            "read_only_reason": ks.reason,
            "signup": "open",
            "auth_mode": state.cfg.auth_mode.as_str(),
        },
        "machine": {
            "exists": sb.is_some(),
            "state": bs,
            "since": since,
            "provider": sb.as_ref().map(|s| s.provider.clone()),
            "sandbox_id": sb.as_ref().and_then(|s| s.sandbox_id.clone()),
        },
    }))
    .into_response()
}

/// `GET /download/mac` → 302 to the newest edge DMG (one stable link for the landing page).
/// The version comes from the rolling `edge` release's `latest.txt`, cached for five minutes.
pub async fn download_mac(State(state): State<AppState>) -> Response {
    let repo = state.cfg.releases_repo.trim_matches('/');
    let now = now_ms();
    if let Some((at, url)) = state.download_cache.lock().await.clone() {
        if now - at < DOWNLOAD_CACHE_MS {
            return found(&url);
        }
    }
    let latest_url = format!("https://github.com/{repo}/releases/download/edge/latest.txt");
    let version = match state.http.get(&latest_url).send().await {
        Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default().trim().to_owned(),
        Ok(r) => {
            tracing::warn!(status = %r.status(), "latest.txt fetch failed");
            String::new()
        }
        Err(e) => {
            tracing::warn!(error = %e, "latest.txt fetch failed");
            String::new()
        }
    };
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return found(&format!("https://github.com/{repo}/releases"));
    }
    let url = format!(
        "https://github.com/{repo}/releases/download/edge-{version}/Arbos-{version}-macos-universal.dmg"
    );
    *state.download_cache.lock().await = Some((now, url.clone()));
    found(&url)
}
