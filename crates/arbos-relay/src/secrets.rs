//! Secrets (D83, `docs/spec-secrets.md`): named values an agent may use but never read.
//!
//! Three words: a **Secret** (name, note, write-only value, hosts), **Access** (one switch per
//! secret × agent, with a note), and **Pause all secrets** (the kill switch). The relay is the
//! authority at the moment of use: `/egress` swaps a placeholder for the value only when Access
//! is on, the host is allowed and the user is not paused; values live as ciphertext under a
//! per-user DEK wrapped by the relay key (§7.2), are decrypted inside one request and dropped,
//! and are redacted from every reply. Every event lands in the append-only audit log (§7.4).
//!
//! The F8 `/vault/*` endpoints in `vault.rs` are thin shims over this model for one release.

use std::collections::{BTreeMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};

use crate::auth::Principal;
use crate::connectors::crypto::Dek;
use crate::db_secrets::{
    Access, AuditEntry, AuditQuery, Secret, SecretRequest, SecretVersion, REQUEST_PENDING_MS,
};
use crate::signin::api_error;
use crate::state::AppState;
use crate::vault::{host_allowed, PLACEHOLDER_SCHEME};

pub const KINDS: [&str; 3] = ["key", "login", "text"];
pub const USES: [&str; 3] = ["header", "env", "browser_fill"];
/// Window after a decision in which `browser_fill` may fetch login fields.
pub const FILL_WINDOW_MS: i64 = 10 * 60 * 1000;
/// Activity shown in the UI (kept forever, D76 Q6).
pub const ACTIVITY_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// `secret_expiring` goes out this long before `expires_at`.
const EXPIRING_NOTICE_MS: i64 = 3 * 24 * 60 * 60 * 1000;
pub const ARBOS_VAULT_NAME: &str = "Arbos";

/// A refused call: `{"error":{"type","message"}}` with its status. Small, so `Result` stays
/// cheap to pass around (clippy `result_large_err`).
#[derive(Debug)]
pub struct ApiErr(pub StatusCode, pub String, pub String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        api_error(self.0, &self.1, self.2)
    }
}

pub(crate) type ApiResult = Result<Response, ApiErr>;

pub(crate) fn err(status: StatusCode, kind: &str, msg: impl Into<String>) -> ApiErr {
    ApiErr(status, kind.to_owned(), msg.into())
}

pub(crate) fn internal(e: impl std::fmt::Display) -> ApiErr {
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
}

fn not_found() -> ApiErr {
    err(StatusCode::NOT_FOUND, "not_found", "no such secret")
}

/// The user a device call acts for; boxes and the admin token have no Settings.
pub(crate) fn device_user(principal: &Principal) -> Result<String, ApiErr> {
    match principal {
        Principal::Device { user_id, .. } => Ok(user_id.clone()),
        Principal::Box { .. } | Principal::Admin => Err(err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "device token required",
        )),
    }
}

/// `(user, project)` for a box call: `X-Arbos-Project` must belong to the box's user.
pub(crate) fn box_project(
    state: &AppState,
    principal: &Principal,
    headers: &HeaderMap,
) -> Result<(String, String), ApiErr> {
    let Principal::Box { user_id } = principal else {
        return Err(err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "box token required (the agent asks, the user decides)",
        ));
    };
    let id = headers
        .get("x-arbos-project")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "X-Arbos-Project header required",
            )
        })?;
    let p = state
        .db
        .project(id)
        .map_err(internal)?
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "bad_request", "no such project"))?;
    if p.user_id != *user_id {
        return Err(err(StatusCode::FORBIDDEN, "not_owner", "not your project"));
    }
    Ok((user_id.clone(), p.id))
}

// ---- names, env names, hosts -------------------------------------------------------------------

/// `stripe live` → `STRIPE_LIVE`; a leading digit gets a `_`.
pub fn env_from_name(name: &str) -> String {
    let mut s = String::new();
    for ch in name.trim().chars() {
        s.push(if ch.is_ascii_alphanumeric() {
            ch.to_ascii_uppercase()
        } else {
            '_'
        });
    }
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let s = s.trim_matches('_').to_owned();
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("_{s}")
    } else {
        s
    }
}

/// 1–64 chars: letters, digits, space, `_`, `-`, `.`; must leave a non-empty env name.
pub fn validate_name(name: &str) -> Result<(String, String), ApiErr> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '));
    let env = env_from_name(name);
    if !ok || env.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "bad_name",
            "Letters, numbers and _ (1–64 characters).",
        ));
    }
    Ok((name.to_owned(), env))
}

pub fn clean_hosts(hosts: &[String]) -> Result<Vec<String>, ApiErr> {
    let mut out: Vec<String> = Vec::new();
    for h in hosts {
        let h = h.trim().to_ascii_lowercase();
        let h = h
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_owned();
        if h.is_empty() {
            continue;
        }
        let body = h.strip_prefix("*.").unwrap_or(&h);
        if body.is_empty()
            || !body
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "bad_host",
                format!("{h}: a host looks like api.example.com or *.example.com"),
            ));
        }
        if !out.contains(&h) {
            out.push(h);
        }
    }
    Ok(out)
}

pub fn placeholder(env: &str, field: &str) -> String {
    format!("{PLACEHOLDER_SCHEME}{env}/{field}")
}

/// Field names per kind; `login` carries the sign-in URL as metadata, not a secret.
fn fields_for_kind(kind: &str, b: &Value) -> Result<serde_json::Map<String, Value>, ApiErr> {
    let mut out = serde_json::Map::new();
    let get = |k: &str| b.get(k).and_then(Value::as_str).map(str::to_owned);
    match kind {
        "key" | "text" => {
            let v = get("value")
                .or_else(|| get("text"))
                .filter(|v| !v.is_empty())
                .ok_or_else(|| err(StatusCode::BAD_REQUEST, "bad_request", "value required"))?;
            out.insert("value".into(), Value::String(v));
        }
        "login" => {
            let f = b.get("fields").unwrap_or(b);
            let fget = |k: &str| f.get(k).and_then(Value::as_str).map(str::to_owned);
            let password = fget("password").filter(|v| !v.is_empty()).ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "login needs a password",
                )
            })?;
            out.insert("password".into(), Value::String(password));
            if let Some(u) = fget("username").filter(|v| !v.is_empty()) {
                out.insert("username".into(), Value::String(u));
            }
            if let Some(u) = fget("url").filter(|v| !v.is_empty()) {
                out.insert("url".into(), Value::String(u));
            }
        }
        _ => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "bad_request",
                format!("kind must be one of {}", KINDS.join(", ")),
            ))
        }
    }
    Ok(out)
}

// ---- key material -----------------------------------------------------------------------------

/// The user's DEK, created and wrapped on first use.
pub(crate) fn user_dek(state: &AppState, user_id: &str) -> anyhow::Result<Dek> {
    if let Some(w) = state.db.user_dek_wrapped(user_id)? {
        return state.relay_key.unwrap_dek(&w);
    }
    let dek = Dek::random();
    state
        .db
        .insert_user_dek(user_id, &state.relay_key.wrap_dek(&dek)?)?;
    // Another request may have won the insert: always read back the stored one.
    let w = state
        .db
        .user_dek_wrapped(user_id)?
        .ok_or_else(|| anyhow::anyhow!("dek vanished"))?;
    state.relay_key.unwrap_dek(&w)
}

fn aad(secret_id: &str, version: i64) -> Vec<u8> {
    format!("{secret_id}|{version}").into_bytes()
}

fn encrypt_fields(
    dek: &Dek,
    secret_id: &str,
    version: i64,
    fields: &Value,
) -> anyhow::Result<String> {
    dek.encrypt(&aad(secret_id, version), &fields.to_string())
}

/// Decrypted fields (`field → value`) of one version. Stays inside this process.
fn decrypt_fields(dek: &Dek, v: &SecretVersion) -> anyhow::Result<BTreeMap<String, String>> {
    let plain = dek.decrypt(&aad(&v.secret_id, v.version), &v.fields_enc)?;
    let val: Value = serde_json::from_str(&plain)?;
    Ok(val
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default())
}

/// Current fields plus every value still live in a previous version (for redaction).
pub(crate) struct Plain {
    pub current: BTreeMap<String, String>,
    pub all_values: Vec<String>,
}

pub(crate) fn plain_of(state: &AppState, dek: &Dek, s: &Secret) -> anyhow::Result<Plain> {
    let versions = state.db.secret_versions(&s.id)?;
    let mut current = BTreeMap::new();
    let mut all_values = Vec::new();
    for v in &versions {
        let f = decrypt_fields(dek, v)?;
        for (k, val) in &f {
            if (k == "value" || k == "password") && !all_values.contains(val) {
                all_values.push(val.clone());
            }
        }
        if v.version == s.current_version {
            current = f;
        }
    }
    Ok(Plain {
        current,
        all_values,
    })
}

/// Field names of the current version, for listings. Logins expose `username` as metadata.
fn field_names(kind: &str) -> Vec<&'static str> {
    match kind {
        "login" => vec!["username", "password"],
        _ => vec!["value"],
    }
}

// ---- audit and pushes -------------------------------------------------------------------------

pub(crate) fn audit(state: &AppState, user_id: &str, e: AuditEntry) {
    if let Err(e) = state.db.append_secret_audit(user_id, &e) {
        tracing::warn!(error = %e, "secret audit append failed");
    }
}

fn entry(action: &str, s: &Secret, by: &str) -> AuditEntry {
    AuditEntry {
        action: action.into(),
        target_kind: "secret".into(),
        target_id: s.id.clone(),
        target_name: s.env.clone(),
        by: by.into(),
        detail: json!({}),
        ..Default::default()
    }
}

fn push_box(state: &AppState, project_id: &str, kind: &str, payload: Value) {
    let _ = state.db.enqueue_push(project_id, kind, &payload);
}

fn push_user(state: &AppState, user_id: &str, kind: &str, payload: Value) {
    let _ = state.db.push_user(user_id, kind, &payload, None);
}

// ---- JSON shapes ------------------------------------------------------------------------------

fn access_json(state: &AppState, a: &Access) -> Value {
    let name = state
        .db
        .project(&a.project_id)
        .ok()
        .flatten()
        .map(|p| p.name)
        .unwrap_or_else(|| a.project_id.clone());
    json!({
        "project_id": a.project_id, "name": name, "access_id": a.id, "enabled": a.enabled,
        "note": a.note, "hosts": a.hosts,
        "scope": { "kind": a.scope_kind, "chat_id": a.scope_chat_id },
        "created_from": a.created_from, "created_at": a.created_at,
        "last_used_at": a.last_used_at, "disabled_at": a.disabled_at,
    })
}

pub(crate) fn secret_json(state: &AppState, s: &Secret) -> Value {
    let agents: Vec<Value> = state
        .db
        .accesses_of_target("secret", &s.id)
        .unwrap_or_default()
        .iter()
        .map(|a| access_json(state, a))
        .collect();
    let username = if s.kind == "login" {
        user_dek(state, &s.user_id)
            .ok()
            .and_then(|dek| plain_of(state, &dek, s).ok())
            .and_then(|p| p.current.get("username").cloned())
    } else {
        None
    };
    json!({
        "id": s.id, "name": s.name, "env": s.env, "kind": s.kind, "note": s.note, "hosts": s.hosts,
        "ask_every_time": s.ask_every_time, "expires_at": s.expires_at, "version": s.current_version,
        "created_at": s.created_at, "updated_at": s.updated_at, "last_used_at": s.last_used_at,
        "username": username, "vault_name": ARBOS_VAULT_NAME,
        "ref": placeholder(&s.env, if s.kind == "login" { "password" } else { "value" }),
        "agents": agents,
    })
}

pub(crate) fn request_json(state: &AppState, r: &SecretRequest) -> Value {
    let project_name = state
        .db
        .project(&r.project_id)
        .ok()
        .flatten()
        .map(|p| p.name);
    json!({
        "id": r.id, "project_id": r.project_id, "project_name": project_name, "chat_id": r.chat_id,
        "target": { "kind": r.target_kind, "id": r.target_id, "ref": r.target_ref },
        "name": r.target_ref, "purpose": r.purpose, "use": r.use_kind, "host": r.host,
        "status": r.status, "decided_by": r.decided_by, "created_at": r.created_at,
        "decided_at": r.decided_at, "expires_at": r.expires_at,
    })
}

pub(crate) fn activity(state: &AppState, user_id: &str, q: AuditQuery) -> Vec<Value> {
    state
        .db
        .list_audit(user_id, &q)
        .unwrap_or_default()
        .iter()
        .map(|a| {
            let project_name = a
                .project_id
                .as_deref()
                .and_then(|p| state.db.project(p).ok().flatten())
                .map(|p| p.name);
            let mut v = serde_json::to_value(a).unwrap_or(Value::Null);
            v["project_name"] = json!(project_name);
            v["name"] = json!(a.target_name);
            v
        })
        .collect()
}

// ---- the one rule: Access on, host allowed, not paused ---------------------------------------

/// Why a use was refused (strings from spec §8.8, never a value).
pub enum Refusal {
    Paused,
    NoSuch,
    NoAccess,
    TurnedOff,
    WrongChat,
    HostNotAllowed(Vec<String>),
    Expired,
    AskEveryTime,
}

impl Refusal {
    pub fn message(&self, name: &str, host: &str) -> String {
        match self {
            Refusal::Paused => "Secrets are paused by the owner.".to_owned(),
            Refusal::NoSuch => format!("{name}: no such secret — see secrets_list."),
            Refusal::NoAccess => {
                format!("{name}: no access for this agent — call secrets_request.")
            }
            Refusal::TurnedOff => format!("{name}: access was turned off by the owner."),
            Refusal::WrongChat => {
                format!("{name}: access is for another chat — call secrets_request.")
            }
            Refusal::HostNotAllowed(allowed) => {
                format!(
                    "{name}: not allowed at {host} (allowed: {}).",
                    allowed.join(", ")
                )
            }
            Refusal::Expired => format!("{name}: expired."),
            Refusal::AskEveryTime => {
                format!("{name}: asks every time — call secrets_request first.")
            }
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Refusal::Paused => "paused",
            Refusal::NoSuch => "not_found",
            Refusal::NoAccess | Refusal::TurnedOff | Refusal::WrongChat | Refusal::AskEveryTime => {
                "no_access"
            }
            Refusal::HostNotAllowed(_) => "host_not_allowed",
            Refusal::Expired => "expired",
        }
    }
}

/// A use the rule allowed, and what it was allowed by.
pub struct Allowed {
    pub access: Option<Access>,
    /// The `allowed_once` request spent by this use.
    pub once: Option<SecretRequest>,
    /// No host was known yet: pin `host` on the access and tell the owner.
    pub pin_host: bool,
}

/// Check one use of `s` by `project_id` at `host` in `chat_id`. `use_kind` is `header | env |
/// browser_fill`. Pure except for the two lookups (access, allowed_once).
pub(crate) fn decide_use(
    state: &AppState,
    s: &Secret,
    project_id: &str,
    host: &str,
    use_kind: &str,
    chat_id: Option<&str>,
) -> Result<Allowed, Refusal> {
    if s.expires_at.is_some_and(|t| t < now_ms()) {
        return Err(Refusal::Expired);
    }
    let access = state.db.access(project_id, "secret", &s.id).ok().flatten();
    let once = state
        .db
        .allowed_once_for(project_id, &s.id, Some(host), use_kind)
        .ok()
        .flatten();
    if let Some(once) = once {
        return Ok(Allowed {
            access,
            once: Some(once),
            pin_host: false,
        });
    }
    let Some(a) = access else {
        return Err(Refusal::NoAccess);
    };
    if !a.enabled {
        return Err(Refusal::TurnedOff);
    }
    if a.scope_kind == "chat" && a.scope_chat_id.as_deref() != chat_id {
        return Err(Refusal::WrongChat);
    }
    if s.ask_every_time {
        return Err(Refusal::AskEveryTime);
    }
    let mut hosts = s.hosts.clone();
    hosts.extend(a.hosts.iter().cloned());
    if hosts.is_empty() {
        return Ok(Allowed {
            access: Some(a),
            once: None,
            pin_host: !host.is_empty(),
        });
    }
    // No host yet (a request without one, a login fill): the host is checked at use.
    if !host.is_empty() && !host_allowed(host, &hosts) {
        return Err(Refusal::HostNotAllowed(hosts));
    }
    Ok(Allowed {
        access: Some(a),
        once: None,
        pin_host: false,
    })
}

/// After an allowed use: spend a one-time grant, pin a first host, touch timestamps, audit.
pub(crate) fn after_use(
    state: &AppState,
    s: &Secret,
    project_id: &str,
    host: &str,
    use_kind: &str,
    chat_id: Option<&str>,
    allowed: &Allowed,
) {
    if let Some(once) = &allowed.once {
        let _ = state.db.consume_allowed_once(&once.id);
    }
    if allowed.pin_host {
        if let Some(a) = &allowed.access {
            let mut a = a.clone();
            a.hosts.push(host.to_owned());
            let _ = state.db.upsert_access(&a);
            let agent = state
                .db
                .project(project_id)
                .ok()
                .flatten()
                .map(|p| p.name)
                .unwrap_or_else(|| "The agent".into());
            push_box(
                state,
                project_id,
                "secret_notice",
                json!({ "ref": placeholder(&s.env, "value"), "env": s.env, "host": host, "chat_id": chat_id,
                    "text": format!("{agent} used {} at {host}.", s.env) }),
            );
            audit(
                state,
                &s.user_id,
                AuditEntry {
                    project_id: Some(project_id.to_owned()),
                    chat_id: chat_id.map(str::to_owned),
                    access_id: Some(a.id.clone()),
                    host: Some(host.to_owned()),
                    detail: json!({ "pinned": true }),
                    ..entry("access_on", s, &format!("agent:{project_id}"))
                },
            );
            spawn_remount(state.clone(), s.user_id.clone());
        }
    }
    let _ = state.db.touch_secret(&s.user_id, &s.id, project_id);
    audit(
        state,
        &s.user_id,
        AuditEntry {
            project_id: Some(project_id.to_owned()),
            chat_id: chat_id.map(str::to_owned),
            request_id: allowed.once.as_ref().map(|r| r.id.clone()),
            access_id: allowed.access.as_ref().map(|a| a.id.clone()),
            use_kind: Some(use_kind.to_owned()),
            host: Some(host.to_owned()),
            ..entry("used", s, &format!("agent:{project_id}"))
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn audit_refusal(
    state: &AppState,
    user_id: &str,
    name: &str,
    secret: Option<&Secret>,
    project_id: &str,
    host: &str,
    use_kind: &str,
    why: &Refusal,
) {
    audit(
        state,
        user_id,
        AuditEntry {
            action: "refused".into(),
            target_kind: "secret".into(),
            target_id: secret.map(|s| s.id.clone()).unwrap_or_default(),
            target_name: secret
                .map(|s| s.env.clone())
                .unwrap_or_else(|| name.to_owned()),
            project_id: Some(project_id.to_owned()),
            use_kind: Some(use_kind.to_owned()),
            host: Some(host.to_owned()),
            by: format!("agent:{project_id}"),
            detail: json!({ "reason": why.kind() }),
            ..Default::default()
        },
    );
}

// ---- Daytona mirror (D28) -----------------------------------------------------------------------

/// Recompute the user's sandbox env: every secret with at least one enabled access and a known
/// host set, as Daytona Secrets (`ENV` for one field, `ENV_FIELD` for logins); `{}` while
/// paused. Best effort; the relay `/egress` path is the authority regardless.
pub(crate) async fn remount_daytona(state: &AppState, user_id: &str) -> Option<String> {
    let dt = state.daytona.as_ref()?;
    let sb = state.db.user_box(user_id).ok().flatten()?;
    if sb.provider != "daytona" {
        return None;
    }
    let sandbox_id = sb.sandbox_id?;
    let paused = state.db.secrets_settings(user_id).ok()?.paused;
    let mut mounts = Vec::new();
    if !paused {
        let dek = user_dek(state, user_id).ok()?;
        for mut s in state.db.list_secrets(user_id).ok()? {
            if s.expires_at.is_some_and(|t| t < now_ms()) {
                continue;
            }
            let accesses = state.db.accesses_of_target("secret", &s.id).ok()?;
            let mut hosts = s.hosts.clone();
            let mut any_on = false;
            for a in accesses.iter().filter(|a| a.enabled) {
                any_on = true;
                hosts.extend(a.hosts.iter().cloned());
            }
            hosts.dedup();
            if !any_on || hosts.is_empty() {
                continue;
            }
            let Ok(plain) = plain_of(state, &dek, &s) else {
                continue;
            };
            let mut ids = s.daytona_secrets.as_object().cloned().unwrap_or_default();
            let mut changed = false;
            for (field, value) in &plain.current {
                if field == "url" || field == "username" {
                    continue;
                }
                let secret_name = format!("arbos-{}-{}", s.id.to_lowercase(), field);
                let outcome = match ids.get(field).and_then(Value::as_str) {
                    Some(existing) => dt
                        .update_secret(existing, value, &hosts)
                        .await
                        .map(|_| existing.to_owned()),
                    None => dt.create_secret(&secret_name, value, &hosts).await,
                };
                match outcome {
                    Ok(sid) => {
                        if ids.get(field).and_then(Value::as_str) != Some(sid.as_str()) {
                            ids.insert(field.clone(), Value::String(sid));
                            changed = true;
                        }
                        let env = if field == "value" {
                            s.env.clone()
                        } else {
                            format!("{}_{}", s.env, field.to_ascii_uppercase())
                        };
                        let mut m = serde_json::Map::new();
                        m.insert(env, Value::String(secret_name));
                        mounts.push(m);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, secret = %s.env, "daytona secret mirror failed");
                    }
                }
            }
            if changed {
                s.daytona_secrets = Value::Object(ids);
                let _ = state.db.update_secret(&s);
            }
        }
    }
    match dt.update_secrets(&sandbox_id, mounts).await {
        Ok(_) => Some("mounted".into()),
        Err(e) => {
            tracing::warn!(error = %e, "daytona updateSecrets failed");
            Some(format!("updateSecrets failed: {e}"))
        }
    }
}

pub(crate) fn spawn_remount(state: AppState, user_id: String) {
    tokio::spawn(async move {
        remount_daytona(&state, &user_id).await;
    });
}

/// Tell the box about one access change and refresh the Daytona mount.
fn after_access_change(state: &AppState, s: &Secret, a: &Access) {
    push_box(
        state,
        &a.project_id,
        "secret_access_changed",
        json!({ "access_id": a.id, "ref": placeholder(&s.env, if s.kind == "login" { "password" } else { "value" }),
            "env": s.env, "name": s.name, "kind": s.kind, "enabled": a.enabled, "hosts": s.hosts.iter().chain(a.hosts.iter()).collect::<Vec<_>>() }),
    );
    spawn_remount(state.clone(), s.user_id.clone());
}

// ---- device: list, create, read, update, value, delete -----------------------------------------

/// `GET /secrets` → `{ paused, vaults, secrets, requests_pending }`.
pub async fn list(State(state): State<AppState>, principal: Principal) -> ApiResult {
    let user_id = device_user(&principal)?;
    let settings = state.db.secrets_settings(&user_id).map_err(internal)?;
    let secrets: Vec<Value> = state
        .db
        .list_secrets(&user_id)
        .map_err(internal)?
        .iter()
        .map(|s| secret_json(&state, s))
        .collect();
    let pending = state
        .db
        .pending_secret_requests_count(&user_id)
        .unwrap_or(0);
    let recent = activity(
        &state,
        &user_id,
        AuditQuery {
            since: Some(now_ms() - ACTIVITY_WINDOW_MS),
            limit: 20,
            ..Default::default()
        },
    );
    Ok(Json(json!({
        "paused": settings.paused, "paused_at": settings.paused_at, "paused_by": settings.paused_by,
        "vaults": crate::vaults::list_json(&state, &user_id),
        "providers": crate::vaults::providers(&state),
        "secrets": secrets, "requests_pending": pending, "activity": recent,
    }))
    .into_response())
}

#[derive(Deserialize, Default)]
pub struct AgentPick {
    pub project_id: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct CreateBody {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub fields: Option<Value>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub hosts: Option<Vec<String>>,
    #[serde(default)]
    pub ask_every_time: Option<bool>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub agents: Vec<AgentPick>,
}

/// The kind a form implies when the body does not say: login fields → login, `text` → text.
fn infer_kind(b: &CreateBody) -> String {
    if let Some(k) = b.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        return match k {
            "api_key" | "secret" => "key".to_owned(),
            other => other.to_owned(),
        };
    }
    if b.fields
        .as_ref()
        .is_some_and(|f| f.get("password").is_some())
    {
        "login".to_owned()
    } else if b.text.is_some() || b.value.as_deref().is_some_and(|v| v.contains('\n')) {
        "text".to_owned()
    } else {
        "key".to_owned()
    }
}

/// `POST /secrets` → 201 `{ secret }`. The value is encrypted at once and never returned.
pub async fn create(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<CreateBody>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let (name, env) = validate_name(&b.name)?;
    let kind = infer_kind(&b);
    if !KINDS.contains(&kind.as_str()) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("kind must be one of {}", KINDS.join(", ")),
        ));
    }
    let body_value = json!({ "value": b.value, "text": b.text, "fields": b.fields });
    let fields = fields_for_kind(&kind, &body_value)?;
    if state
        .db
        .secret_by_name(&user_id, &name)
        .map_err(internal)?
        .is_some()
        || state
            .db
            .secret_by_name(&user_id, &env)
            .map_err(internal)?
            .is_some()
    {
        return Err(err(
            StatusCode::CONFLICT,
            "already_exists",
            format!("You already have a secret called {env}."),
        ));
    }
    let hosts = clean_hosts(b.hosts.as_deref().unwrap_or(&[]))?;
    let dek = user_dek(&state, &user_id).map_err(internal)?;
    let now = now_ms();
    let id = ulid();
    let s = Secret {
        id: id.clone(),
        user_id: user_id.clone(),
        name,
        env,
        kind,
        note: b.note.unwrap_or_default().chars().take(500).collect(),
        hosts,
        ask_every_time: b.ask_every_time.unwrap_or(false),
        expires_at: b.expires_at,
        current_version: 1,
        created_at: now,
        updated_at: now,
        last_used_at: None,
        daytona_secrets: json!({}),
    };
    let v1 = SecretVersion {
        secret_id: id.clone(),
        version: 1,
        fields_enc: encrypt_fields(&dek, &id, 1, &Value::Object(fields)).map_err(internal)?,
        created_at: now,
        retire_at: None,
    };
    state.db.insert_secret(&s, &v1).map_err(internal)?;
    audit(
        &state,
        &user_id,
        AuditEntry {
            detail: json!({ "kind": s.kind, "hosts": s.hosts }),
            ..entry("created", &s, &format!("user:{user_id}"))
        },
    );
    for pick in &b.agents {
        if let Some(a) = grant_access(
            &state,
            &user_id,
            &s,
            &pick.project_id,
            pick.note.as_deref(),
            None,
            true,
            "settings",
        )? {
            after_access_change(&state, &s, &a);
        }
    }
    Ok((
        StatusCode::CREATED,
        Json(json!({ "secret": secret_json(&state, &s) })),
    )
        .into_response())
}

/// Create or update the access row for `(secret, project)`; audits on/off. `Ok(None)` when the
/// project is not the user's.
#[allow(clippy::too_many_arguments)]
fn grant_access(
    state: &AppState,
    user_id: &str,
    s: &Secret,
    project_id: &str,
    note: Option<&str>,
    hosts: Option<Vec<String>>,
    enabled: bool,
    created_from: &str,
) -> Result<Option<Access>, ApiErr> {
    let Some(p) = state.db.project(project_id).map_err(internal)? else {
        return Err(err(StatusCode::NOT_FOUND, "not_found", "no such agent"));
    };
    if p.user_id != user_id {
        return Err(err(StatusCode::FORBIDDEN, "not_owner", "not your agent"));
    }
    let existing = state.db.access(&p.id, "secret", &s.id).map_err(internal)?;
    let was_on = existing.as_ref().is_some_and(|a| a.enabled);
    let now = now_ms();
    let a = Access {
        id: existing.as_ref().map(|a| a.id.clone()).unwrap_or_else(ulid),
        user_id: user_id.to_owned(),
        project_id: p.id.clone(),
        target_kind: "secret".into(),
        target_id: s.id.clone(),
        note: note
            .map(|n| n.chars().take(500).collect())
            .or_else(|| existing.as_ref().map(|a| a.note.clone()))
            .unwrap_or_default(),
        hosts: hosts
            .or_else(|| existing.as_ref().map(|a| a.hosts.clone()))
            .unwrap_or_default(),
        scope_kind: existing
            .as_ref()
            .map(|a| a.scope_kind.clone())
            .unwrap_or_else(|| "all".into()),
        scope_chat_id: existing.as_ref().and_then(|a| a.scope_chat_id.clone()),
        enabled,
        created_from: existing
            .as_ref()
            .map(|a| a.created_from.clone())
            .unwrap_or_else(|| created_from.to_owned()),
        created_at: existing.as_ref().map(|a| a.created_at).unwrap_or(now),
        disabled_at: if enabled {
            None
        } else {
            existing.as_ref().and_then(|a| a.disabled_at).or(Some(now))
        },
        last_used_at: existing.as_ref().and_then(|a| a.last_used_at),
    };
    state.db.upsert_access(&a).map_err(internal)?;
    if was_on != enabled || existing.is_none() {
        audit(
            state,
            user_id,
            AuditEntry {
                project_id: Some(p.id.clone()),
                access_id: Some(a.id.clone()),
                detail: json!({ "note": a.note, "hosts": a.hosts, "created_from": created_from }),
                ..entry(
                    if enabled { "access_on" } else { "access_off" },
                    s,
                    &format!("user:{user_id}"),
                )
            },
        );
    }
    Ok(Some(a))
}

/// `GET /secrets/{id}` → `{ secret, activity }`.
pub async fn get(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let s = state
        .db
        .secret(&user_id, &id)
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let act = activity(
        &state,
        &user_id,
        AuditQuery {
            target_id: Some(s.id.clone()),
            since: Some(now_ms() - ACTIVITY_WINDOW_MS),
            limit: 50,
            ..Default::default()
        },
    );
    Ok(Json(json!({ "secret": secret_json(&state, &s), "activity": act })).into_response())
}

#[derive(Deserialize, Default)]
pub struct PatchBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub hosts: Option<Vec<String>>,
    #[serde(default)]
    pub ask_every_time: Option<bool>,
    /// `null` clears; absent keeps.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub expires_at: Option<Option<i64>>,
}

fn deserialize_double_option<'de, D>(d: D) -> Result<Option<Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<i64>::deserialize(d)?))
}

/// `PATCH /secrets/{id}` → `{ secret }`.
pub async fn patch(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<PatchBody>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let mut s = state
        .db
        .secret(&user_id, &id)
        .map_err(internal)?
        .ok_or_else(not_found)?;
    if let Some(name) = b.name.as_deref() {
        let (name, env) = validate_name(name)?;
        for other in [&name, &env] {
            if let Some(o) = state.db.secret_by_name(&user_id, other).map_err(internal)? {
                if o.id != s.id {
                    return Err(err(
                        StatusCode::CONFLICT,
                        "already_exists",
                        format!("You already have a secret called {env}."),
                    ));
                }
            }
        }
        s.name = name;
        s.env = env;
    }
    if let Some(note) = b.note {
        s.note = note.chars().take(500).collect();
    }
    let hosts_changed = b.hosts.is_some();
    if let Some(h) = b.hosts.as_deref() {
        s.hosts = clean_hosts(h)?;
    }
    if let Some(a) = b.ask_every_time {
        s.ask_every_time = a;
    }
    if let Some(e) = b.expires_at {
        s.expires_at = e;
    }
    state.db.update_secret(&s).map_err(internal)?;
    audit(
        &state,
        &user_id,
        AuditEntry {
            detail: json!({ "hosts": s.hosts, "ask_every_time": s.ask_every_time, "expires_at": s.expires_at }),
            ..entry("updated", &s, &format!("user:{user_id}"))
        },
    );
    if hosts_changed {
        for a in state
            .db
            .accesses_of_target("secret", &s.id)
            .unwrap_or_default()
        {
            after_access_change(&state, &s, &a);
        }
    }
    Ok(Json(json!({ "secret": secret_json(&state, &s) })).into_response())
}

/// `PUT /secrets/{id}/value` `{ value | text | fields }` → `{ secret }`: version n+1; version n
/// keeps resolving for 24 h; Daytona mirrors refreshed; the box drops its cached placeholder env.
pub async fn replace_value(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let s = state
        .db
        .secret(&user_id, &id)
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let fields = fields_for_kind(&s.kind, &b)?;
    let dek = user_dek(&state, &user_id).map_err(internal)?;
    let next = s.current_version + 1;
    let enc = encrypt_fields(&dek, &s.id, next, &Value::Object(fields)).map_err(internal)?;
    let got = state
        .db
        .add_secret_version(&user_id, &s.id, &enc)
        .map_err(internal)?;
    if got != next {
        return Err(err(
            StatusCode::CONFLICT,
            "conflict",
            "the value changed under you; reload and try again",
        ));
    }
    let s = state
        .db
        .secret(&user_id, &id)
        .map_err(internal)?
        .ok_or_else(not_found)?;
    audit(
        &state,
        &user_id,
        AuditEntry {
            detail: json!({ "version": s.current_version }),
            ..entry("value_replaced", &s, &format!("user:{user_id}"))
        },
    );
    for a in state
        .db
        .accesses_of_target("secret", &s.id)
        .unwrap_or_default()
    {
        push_box(
            &state,
            &a.project_id,
            "secret_value_replaced",
            json!({ "ref": placeholder(&s.env, "value"), "env": s.env }),
        );
    }
    spawn_remount(state.clone(), user_id.clone());
    Ok(Json(json!({ "secret": secret_json(&state, &s) })).into_response())
}

/// `DELETE /secrets/{id}` → 204. Ciphertext, versions and accesses go; audit stays.
pub async fn delete(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let s = state
        .db
        .secret(&user_id, &id)
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let accesses = state
        .db
        .accesses_of_target("secret", &s.id)
        .unwrap_or_default();
    state.db.delete_secret(&user_id, &id).map_err(internal)?;
    audit(
        &state,
        &user_id,
        AuditEntry {
            detail: json!({ "agents": accesses.len() }),
            ..entry("deleted", &s, &format!("user:{user_id}"))
        },
    );
    for a in accesses {
        let mut off = a.clone();
        off.enabled = false;
        after_access_change(&state, &s, &off);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---- device: access ----------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct AccessBody {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub hosts: Option<Vec<String>>,
    /// `{ "kind": "all" | "chat", "chat_id"? }`.
    #[serde(default)]
    pub scope: Option<Value>,
}

fn default_true() -> bool {
    true
}

/// `PUT /secrets/{id}/access/{project_id}` → `{ access }` (create or update; idempotent).
pub async fn put_access(
    State(state): State<AppState>,
    principal: Principal,
    Path((id, project_id)): Path<(String, String)>,
    body: Option<Json<AccessBody>>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let b = body.map(|b| b.0).unwrap_or_default();
    let s = state
        .db
        .secret(&user_id, &id)
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let hosts = match b.hosts.as_deref() {
        Some(h) => Some(clean_hosts(h)?),
        None => None,
    };
    let mut a = grant_access(
        &state,
        &user_id,
        &s,
        &project_id,
        b.note.as_deref(),
        hosts,
        b.enabled,
        "settings",
    )?
    .ok_or_else(not_found)?;
    if let Some(scope) = b.scope.as_ref() {
        let kind = scope.get("kind").and_then(Value::as_str).unwrap_or("all");
        a.scope_kind = if kind == "chat" { "chat" } else { "all" }.into();
        a.scope_chat_id = if kind == "chat" {
            scope
                .get("chat_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            None
        };
        state.db.upsert_access(&a).map_err(internal)?;
    }
    after_access_change(&state, &s, &a);
    Ok(Json(json!({ "access": access_json(&state, &a) })).into_response())
}

/// `DELETE /secrets/{id}/access/{project_id}` → 204: off, and gone from the lists.
pub async fn delete_access(
    State(state): State<AppState>,
    principal: Principal,
    Path((id, project_id)): Path<(String, String)>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let s = state
        .db
        .secret(&user_id, &id)
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let Some(existing) = state
        .db
        .access(&project_id, "secret", &s.id)
        .map_err(internal)?
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    if let Some(mut a) = state
        .db
        .delete_access(&user_id, &existing.id)
        .map_err(internal)?
    {
        a.enabled = false;
        audit(
            &state,
            &user_id,
            AuditEntry {
                project_id: Some(a.project_id.clone()),
                access_id: Some(a.id.clone()),
                ..entry("access_off", &s, &format!("user:{user_id}"))
            },
        );
        after_access_change(&state, &s, &a);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /secrets/{id}/revoke-all` → `{ secret }` (every access off).
pub async fn revoke_all(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let s = state
        .db
        .secret(&user_id, &id)
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let were_on = state
        .db
        .disable_accesses_of_target(&user_id, "secret", &s.id)
        .map_err(internal)?;
    for mut a in were_on {
        a.enabled = false;
        audit(
            &state,
            &user_id,
            AuditEntry {
                project_id: Some(a.project_id.clone()),
                access_id: Some(a.id.clone()),
                detail: json!({ "revoke_all": true }),
                ..entry("access_off", &s, &format!("user:{user_id}"))
            },
        );
        after_access_change(&state, &s, &a);
    }
    Ok(Json(json!({ "secret": secret_json(&state, &s) })).into_response())
}

/// `GET /projects/{id}/secrets` → `{ paused, items, requests, activity }` for the agent profile.
/// Owner or guest may read.
pub async fn project_secrets(
    State(state): State<AppState>,
    principal: Principal,
    Path(project_id): Path<String>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let p = state
        .db
        .project(&project_id)
        .map_err(internal)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "not_found", "no such agent"))?;
    let member =
        p.user_id == user_id || state.db.is_project_member(&p.id, &user_id).unwrap_or(false);
    if !member {
        return Err(err(StatusCode::FORBIDDEN, "forbidden", "not your agent"));
    }
    let owner = p.user_id.clone();
    let settings = state.db.secrets_settings(&owner).map_err(internal)?;
    let mut items = Vec::new();
    for a in state.db.accesses_of_project(&p.id).map_err(internal)? {
        if a.target_kind == "secret" {
            if let Some(s) = state.db.secret(&owner, &a.target_id).map_err(internal)? {
                let mut v = access_json(&state, &a);
                v["secret"] = json!({ "id": s.id, "name": s.name, "env": s.env, "kind": s.kind, "note": s.note,
                    "hosts": s.hosts, "last_used_at": s.last_used_at, "vault_name": ARBOS_VAULT_NAME });
                items.push(v);
            }
        } else if let Some(v) = crate::vaults::access_item_json(&state, &owner, &a) {
            items.push(v);
        }
    }
    let requests: Vec<Value> = state
        .db
        .list_secret_requests(&owner, Some(&p.id), 50)
        .map_err(internal)?
        .iter()
        .map(|r| request_json(&state, r))
        .collect();
    let act = activity(
        &state,
        &owner,
        AuditQuery {
            project_id: Some(p.id.clone()),
            since: Some(now_ms() - ACTIVITY_WINDOW_MS),
            limit: 50,
            ..Default::default()
        },
    );
    Ok(Json(json!({ "paused": settings.paused, "owner": owner == user_id, "items": items, "requests": requests, "activity": act }))
        .into_response())
}

// ---- device: kill switch, activity, requests ---------------------------------------------------

#[derive(Deserialize)]
pub struct PauseBody {
    pub paused: bool,
}

/// Flip the kill switch for a user: relay refuses at once; boxes hear within 5 s; Daytona
/// mounts within 15 s.
pub(crate) async fn set_paused(state: &AppState, user_id: &str, paused: bool, by: &str) -> Value {
    let before = state.db.secrets_settings(user_id).unwrap_or_default();
    let settings = state
        .db
        .set_secrets_paused(user_id, paused, by)
        .unwrap_or_default();
    if before.paused != paused {
        audit(
            state,
            user_id,
            AuditEntry {
                action: if paused { "paused" } else { "unpaused" }.into(),
                target_kind: "user".into(),
                target_id: user_id.to_owned(),
                target_name: "all secrets".into(),
                by: by.to_owned(),
                detail: json!({}),
                ..Default::default()
            },
        );
    }
    for p in state.db.projects_of_user(user_id).unwrap_or_default() {
        push_box(state, &p.id, "secrets_paused", json!({ "paused": paused }));
    }
    push_user(
        state,
        user_id,
        "secrets_paused",
        json!({ "paused": paused }),
    );
    remount_daytona(state, user_id).await;
    json!({ "paused": settings.paused, "paused_at": settings.paused_at, "paused_by": settings.paused_by })
}

/// `PUT /me/secrets/settings` `{ paused }`.
pub async fn put_settings(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<PauseBody>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    Ok(Json(set_paused(&state, &user_id, b.paused, "user").await).into_response())
}

#[derive(Deserialize, Default)]
pub struct ActivityQuery {
    #[serde(default)]
    pub secret_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub vault_id: Option<String>,
    #[serde(default)]
    pub before: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `GET /secrets/activity?secret_id=&project_id=&vault_id=&before=&limit=` → `{ items, next_before }`.
pub async fn list_activity(
    State(state): State<AppState>,
    principal: Principal,
    Query(q): Query<ActivityQuery>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let items = activity(
        &state,
        &user_id,
        AuditQuery {
            target_id: q.secret_id.or(q.vault_id),
            project_id: q.project_id,
            before: q.before,
            since: None,
            limit,
        },
    );
    let next_before = if items.len() as i64 == limit {
        items
            .last()
            .and_then(|a| a.get("at").and_then(Value::as_i64))
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_before": next_before })).into_response())
}

/// `GET /secrets/requests` → `{ items }` (pending first).
pub async fn list_requests(State(state): State<AppState>, principal: Principal) -> ApiResult {
    let user_id = device_user(&principal)?;
    let items: Vec<Value> = state
        .db
        .list_secret_requests(&user_id, None, 100)
        .map_err(internal)?
        .iter()
        .map(|r| request_json(&state, r))
        .collect();
    Ok(Json(json!({ "items": items })).into_response())
}

#[derive(Deserialize)]
pub struct DecideBody {
    /// `once | chat | always | deny`.
    pub decision: String,
}

/// The four card answers → request status.
fn status_for(decision: &str) -> Option<&'static str> {
    match decision {
        "once" | "allow_once" | "approve" => Some("allowed_once"),
        "chat" | "allow_chat" => Some("allowed_chat"),
        "always" | "allow_always" | "approved" => Some("allowed_always"),
        "deny" | "denied" | "reject" => Some("denied"),
        _ => None,
    }
}

/// Apply a decision: `chat`/`always` create (or re-enable) the Access with the request's
/// purpose as the note and its host pinned; `once` is a 5-minute grant; `deny` is final.
pub(crate) async fn decide(
    state: &AppState,
    user_id: &str,
    request_id: &str,
    decision: &str,
    decided_by: &str,
) -> Result<(SecretRequest, Option<Access>), ApiErr> {
    let status = status_for(decision).ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "decision must be once, chat, always or deny",
        )
    })?;
    let rq = state
        .db
        .decide_secret_request(user_id, request_id, status, decided_by)
        .map_err(internal)?
        .ok_or_else(|| {
            err(
                StatusCode::CONFLICT,
                "not_pending",
                "request is not pending (already decided, expired, or not yours)",
            )
        })?;
    let mut access = None;
    let secret = match (rq.target_kind.as_str(), rq.target_id.as_deref()) {
        ("secret", Some(id)) => state.db.secret(user_id, id).map_err(internal)?,
        _ => None,
    };
    if let Some(s) = &secret {
        audit(
            state,
            user_id,
            AuditEntry {
                project_id: Some(rq.project_id.clone()),
                chat_id: Some(rq.chat_id.clone()),
                request_id: Some(rq.id.clone()),
                use_kind: Some(rq.use_kind.clone()),
                host: rq.host.clone(),
                detail: json!({ "decision": status }),
                ..entry("decided", s, decided_by)
            },
        );
        if matches!(status, "allowed_chat" | "allowed_always") {
            let hosts = rq.host.clone().map(|h| vec![h]);
            let mut a = grant_access(
                state,
                user_id,
                s,
                &rq.project_id,
                Some(&rq.purpose),
                None,
                true,
                &format!("card:{}", rq.id),
            )?
            .ok_or_else(not_found)?;
            if let Some(h) = hosts {
                for host in h {
                    if !a.hosts.contains(&host) && !s.hosts.contains(&host) {
                        a.hosts.push(host);
                    }
                }
            }
            if status == "allowed_chat" {
                a.scope_kind = "chat".into();
                a.scope_chat_id = Some(rq.chat_id.clone());
            } else {
                a.scope_kind = "all".into();
                a.scope_chat_id = None;
            }
            state.db.upsert_access(&a).map_err(internal)?;
            after_access_change(state, s, &a);
            access = Some(a);
        }
    } else if rq.target_kind == "vault" {
        access = crate::vaults::decide_vault_request(state, user_id, &rq, status).await?;
    }
    push_box(
        state,
        &rq.project_id,
        "secret_decided",
        json!({ "request_id": rq.id, "status": rq.status, "access_id": access.as_ref().map(|a| a.id.clone()),
            "ref": rq.target_ref, "use": rq.use_kind, "host": rq.host }),
    );
    Ok((rq, access))
}

/// `POST /secrets/requests/{id}/decide { decision }` → `{ request, access? }`.
pub async fn decide_request(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<DecideBody>,
) -> ApiResult {
    let user_id = device_user(&principal)?;
    let (rq, access) = decide(&state, &user_id, &id, &b.decision, &format!("card:{id}")).await?;
    Ok(Json(json!({ "request": request_json(&state, &rq), "access": access.map(|a| access_json(&state, &a)) }))
        .into_response())
}

// ---- box ---------------------------------------------------------------------------------------

/// What this project may use right now, names only.
pub(crate) fn box_items(state: &AppState, user_id: &str, project_id: &str) -> Vec<Value> {
    let mut items = Vec::new();
    for s in state.db.list_secrets(user_id).unwrap_or_default() {
        let access = state.db.access(project_id, "secret", &s.id).ok().flatten();
        let fields: Vec<&str> = field_names(&s.kind);
        let placeholders: BTreeMap<String, String> = fields
            .iter()
            .map(|f| ((*f).to_owned(), placeholder(&s.env, f)))
            .collect();
        let mut hosts = s.hosts.clone();
        if let Some(a) = &access {
            hosts.extend(a.hosts.iter().cloned());
        }
        hosts.dedup();
        let enabled = access.as_ref().is_some_and(|a| a.enabled)
            && !s.expires_at.is_some_and(|t| t < now_ms());
        items.push(json!({
            "id": s.id, "name": s.name, "env": s.env, "kind": s.kind, "note": s.note, "hosts": hosts,
            "ref": placeholder(&s.env, if s.kind == "login" { "password" } else { "value" }),
            "fields": fields, "placeholders": placeholders, "vault_name": ARBOS_VAULT_NAME,
            "access_id": access.as_ref().map(|a| a.id.clone()), "enabled": enabled,
            "ask_every_time": s.ask_every_time, "expires_at": s.expires_at,
            "scope": access.as_ref().map(|a| json!({ "kind": a.scope_kind, "chat_id": a.scope_chat_id })),
        }));
    }
    items.extend(crate::vaults::box_items(state, user_id, project_id));
    items
}

/// `GET /box/secrets` → `{ paused, items }`.
pub async fn box_list(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
) -> ApiResult {
    let (user_id, project_id) = box_project(&state, &principal, &headers)?;
    let paused = state
        .db
        .secrets_settings(&user_id)
        .map(|s| s.paused)
        .unwrap_or(false);
    Ok(
        Json(json!({ "paused": paused, "items": box_items(&state, &user_id, &project_id) }))
            .into_response(),
    )
}

#[derive(Deserialize)]
pub struct BoxRequestBody {
    #[serde(default, alias = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub purpose: String,
    #[serde(rename = "use", default)]
    pub use_kind: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default, alias = "chatId")]
    pub chat_id: String,
}

/// `POST /box/secrets/request` → 201 `{ request }` (a card is needed) or 200
/// `{ request, status: "allowed_always", access_id }` when an Access already covers it.
pub async fn box_request(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Json(b): Json<BoxRequestBody>,
) -> ApiResult {
    let (user_id, project_id) = box_project(&state, &principal, &headers)?;
    let use_kind = b.use_kind.as_deref().unwrap_or("header").trim().to_owned();
    if !USES.contains(&use_kind.as_str()) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("use must be one of {}", USES.join(", ")),
        ));
    }
    let settings = state.db.secrets_settings(&user_id).map_err(internal)?;
    if settings.paused {
        return Err(err(
            StatusCode::FORBIDDEN,
            "paused",
            Refusal::Paused.message("", ""),
        ));
    }
    let host = b
        .host
        .as_deref()
        .map(|h| clean_hosts(&[h.to_owned()]))
        .transpose()?
        .and_then(|h| h.into_iter().next());
    let name = b
        .reference
        .as_deref()
        .or(b.name.as_deref())
        .unwrap_or("")
        .trim()
        .to_owned();
    if name.starts_with("op://") {
        return crate::vaults::box_request(
            &state,
            &user_id,
            &project_id,
            &name,
            &b.purpose,
            &use_kind,
            host,
            &b.chat_id,
        )
        .await;
    }
    let name = name
        .strip_prefix(PLACEHOLDER_SCHEME)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(&name)
        .to_owned();
    let Some(s) = state.db.secret_by_name(&user_id, &name).map_err(internal)? else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("{name}: no such secret (the user adds secrets in Settings → Secrets)"),
        ));
    };
    if use_kind == "browser_fill" && s.kind != "login" {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("{}: browser_fill needs a Login", s.env),
        ));
    }
    let chat_id = if b.chat_id.is_empty() {
        None
    } else {
        Some(b.chat_id.as_str())
    };
    // Standing access covers it: no card, but a decided request row so `values` has a window.
    if !s.ask_every_time {
        if let Ok(allowed) = decide_use(
            &state,
            &s,
            &project_id,
            host.as_deref().unwrap_or(""),
            &use_kind,
            chat_id,
        ) {
            if allowed.once.is_none() {
                if let Some(a) = &allowed.access {
                    let now = now_ms();
                    let rq = SecretRequest {
                        id: ulid(),
                        user_id: user_id.clone(),
                        project_id: project_id.clone(),
                        chat_id: b.chat_id.clone(),
                        target_kind: "secret".into(),
                        target_id: Some(s.id.clone()),
                        target_ref: s.env.clone(),
                        purpose: b.purpose.chars().take(400).collect(),
                        use_kind: use_kind.clone(),
                        host: host.clone(),
                        status: "allowed_always".into(),
                        decided_by: Some(format!("rule:{}", a.id)),
                        created_at: now,
                        decided_at: Some(now),
                        expires_at: i64::MAX,
                    };
                    state.db.insert_secret_request(&rq).map_err(internal)?;
                    if allowed.pin_host {
                        after_use(
                            &state,
                            &s,
                            &project_id,
                            host.as_deref().unwrap_or(""),
                            &use_kind,
                            chat_id,
                            &Allowed {
                                access: Some(a.clone()),
                                once: None,
                                pin_host: true,
                            },
                        );
                    }
                    return Ok(Json(json!({ "request": request_json(&state, &rq), "status": "allowed_always", "access_id": a.id, "item": box_item(&state, &s, Some(a)) }))
                        .into_response());
                }
            }
        }
    }
    // Resume an undecided request for the same thing instead of a second card.
    if let Some(rq) = state
        .db
        .pending_secret_request_for(&project_id, &s.env, &use_kind)
        .map_err(internal)?
    {
        return Ok((StatusCode::CREATED, Json(json!({ "request": request_json(&state, &rq), "status": "pending", "resumed": true })))
            .into_response());
    }
    let now = now_ms();
    let rq = SecretRequest {
        id: ulid(),
        user_id: user_id.clone(),
        project_id: project_id.clone(),
        chat_id: b.chat_id.clone(),
        target_kind: "secret".into(),
        target_id: Some(s.id.clone()),
        target_ref: s.env.clone(),
        purpose: b.purpose.chars().take(400).collect(),
        use_kind: use_kind.clone(),
        host: host.clone(),
        status: "pending".into(),
        decided_by: None,
        created_at: now,
        decided_at: None,
        expires_at: now + REQUEST_PENDING_MS,
    };
    state.db.insert_secret_request(&rq).map_err(internal)?;
    audit(
        &state,
        &user_id,
        AuditEntry {
            project_id: Some(project_id.clone()),
            chat_id: chat_id.map(str::to_owned),
            request_id: Some(rq.id.clone()),
            use_kind: Some(use_kind.clone()),
            host: host.clone(),
            detail: json!({ "purpose": rq.purpose }),
            ..entry("requested", &s, &format!("agent:{project_id}"))
        },
    );
    let agent = state
        .db
        .project(&project_id)
        .ok()
        .flatten()
        .map(|p| p.name)
        .unwrap_or_default();
    push_user(
        &state,
        &user_id,
        "secret_request",
        json!({ "request_id": rq.id, "project_id": project_id, "project_name": agent, "name": s.env, "host": host, "use": use_kind }),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "request": request_json(&state, &rq), "status": "pending" })),
    )
        .into_response())
}

fn box_item(state: &AppState, s: &Secret, access: Option<&Access>) -> Value {
    box_items(
        state,
        &s.user_id,
        access.map(|a| a.project_id.as_str()).unwrap_or(""),
    )
    .into_iter()
    .find(|v| v.get("id").and_then(Value::as_str) == Some(s.id.as_str()))
    .unwrap_or(Value::Null)
}

/// `GET /box/secrets/requests/{id}` → `{ request }` (own project only).
pub async fn box_request_status(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let (_user_id, project_id) = box_project(&state, &principal, &headers)?;
    let rq = state
        .db
        .secret_request(&id)
        .map_err(internal)?
        .filter(|r| r.project_id == project_id)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "not_found", "no such request"))?;
    Ok(Json(json!({ "request": request_json(&state, &rq) })).into_response())
}

/// `POST /box/secrets/requests/{id}/values` → login fields for one allowed `browser_fill`
/// request within ten minutes of the decision. The only path on which a value leaves the relay
/// in clear; agentd types it and forgets it.
pub async fn box_request_values(
    State(state): State<AppState>,
    principal: Principal,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let (user_id, project_id) = box_project(&state, &principal, &headers)?;
    if state
        .db
        .secrets_settings(&user_id)
        .map_err(internal)?
        .paused
    {
        return Err(err(
            StatusCode::FORBIDDEN,
            "paused",
            Refusal::Paused.message("", ""),
        ));
    }
    let rq = state
        .db
        .secret_request(&id)
        .map_err(internal)?
        .filter(|r| r.project_id == project_id)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "not_found", "no such request"))?;
    if rq.use_kind != "browser_fill" {
        return Err(err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "values are only released for browser_fill requests; use /egress",
        ));
    }
    if !rq.is_allowed() {
        return Err(err(
            StatusCode::CONFLICT,
            "not_pending",
            format!("request is {}", rq.status),
        ));
    }
    if rq.decided_at.is_some_and(|t| now_ms() - t > FILL_WINDOW_MS) {
        return Err(err(
            StatusCode::CONFLICT,
            "window_closed",
            "approval window closed; ask again",
        ));
    }
    let s = rq
        .target_id
        .as_deref()
        .and_then(|i| state.db.secret(&user_id, i).ok().flatten())
        .ok_or_else(|| err(StatusCode::GONE, "not_found", "the secret was deleted"))?;
    let host = rq.host.clone().unwrap_or_default();
    // The standing rule still applies at the moment of use.
    let allowed = match decide_use(
        &state,
        &s,
        &project_id,
        &host,
        "browser_fill",
        Some(rq.chat_id.as_str()),
    ) {
        Ok(a) => a,
        Err(Refusal::NoAccess | Refusal::AskEveryTime)
            if rq.status == "allowed_once" || rq.status == "allowed_chat" =>
        {
            Allowed {
                access: None,
                once: None,
                pin_host: false,
            }
        }
        Err(why) => {
            audit_refusal(
                &state,
                &user_id,
                &s.env,
                Some(&s),
                &project_id,
                &host,
                "browser_fill",
                &why,
            );
            return Err(err(
                StatusCode::FORBIDDEN,
                why.kind(),
                why.message(&s.env, &host),
            ));
        }
    };
    let dek = user_dek(&state, &user_id).map_err(internal)?;
    let plain = plain_of(&state, &dek, &s).map_err(internal)?;
    after_use(
        &state,
        &s,
        &project_id,
        &host,
        "browser_fill",
        Some(rq.chat_id.as_str()),
        &allowed,
    );
    Ok(Json(
        json!({ "name": s.env, "kind": s.kind, "allowed_hosts": s.hosts, "fields": plain.current }),
    )
    .into_response())
}

// ---- egress resolution -------------------------------------------------------------------------

/// Everything `/egress` needs for one request: usable items with plaintext, refusals by
/// placeholder key, and every value (current and previous) to redact from the reply.
pub(crate) struct EgressPlan {
    pub items: Vec<crate::vault::Resolved>,
    pub refused: Vec<(String, String)>,
    pub redact: Vec<String>,
    /// `(secret, allowed)` to settle after the request goes out.
    pub used: Vec<(Secret, Allowed)>,
}

/// Decide every placeholder in `texts` for `project_id` at `host`. Values are decrypted only
/// for secrets the rule allows.
pub(crate) fn plan_egress(
    state: &AppState,
    user_id: &str,
    project_id: &str,
    host: &str,
    chat_id: Option<&str>,
    texts: &[&str],
) -> Result<EgressPlan, ApiErr> {
    let mut plan = EgressPlan {
        items: Vec::new(),
        refused: Vec::new(),
        redact: Vec::new(),
        used: Vec::new(),
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut dek: Option<Dek> = None;
    for t in texts {
        for (env, field) in crate::vault::find_placeholders(t) {
            if !seen.insert(env.clone()) {
                continue;
            }
            if env.starts_with("op:") {
                continue;
            }
            let Some(s) = state.db.secret_by_name(user_id, &env).map_err(internal)? else {
                plan.refused.push((
                    format!("{env}/{field}"),
                    Refusal::NoSuch.message(&env, host),
                ));
                audit_refusal(
                    state,
                    user_id,
                    &env,
                    None,
                    project_id,
                    host,
                    "header",
                    &Refusal::NoSuch,
                );
                continue;
            };
            match decide_use(state, &s, project_id, host, "header", chat_id) {
                Ok(allowed) => {
                    if dek.is_none() {
                        dek = Some(user_dek(state, user_id).map_err(internal)?);
                    }
                    let plain =
                        plain_of(state, dek.as_ref().expect("dek"), &s).map_err(internal)?;
                    for v in &plain.all_values {
                        if !plan.redact.contains(v) {
                            plan.redact.push(v.clone());
                        }
                    }
                    let mut fields = plain.current;
                    // Logins: the password is the secret; the username is metadata but
                    // substitutable too (basic auth), the URL is not.
                    fields.remove("url");
                    plan.items.push(crate::vault::Resolved {
                        name: s.env.clone(),
                        allowed_hosts: vec![host.to_owned()],
                        fields,
                        approved: true,
                    });
                    plan.used.push((s, allowed));
                }
                Err(why) => {
                    plan.refused
                        .push((format!("{env}/{field}"), why.message(&s.env, host)));
                    audit_refusal(
                        state,
                        user_id,
                        &s.env,
                        Some(&s),
                        project_id,
                        host,
                        "header",
                        &why,
                    );
                }
            }
        }
    }
    Ok(plan)
}

// ---- admin -------------------------------------------------------------------------------------

/// `GET /admin/secrets` → counts per user, paused users, provider status.
pub async fn admin_status(State(state): State<AppState>, principal: Principal) -> ApiResult {
    if !matches!(principal, Principal::Admin) {
        return Err(err(StatusCode::FORBIDDEN, "forbidden", "admin only"));
    }
    let users: Vec<Value> = state
        .db
        .secrets_counts()
        .map_err(internal)?
        .into_iter()
        .map(|(u, n, on)| json!({ "user_id": u, "secrets": n, "accesses_on": on }))
        .collect();
    Ok(Json(json!({
        "users": users,
        "paused": state.db.paused_user_ids().unwrap_or_default(),
        "kek_from_env": crate::connectors::crypto::RelayKey::from_env(),
        "providers": crate::vaults::admin_status(&state),
    }))
    .into_response())
}

#[derive(Deserialize, Default)]
pub struct AdminPauseBody {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub all: bool,
    #[serde(default = "default_true")]
    pub paused: bool,
}

/// `POST /admin/secrets/pause { user_id | all, paused? }`.
pub async fn admin_pause(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<AdminPauseBody>,
) -> ApiResult {
    if !matches!(principal, Principal::Admin) {
        return Err(err(StatusCode::FORBIDDEN, "forbidden", "admin only"));
    }
    let users: Vec<String> = if b.all {
        state
            .db
            .secrets_counts()
            .map_err(internal)?
            .into_iter()
            .map(|(u, _, _)| u)
            .chain(state.db.paused_user_ids().unwrap_or_default())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    } else {
        b.user_id.into_iter().collect()
    };
    let mut out = Vec::new();
    for u in users {
        let v = set_paused(&state, &u, b.paused, "admin").await;
        out.push(json!({ "user_id": u, "paused": v["paused"] }));
    }
    Ok(Json(json!({ "users": out })).into_response())
}

// ---- F8 migration and sweeper ------------------------------------------------------------------

/// One-shot: `vault_items` → secrets (version 1, re-encrypted under the owner's DEK), approved
/// `vault_requests` → Access rows, pending → requests, the rest → audit. Keyed in `meta`.
pub fn migrate_f8(state: &AppState) -> anyhow::Result<()> {
    const KEY: &str = "secrets_f8_migrated";
    if state.db.meta_get(KEY)?.is_some() {
        return Ok(());
    }
    let mut moved = 0;
    let users: Vec<String> = state.db.vault_item_users()?;
    for user_id in users {
        let dek = user_dek(state, &user_id)?;
        for it in state.db.list_vault_items(&user_id)? {
            let plain = match state.relay_key.decrypt(&it.fields_enc) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, item = %it.name, "F8 migration: item did not decrypt; skipped");
                    continue;
                }
            };
            let fields: Value = serde_json::from_str(&plain).unwrap_or(json!({}));
            let kind = if it.kind == "login" { "login" } else { "key" };
            let (name, env) = match validate_name(&it.name) {
                Ok(v) => v,
                Err(_) => (it.name.clone(), env_from_name(&it.name)),
            };
            if state.db.secret_by_name(&user_id, &env)?.is_some() {
                continue;
            }
            let now = now_ms();
            let s = Secret {
                id: it.id.clone(),
                user_id: user_id.clone(),
                name,
                env,
                kind: kind.into(),
                note: String::new(),
                hosts: it.allowed_hosts.clone(),
                ask_every_time: false,
                expires_at: None,
                current_version: 1,
                created_at: it.created_at,
                updated_at: now,
                last_used_at: it.last_used_at,
                daytona_secrets: it.daytona_secrets.clone(),
            };
            let v1 = SecretVersion {
                secret_id: s.id.clone(),
                version: 1,
                fields_enc: encrypt_fields(&dek, &s.id, 1, &fields)?,
                created_at: it.created_at,
                retire_at: None,
            };
            state.db.insert_secret(&s, &v1)?;
            moved += 1;
            for rq in state.db.vault_requests_for_item(&it.id)? {
                match rq.status.as_str() {
                    "approved" => {
                        let a = Access {
                            id: ulid(),
                            user_id: user_id.clone(),
                            project_id: rq.project_id.clone(),
                            target_kind: "secret".into(),
                            target_id: s.id.clone(),
                            note: rq.purpose.clone(),
                            hosts: vec![],
                            scope_kind: "all".into(),
                            scope_chat_id: None,
                            enabled: true,
                            created_from: format!("card:{}", rq.id),
                            created_at: rq.decided_at.unwrap_or(rq.created_at),
                            disabled_at: None,
                            last_used_at: None,
                        };
                        state.db.upsert_access(&a)?;
                    }
                    "pending" => {
                        let now = now_ms();
                        state.db.insert_secret_request(&SecretRequest {
                            id: rq.id.clone(),
                            user_id: user_id.clone(),
                            project_id: rq.project_id.clone(),
                            chat_id: rq.chat_id.clone(),
                            target_kind: "secret".into(),
                            target_id: Some(s.id.clone()),
                            target_ref: s.env.clone(),
                            purpose: rq.purpose.clone(),
                            use_kind: rq.use_kind.clone(),
                            host: None,
                            status: "pending".into(),
                            decided_by: None,
                            created_at: rq.created_at,
                            decided_at: None,
                            expires_at: now + REQUEST_PENDING_MS,
                        })?;
                    }
                    other => audit(
                        state,
                        &user_id,
                        AuditEntry {
                            project_id: Some(rq.project_id.clone()),
                            request_id: Some(rq.id.clone()),
                            use_kind: Some(rq.use_kind.clone()),
                            detail: json!({ "decision": other, "migrated": true }),
                            ..entry("decided", &s, "system")
                        },
                    ),
                }
            }
            audit(
                state,
                &user_id,
                AuditEntry {
                    detail: json!({ "migrated_from": "vault_items" }),
                    ..entry("created", &s, "system")
                },
            );
        }
    }
    state.db.meta_set(KEY, "1")?;
    if moved > 0 {
        tracing::info!(moved, "F8 vault items migrated to secrets");
    }
    Ok(())
}

/// Every minute: secrets past `expires_at` lose their accesses (once, audited), and the owner
/// hears three days ahead (`secret_expiring`, once).
pub async fn spawn_sweeper(state: AppState) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            sweep_once(&state).await;
        }
    });
}

async fn sweep_once(state: &AppState) {
    let now = now_ms();
    for s in state.db.expired_secrets().unwrap_or_default() {
        let were_on = state
            .db
            .disable_accesses_of_target(&s.user_id, "secret", &s.id)
            .unwrap_or_default();
        if were_on.is_empty() {
            continue;
        }
        audit(
            state,
            &s.user_id,
            AuditEntry {
                detail: json!({ "agents": were_on.len() }),
                ..entry("expired", &s, "system")
            },
        );
        for mut a in were_on {
            a.enabled = false;
            after_access_change(state, &s, &a);
        }
        push_user(
            state,
            &s.user_id,
            "secret_expiring",
            json!({ "secret_id": s.id, "name": s.env, "expires_at": s.expires_at, "expired": true }),
        );
    }
    // Three-day notice, once per secret (the audit row is the marker).
    for (user_id, _, _) in state.db.secrets_counts().unwrap_or_default() {
        for s in state.db.list_secrets(&user_id).unwrap_or_default() {
            let Some(t) = s.expires_at else { continue };
            if t < now || t - now > EXPIRING_NOTICE_MS {
                continue;
            }
            let q = AuditQuery {
                target_id: Some(s.id.clone()),
                limit: 50,
                ..Default::default()
            };
            let already = state
                .db
                .list_audit(&user_id, &q)
                .unwrap_or_default()
                .iter()
                .any(|a| {
                    a.action == "expiring"
                        && a.detail.get("expires_at").and_then(Value::as_i64) == Some(t)
                });
            if already {
                continue;
            }
            audit(
                state,
                &user_id,
                AuditEntry {
                    detail: json!({ "expires_at": t }),
                    ..entry("expiring", &s, "system")
                },
            );
            push_user(
                state,
                &user_id,
                "secret_expiring",
                json!({ "secret_id": s.id, "name": s.env, "expires_at": t, "expired": false }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names_follow_the_spec() {
        assert_eq!(env_from_name("STRIPE_KEY"), "STRIPE_KEY");
        assert_eq!(env_from_name("stripe live"), "STRIPE_LIVE");
        assert_eq!(env_from_name("GitHub login"), "GITHUB_LOGIN");
        assert_eq!(env_from_name("demo-api.key"), "DEMO_API_KEY");
        assert_eq!(env_from_name("1password"), "_1PASSWORD");
        assert_eq!(env_from_name("  __x__ "), "X");
        assert!(validate_name("").is_err());
        assert!(validate_name("has/slash").is_err());
        assert!(validate_name("ok name").is_ok());
    }

    #[test]
    fn hosts_are_cleaned_and_checked() {
        let h = clean_hosts(&[
            "https://api.stripe.com/v1".into(),
            "API.STRIPE.COM".into(),
            "*.acme.internal:8443".into(),
            " ".into(),
        ])
        .unwrap();
        assert_eq!(h, vec!["api.stripe.com", "*.acme.internal"]);
        assert!(clean_hosts(&["bad host".into()]).is_err());
        assert!(clean_hosts(&["evil.example/../x".into()]).is_ok());
    }

    #[test]
    fn refusal_strings_never_carry_values() {
        let allowed = vec!["api.stripe.com".to_owned()];
        assert_eq!(
            Refusal::HostNotAllowed(allowed).message("STRIPE_KEY", "evil.example"),
            "STRIPE_KEY: not allowed at evil.example (allowed: api.stripe.com)."
        );
        assert_eq!(
            Refusal::TurnedOff.message("STRIPE_KEY", ""),
            "STRIPE_KEY: access was turned off by the owner."
        );
        assert_eq!(
            Refusal::Paused.message("x", "y"),
            "Secrets are paused by the owner."
        );
        assert_eq!(status_for("always"), Some("allowed_always"));
        assert_eq!(status_for("nope"), None);
    }
}
