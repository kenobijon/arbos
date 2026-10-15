//! Credential vault (F8, design §4.7 "secrets, two layers", D2, D19, D28). Items live here,
//! encrypted with the relay key; a box never receives a value. The agent gets a placeholder
//! `arbos-secret://<item>/<field>` and sends requests through `POST /egress`, where the relay swaps
//! the placeholder for the real value only when the user approved the item for that project and
//! the destination host is on the item's `allowed_hosts`; the reply comes back with every value
//! redacted. Logins are typed into the browser by agentd (`browser_fill`), which fetches the fields
//! for one approved request and keeps them for a few seconds; the model never sees them.
//!
//! `ARBOS_VAULT_MOCK=1` seeds two items and serves an httpbin-style echo target at
//! `/vault/mock/echo`, so the whole flow runs on one machine.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use arbos_core::ids::{now_ms, ulid};

use crate::auth::Principal;
use crate::db::{VaultItem, VaultRequest};
use crate::state::AppState;

type ApiResult = Result<Response, (StatusCode, String)>;

pub const PLACEHOLDER_SCHEME: &str = "arbos-secret://";
pub const REDACTED: &str = "[REDACTED:vault]";
/// A pending request the user never answers expires after this long.
const REQUEST_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// Window after approval in which `browser_fill` may fetch the fields.
const FILL_WINDOW_MS: i64 = 10 * 60 * 1000;
const EGRESS_TIMEOUT: Duration = Duration::from_secs(30);
const EGRESS_MAX_BODY: usize = 1024 * 1024;
const KINDS: [&str; 3] = ["login", "secret", "api_key"];
const USES: [&str; 3] = ["env", "header", "browser_fill"];
/// Mock items (`ARBOS_VAULT_MOCK=1`): demo values, host-restricted to this machine.
const MOCK_ITEMS: [(&str, &str); 2] = [("demo-login", "login"), ("demo-api-key", "api_key")];

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn bad(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

fn device_only(principal: &Principal) -> Result<(), (StatusCode, String)> {
    if principal.is_box() {
        Err((StatusCode::FORBIDDEN, "device only".into()))
    } else {
        Ok(())
    }
}

fn env_name(item: &str) -> String {
    let mut s = String::from("ARBOS_SECRET_");
    for ch in item.chars() {
        s.push(if ch.is_ascii_alphanumeric() {
            ch.to_ascii_uppercase()
        } else {
            '_'
        });
    }
    s
}

pub fn placeholder(item: &str, field: &str) -> String {
    format!("{PLACEHOLDER_SCHEME}{item}/{field}")
}

/// The item as every reader sees it: names, kinds, hosts, field *names*; never a value.
pub fn item_json(it: &VaultItem, fields: &[String], mock: bool) -> Value {
    let placeholders: BTreeMap<String, String> = fields
        .iter()
        .map(|f| (f.clone(), placeholder(&it.name, f)))
        .collect();
    json!({
        "id": it.id, "name": it.name, "kind": it.kind, "allowed_hosts": it.allowed_hosts,
        "fields": fields, "placeholders": placeholders, "env": env_name(&it.name),
        "created_at": it.created_at, "last_used_at": it.last_used_at,
        "daytona": !it.daytona_secrets.as_object().is_none_or(|m| m.is_empty()),
        "mock": mock,
    })
}

pub fn request_json(r: &VaultRequest) -> Value {
    json!({
        "id": r.id, "project_id": r.project_id, "chat_id": r.chat_id, "item_id": r.item_id, "item_name": r.item_name,
        "purpose": r.purpose, "use": r.use_kind, "status": r.status, "created_at": r.created_at,
        "decided_at": r.decided_at, "expires_at": r.expires_at,
    })
}

/// Decrypted fields of an item (`field → value`). Stays inside this process.
fn fields_of(state: &AppState, it: &VaultItem) -> anyhow::Result<BTreeMap<String, String>> {
    let plain = state.relay_key.decrypt(&it.fields_enc)?;
    let v: Value = serde_json::from_str(&plain)?;
    Ok(v.as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default())
}

fn field_names(state: &AppState, it: &VaultItem) -> Vec<String> {
    fields_of(state, it)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

// ---- host allow-list and placeholder substitution (pure; the invariant tests cover these) -----

/// `host` matches an allow-list entry exactly or through a leading `*.` wildcard; ports are ignored.
pub fn host_allowed(host: &str, allowed: &[String]) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    allowed.iter().any(|a| {
        let a = a.trim().to_ascii_lowercase();
        let a = a.split(':').next().unwrap_or("").to_owned();
        if a.is_empty() {
            return false;
        }
        if let Some(suffix) = a.strip_prefix("*.") {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == a
        }
    })
}

/// A vault item with its plaintext, as the substitution step needs it.
pub struct Resolved {
    pub name: String,
    pub allowed_hosts: Vec<String>,
    pub fields: BTreeMap<String, String>,
    /// The project has an approved, live request for this item.
    pub approved: bool,
}

/// Every `arbos-secret://item/field` occurrence in `s`, in order of appearance.
pub fn find_placeholders(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find(PLACEHOLDER_SCHEME) {
        let after = &rest[i + PLACEHOLDER_SCHEME.len()..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')))
            .unwrap_or(after.len());
        let token = &after[..end];
        if let Some((item, field)) = token.split_once('/') {
            if !item.is_empty() && !field.is_empty() {
                out.push((item.to_owned(), field.to_owned()));
            }
        }
        rest = &after[end..];
    }
    out
}

#[derive(Default)]
pub struct Substitution {
    /// Placeholders swapped for the real value (`item/field`).
    pub substituted: Vec<String>,
    /// Placeholders left as they were, with the reason.
    pub refused: Vec<(String, String)>,
    /// Plaintext values that went out and must never come back.
    pub secrets: Vec<String>,
}

/// Replace placeholders in `text` for `host`. A placeholder is swapped only when the item exists,
/// the project holds an approval for it, and `host` is on the item's allow-list; everything else
/// stays verbatim so a non-allowed destination only ever sees the placeholder.
pub fn substitute(text: &str, host: &str, items: &[Resolved], sub: &mut Substitution) -> String {
    let mut out = text.to_owned();
    for (item, field) in find_placeholders(text) {
        let key = format!("{item}/{field}");
        let ph = placeholder(&item, &field);
        let Some(res) = items.iter().find(|r| r.name == item) else {
            sub.refused.push((key, "no such item".into()));
            continue;
        };
        let Some(value) = res.fields.get(&field) else {
            sub.refused.push((key, "no such field".into()));
            continue;
        };
        if !res.approved {
            sub.refused
                .push((key, "no approved vault request for this project".into()));
            continue;
        }
        if !host_allowed(host, &res.allowed_hosts) {
            sub.refused.push((
                key,
                format!(
                    "host {host} not in allowed_hosts {}",
                    res.allowed_hosts.join(", ")
                ),
            ));
            continue;
        }
        out = out.replace(&ph, value);
        if !sub.substituted.contains(&key) {
            sub.substituted.push(key);
        }
        if !sub.secrets.contains(value) {
            sub.secrets.push(value.clone());
        }
    }
    out
}

/// Replace every secret with `[REDACTED:vault]` in all strings of `v` (keys included).
pub fn redact_value(v: &mut Value, secrets: &[String]) {
    if secrets.iter().all(String::is_empty) {
        return;
    }
    match v {
        Value::String(s) => {
            for sec in secrets.iter().filter(|s| !s.is_empty()) {
                if s.contains(sec.as_str()) {
                    *s = s.replace(sec.as_str(), REDACTED);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|x| redact_value(x, secrets)),
        Value::Object(map) => {
            let mut renamed = Vec::new();
            for (k, val) in map.iter_mut() {
                redact_value(val, secrets);
                if secrets
                    .iter()
                    .any(|s| !s.is_empty() && k.contains(s.as_str()))
                {
                    renamed.push(k.clone());
                }
            }
            for k in renamed {
                if let Some(val) = map.remove(&k) {
                    map.insert(REDACTED.to_owned(), val);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

// ---- device endpoints: items and requests ------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct ItemBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// `{ "url", "username", "password" }` or `{ "value" }`; absent on update keeps the stored one.
    #[serde(default)]
    pub fields: Option<Value>,
    #[serde(default, alias = "allowedHosts")]
    pub allowed_hosts: Option<Vec<String>>,
}

fn validate_name(name: &str) -> Result<String, (StatusCode, String)> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(bad(
            "name: 1-64 characters, letters, digits, '-', '_' or '.'",
        ));
    }
    Ok(name.to_owned())
}

fn clean_hosts(hosts: Vec<String>) -> Vec<String> {
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
        if !h.is_empty() && !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

fn fields_object(kind: &str, fields: &Value) -> Result<Value, (StatusCode, String)> {
    let obj = fields
        .as_object()
        .ok_or_else(|| bad("fields must be an object of strings"))?;
    let mut out = serde_json::Map::new();
    for (k, v) in obj {
        let Some(s) = v.as_str() else {
            return Err(bad(format!("field {k} must be a string")));
        };
        if !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || k.is_empty() {
            return Err(bad(format!("field name {k}: letters, digits, '_'")));
        }
        out.insert(k.clone(), Value::String(s.to_owned()));
    }
    let required: &[&str] = match kind {
        "login" => &["username", "password"],
        "secret" | "api_key" => &["value"],
        _ => &[],
    };
    for r in required {
        if !out.contains_key(*r) {
            return Err(bad(format!("{kind} needs field {r}")));
        }
    }
    Ok(Value::Object(out))
}

/// `GET /vault/items` (device and box): names, kinds, hosts, field names; never values.
pub async fn list_items(State(state): State<AppState>, _principal: Principal) -> ApiResult {
    let mock = state.cfg.vault_mock;
    let items: Vec<Value> = state
        .db
        .list_vault_items()
        .map_err(internal)?
        .iter()
        .map(|it| item_json(it, &field_names(&state, it), mock))
        .collect();
    Ok(Json(json!({ "mock": mock, "items": items })).into_response())
}

/// `POST /vault/items` (device): create or replace an item by name. Values are write-only.
pub async fn create_item(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<ItemBody>,
) -> ApiResult {
    device_only(&principal)?;
    let name = validate_name(b.name.as_deref().unwrap_or(""))?;
    let kind = b.kind.as_deref().unwrap_or("secret").trim().to_owned();
    if !KINDS.contains(&kind.as_str()) {
        return Err(bad(format!("kind must be one of {}", KINDS.join(", "))));
    }
    let fields = fields_object(
        &kind,
        b.fields.as_ref().ok_or_else(|| bad("fields required"))?,
    )?;
    let existing = state.db.vault_item_by_name(&name).map_err(internal)?;
    let it = VaultItem {
        id: existing.as_ref().map(|e| e.id.clone()).unwrap_or_else(ulid),
        name,
        kind,
        fields_enc: state
            .relay_key
            .encrypt(&fields.to_string())
            .map_err(internal)?,
        allowed_hosts: clean_hosts(b.allowed_hosts.unwrap_or_default()),
        created_at: existing
            .as_ref()
            .map(|e| e.created_at)
            .unwrap_or_else(now_ms),
        last_used_at: existing.as_ref().and_then(|e| e.last_used_at),
        daytona_secrets: existing
            .as_ref()
            .map(|e| e.daytona_secrets.clone())
            .unwrap_or_else(|| json!({})),
    };
    state.db.upsert_vault_item(&it).map_err(internal)?;
    let names = field_names(&state, &it);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "item": item_json(&it, &names, state.cfg.vault_mock) })),
    )
        .into_response())
}

/// `PUT /vault/items/{id}` (device): change hosts and/or fields.
pub async fn update_item(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<ItemBody>,
) -> ApiResult {
    device_only(&principal)?;
    let mut it = state
        .db
        .vault_item(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such item".to_owned()))?;
    if let Some(hosts) = b.allowed_hosts {
        it.allowed_hosts = clean_hosts(hosts);
    }
    if let Some(fields) = b.fields.as_ref() {
        let obj = fields_object(&it.kind, fields)?;
        it.fields_enc = state
            .relay_key
            .encrypt(&obj.to_string())
            .map_err(internal)?;
    }
    state.db.upsert_vault_item(&it).map_err(internal)?;
    let names = field_names(&state, &it);
    Ok(Json(json!({ "item": item_json(&it, &names, state.cfg.vault_mock) })).into_response())
}

pub async fn delete_item(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    device_only(&principal)?;
    let ok = state.db.delete_vault_item(&id).map_err(internal)?;
    Ok(if ok {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
    .into_response())
}

/// `GET /vault/requests` (device): pending first, then the newest decided ones.
pub async fn list_requests(State(state): State<AppState>, principal: Principal) -> ApiResult {
    device_only(&principal)?;
    let items: Vec<Value> = state
        .db
        .list_vault_requests(100)
        .map_err(internal)?
        .iter()
        .map(request_json)
        .collect();
    Ok(Json(json!({ "mock": state.cfg.vault_mock, "items": items })).into_response())
}

#[derive(Deserialize)]
pub struct DecideBody {
    pub approve: bool,
}

/// `POST /vault/requests/{id}/decide { approve }` (device). On approve of an `env` use for a
/// Daytona box, the item is mirrored as Daytona Secrets so the placeholder also works through
/// Daytona's egress proxy (D28); process boxes rely on `/egress`.
pub async fn decide_request(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    Json(b): Json<DecideBody>,
) -> ApiResult {
    device_only(&principal)?;
    let rq = state
        .db
        .decide_vault_request(&id, b.approve)
        .map_err(internal)?
        .ok_or((
            StatusCode::CONFLICT,
            "request is not pending (already decided or expired)".to_owned(),
        ))?;
    let mut daytona: Option<String> = None;
    if b.approve && rq.use_kind == "env" {
        if let Some(item_id) = rq.item_id.as_deref() {
            daytona = mirror_to_daytona(&state, &rq.project_id, item_id).await;
        }
    }
    let _ = state.db.enqueue_push(
        &rq.project_id,
        "vault_grant",
        &json!({ "requestId": rq.id, "itemName": rq.item_name, "use": rq.use_kind, "status": rq.status }),
    );
    Ok(Json(json!({ "request": request_json(&rq), "daytona": daytona })).into_response())
}

/// Best effort: create/refresh one Daytona secret per field and mount them on the project's
/// sandbox as `ARBOS_SECRET_<NAME>` (single-field items) or `ARBOS_SECRET_<NAME>_<FIELD>`.
async fn mirror_to_daytona(state: &AppState, project_id: &str, item_id: &str) -> Option<String> {
    let dt = state.daytona.as_ref()?;
    let sb = state.db.sandbox(project_id).ok().flatten()?;
    if sb.provider != "daytona" {
        return None;
    }
    let sandbox_id = sb.sandbox_id?;
    let mut it = state.db.vault_item(item_id).ok().flatten()?;
    let fields = fields_of(state, &it).ok()?;
    let mut ids = it.daytona_secrets.as_object().cloned().unwrap_or_default();
    let mut mounts = Vec::new();
    for (field, value) in &fields {
        let secret_name = format!("arbos-{}-{}", it.id.to_lowercase(), field);
        let outcome = match ids.get(field).and_then(Value::as_str) {
            Some(existing) => dt
                .update_secret(existing, value, &it.allowed_hosts)
                .await
                .map(|_| existing.to_owned()),
            None => {
                dt.create_secret(&secret_name, value, &it.allowed_hosts)
                    .await
            }
        };
        match outcome {
            Ok(sid) => {
                ids.insert(field.clone(), Value::String(sid));
                let env = if fields.len() == 1 {
                    env_name(&it.name)
                } else {
                    format!("{}_{}", env_name(&it.name), field.to_ascii_uppercase())
                };
                let mut m = serde_json::Map::new();
                m.insert(env, Value::String(secret_name));
                mounts.push(m);
            }
            Err(e) => {
                tracing::warn!(error = %e, item = %it.name, "daytona secret mirror failed");
                return Some(format!("secret mirror failed: {e}"));
            }
        }
    }
    it.daytona_secrets = Value::Object(ids);
    let _ = state.db.upsert_vault_item(&it);
    match dt.update_secrets(&sandbox_id, mounts).await {
        Ok(_) => Some("mounted".into()),
        Err(e) => {
            tracing::warn!(error = %e, "daytona updateSecrets failed");
            Some(format!("updateSecrets failed: {e}"))
        }
    }
}

// ---- box endpoints: request, status, values, egress --------------------------------------------

#[derive(Deserialize)]
pub struct RequestBody {
    pub name: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(rename = "use")]
    pub use_kind: String,
    #[serde(default, rename = "chatId", alias = "chat_id")]
    pub chat_id: String,
}

fn box_project(principal: &Principal) -> Result<String, (StatusCode, String)> {
    match principal {
        Principal::Box { project_id } => Ok(project_id.clone()),
        Principal::Device { .. } | Principal::Admin => Err((
            StatusCode::FORBIDDEN,
            "box token required (the agent asks, the user decides)".into(),
        )),
    }
}

/// `POST /vault/request` (box): the agent asks to use an item. Returns `{ id, status }`.
pub async fn create_request(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<RequestBody>,
) -> ApiResult {
    let project_id = box_project(&principal)?;
    if !USES.contains(&b.use_kind.as_str()) {
        return Err(bad(format!("use must be one of {}", USES.join(", "))));
    }
    let name = b.name.trim().to_owned();
    let item = state.db.vault_item_by_name(&name).map_err(internal)?;
    let Some(item) = item else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no vault item named {name} (the user adds items in Settings → Vault)"),
        ));
    };
    if b.use_kind == "browser_fill" && item.kind != "login" {
        return Err(bad("browser_fill needs a login item"));
    }
    let now = now_ms();
    let rq = VaultRequest {
        id: ulid(),
        project_id,
        chat_id: b.chat_id,
        item_id: Some(item.id.clone()),
        item_name: item.name.clone(),
        purpose: b.purpose.chars().take(400).collect(),
        use_kind: b.use_kind,
        status: "pending".into(),
        created_at: now,
        decided_at: None,
        expires_at: now + REQUEST_TTL_MS,
    };
    state.db.insert_vault_request(&rq).map_err(internal)?;
    Ok((StatusCode::CREATED, Json(request_json(&rq))).into_response())
}

/// `GET /vault/requests/{id}` (box: own project only; device: any): status, never values.
pub async fn request_status(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let rq = state
        .db
        .vault_request(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such request".to_owned()))?;
    if !principal.can_access_project(&rq.project_id) {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    Ok(Json(request_json(&rq)).into_response())
}

/// `POST /vault/requests/{id}/values` (box): the fields of a login for one approved
/// `browser_fill` request, within ten minutes of the decision. agentd types them into the browser
/// and forgets them; this is the only path on which a value leaves the relay in clear.
pub async fn request_values(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> ApiResult {
    let project_id = box_project(&principal)?;
    let rq = state
        .db
        .vault_request(&id)
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "no such request".to_owned()))?;
    if rq.project_id != project_id {
        return Err((StatusCode::FORBIDDEN, "not your project".into()));
    }
    if rq.use_kind != "browser_fill" {
        return Err((
            StatusCode::FORBIDDEN,
            "values are only released for browser_fill requests; use /egress".into(),
        ));
    }
    if rq.status != "approved" {
        return Err((StatusCode::CONFLICT, format!("request is {}", rq.status)));
    }
    if rq.decided_at.is_some_and(|t| now_ms() - t > FILL_WINDOW_MS) {
        return Err((
            StatusCode::CONFLICT,
            "approval window closed; ask again".into(),
        ));
    }
    let item = rq
        .item_id
        .as_deref()
        .and_then(|i| state.db.vault_item(i).ok().flatten())
        .ok_or((StatusCode::GONE, "item was deleted".to_owned()))?;
    let fields = fields_of(&state, &item).map_err(internal)?;
    let _ = state.db.touch_vault_item(&item.id);
    Ok(Json(json!({ "name": item.name, "kind": item.kind, "allowed_hosts": item.allowed_hosts, "fields": fields }))
        .into_response())
}

#[derive(Deserialize)]
pub struct EgressBody {
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// String body, or JSON (serialised) when an object/array.
    #[serde(default)]
    pub body: Option<Value>,
    /// Optional: the placeholders the caller intends to use (reported back, not required).
    #[serde(default)]
    pub placeholders: Vec<String>,
}

/// Items the project may use right now, with plaintext for the substitution step.
fn resolve_items(state: &AppState, project_id: &str) -> anyhow::Result<Vec<Resolved>> {
    let mut out = Vec::new();
    for it in state.db.list_vault_items()? {
        let approved = !state
            .db
            .approved_vault_requests(project_id, &it.id)?
            .is_empty();
        out.push(Resolved {
            name: it.name.clone(),
            allowed_hosts: it.allowed_hosts.clone(),
            fields: fields_of(state, &it)?,
            approved,
        });
    }
    Ok(out)
}

/// `POST /egress` (box): perform an HTTP request on the box's behalf with placeholders swapped for
/// allowed hosts, and return the reply with every substituted value redacted.
pub async fn egress(
    State(state): State<AppState>,
    principal: Principal,
    Json(b): Json<EgressBody>,
) -> ApiResult {
    let project_id = box_project(&principal)?;
    let url = reqwest::Url::parse(b.url.trim()).map_err(|e| bad(format!("url: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad("url must be http(s)"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| bad("url has no host"))?
        .to_owned();
    let method = Method::from_bytes(
        b.method
            .as_deref()
            .unwrap_or("GET")
            .trim()
            .to_ascii_uppercase()
            .as_bytes(),
    )
    .map_err(|_| bad("method"))?;
    if !find_placeholders(url.as_str()).is_empty() {
        return Err(bad(
            "placeholders are substituted in headers and body only, never in the URL",
        ));
    }
    let items = resolve_items(&state, &project_id).map_err(internal)?;
    let mut sub = Substitution::default();
    let mut headers = BTreeMap::new();
    for (k, v) in &b.headers {
        headers.insert(k.clone(), substitute(v, &host, &items, &mut sub));
    }
    let body_text = match &b.body {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    };
    let body_out = body_text
        .as_deref()
        .map(|t| substitute(t, &host, &items, &mut sub));
    let used: HashSet<String> = sub.substituted.iter().cloned().collect();
    for it in state.db.list_vault_items().map_err(internal)? {
        if used.iter().any(|u| u.starts_with(&format!("{}/", it.name))) {
            let _ = state.db.touch_vault_item(&it.id);
        }
    }

    let mut req = state.http.request(method.clone(), url.clone());
    req = req.timeout(EGRESS_TIMEOUT);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(body) = body_out {
        req = req.body(body);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let mut v = json!({ "ok": false, "error": format!("{host}: {e}"), "substituted": sub.substituted,
                "refused": sub.refused.iter().map(|(p, r)| json!({ "placeholder": p, "reason": r })).collect::<Vec<_>>() });
            redact_value(&mut v, &sub.secrets);
            return Ok((StatusCode::BAD_GATEWAY, Json(v)).into_response());
        }
    };
    let status = resp.status();
    let mut resp_headers = BTreeMap::new();
    for (k, v) in resp.headers() {
        if let Ok(s) = v.to_str() {
            resp_headers.insert(k.as_str().to_owned(), s.to_owned());
        }
    }
    let bytes = resp.bytes().await.unwrap_or_default();
    let truncated = bytes.len() > EGRESS_MAX_BODY;
    let slice = &bytes[..bytes.len().min(EGRESS_MAX_BODY)];
    let body_json: Value = match std::str::from_utf8(slice) {
        Ok(text) => Value::String(text.to_owned()),
        Err(_) => json!({ "base64": B64.encode(slice) }),
    };
    let mut v = json!({
        "ok": status.is_success(), "status": status.as_u16(), "headers": resp_headers, "body": body_json,
        "truncated": truncated, "substituted": sub.substituted,
        "refused": sub.refused.iter().map(|(p, r)| json!({ "placeholder": p, "reason": r })).collect::<Vec<_>>(),
        "requested_placeholders": b.placeholders,
    });
    redact_value(&mut v, &sub.secrets);
    Ok(Json(v).into_response())
}

// ---- mock ----------------------------------------------------------------------------------------

/// `ARBOS_VAULT_MOCK=1`: two demo items whose hosts are this machine, so `/egress` against the
/// relay's own echo target shows the substitution end to end.
pub fn seed_mock(state: &AppState) -> anyhow::Result<()> {
    if !state.cfg.vault_mock {
        return Ok(());
    }
    let echo = format!("{}/vault/mock/echo", state.cfg.public_url);
    let hosts = vec!["127.0.0.1".to_owned(), "localhost".to_owned()];
    for (name, kind) in MOCK_ITEMS {
        if state.db.vault_item_by_name(name)?.is_some() {
            continue;
        }
        let fields = match kind {
            "login" => {
                json!({ "url": echo, "username": "demo@arbos.local", "password": format!("mock-pw-{}", ulid().to_lowercase()) })
            }
            _ => json!({ "value": format!("mock-key-{}", ulid().to_lowercase()) }),
        };
        state.db.upsert_vault_item(&VaultItem {
            id: ulid(),
            name: name.into(),
            kind: kind.into(),
            fields_enc: state.relay_key.encrypt(&fields.to_string())?,
            allowed_hosts: hosts.clone(),
            created_at: now_ms(),
            last_used_at: None,
            daytona_secrets: json!({}),
        })?;
    }
    tracing::info!(target = %echo, "ARBOS_VAULT_MOCK=1: seeded demo-login and demo-api-key (hosts 127.0.0.1, localhost)");
    Ok(())
}

/// `ANY /vault/mock/echo`: an httpbin-style target that reflects method, headers and body. No auth;
/// enabled only in mock mode. Shows what a real destination would have received.
pub async fn mock_echo(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if !state.cfg.vault_mock {
        return Err((StatusCode::NOT_FOUND, "not in mock mode".into()));
    }
    let hdrs: BTreeMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_owned(), s.to_owned()))
        })
        .collect();
    let body_text = String::from_utf8_lossy(&body).into_owned();
    let authorized = hdrs
        .get("authorization")
        .is_some_and(|a| a.starts_with("Bearer mock-key-") || a.starts_with("Bearer "));
    Ok(Json(json!({
        "echo": true, "method": method.as_str(), "headers": hdrs, "body": body_text,
        "authorized": authorized, "received_placeholder": body_text.contains(PLACEHOLDER_SCHEME)
            || hdrs.values().any(|v| v.contains(PLACEHOLDER_SCHEME)),
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Resolved> {
        vec![
            Resolved {
                name: "acme".into(),
                allowed_hosts: vec!["api.acme.test".into(), "*.acme.internal".into()],
                fields: BTreeMap::from([("value".to_owned(), "s3cr3t-VALUE-9f8e7d".to_owned())]),
                approved: true,
            },
            Resolved {
                name: "unapproved".into(),
                allowed_hosts: vec!["api.acme.test".into()],
                fields: BTreeMap::from([("value".to_owned(), "never-sent-1234".to_owned())]),
                approved: false,
            },
        ]
    }

    /// Invariant (F8): a placeholder is never substituted for a host outside `allowed_hosts`
    /// (nor for an item the project has no approval for); the destination only ever sees the
    /// placeholder text.
    #[test]
    fn placeholder_not_substituted_for_non_allowed_host() {
        let items = items();
        let header = "Bearer arbos-secret://acme/value";
        for host in [
            "evil.example",
            "acme.test",
            "api.acme.test.evil.example",
            "notacme.internal",
        ] {
            let mut sub = Substitution::default();
            let out = substitute(header, host, &items, &mut sub);
            assert_eq!(out, header, "{host} must keep the placeholder");
            assert!(
                sub.substituted.is_empty() && sub.secrets.is_empty(),
                "{host}"
            );
            assert_eq!(sub.refused.len(), 1);
        }
        // Allowed hosts (exact and wildcard) do substitute.
        for host in ["api.acme.test", "API.ACME.TEST", "db.acme.internal"] {
            let mut sub = Substitution::default();
            let out = substitute(header, host, &items, &mut sub);
            assert_eq!(out, "Bearer s3cr3t-VALUE-9f8e7d", "{host}");
            assert_eq!(sub.substituted, vec!["acme/value".to_owned()]);
        }
        // No approval: allowed host, still refused.
        let mut sub = Substitution::default();
        let out = substitute(
            "x arbos-secret://unapproved/value",
            "api.acme.test",
            &items,
            &mut sub,
        );
        assert_eq!(out, "x arbos-secret://unapproved/value");
        assert!(sub.refused[0].1.contains("no approved"));
        // Placeholders in a JSON body, several at once.
        let mut sub = Substitution::default();
        let body = r#"{"k":"arbos-secret://acme/value","again":"arbos-secret://acme/value","missing":"arbos-secret://acme/nope"}"#;
        let out = substitute(body, "api.acme.test", &items, &mut sub);
        assert!(
            !out.contains("arbos-secret://acme/value") && out.contains("arbos-secret://acme/nope")
        );
        assert_eq!(sub.secrets, vec!["s3cr3t-VALUE-9f8e7d".to_owned()]);
    }

    /// Invariant (F8): the raw value never appears in an egress reply or in any item/request
    /// listing: every string (and key) carrying it is redacted, and the item JSON only names fields.
    #[test]
    fn raw_value_never_in_egress_response_or_listing() {
        let secret = "s3cr3t-VALUE-9f8e7d".to_owned();
        // An upstream that echoes everything it got, including the value in a header name.
        let mut reply = json!({
            "status": 200,
            "headers": { "x-echo": format!("Bearer {secret}"), secret.clone(): "as a key" },
            "body": format!("{{\"got\":\"{secret}\",\"nested\":[\"{secret}-suffix\"]}}"),
            "fine": "untouched",
        });
        redact_value(&mut reply, std::slice::from_ref(&secret));
        let text = reply.to_string();
        assert!(!text.contains(&secret), "{text}");
        assert!(text.contains(REDACTED));
        assert_eq!(reply["fine"], "untouched");
        assert_eq!(reply["headers"][REDACTED], "as a key");

        // Item listing: names, kind, hosts, field names, placeholders; never the ciphertext or value.
        let key = crate::connectors::crypto::RelayKey::derive("test-key");
        let enc = key
            .encrypt(&json!({ "value": secret }).to_string())
            .unwrap();
        let it = VaultItem {
            id: "i1".into(),
            name: "acme".into(),
            kind: "api_key".into(),
            fields_enc: enc.clone(),
            allowed_hosts: vec!["api.acme.test".into()],
            created_at: 0,
            last_used_at: None,
            daytona_secrets: json!({}),
        };
        let listing = item_json(&it, &["value".to_owned()], false).to_string();
        assert!(
            !listing.contains(&secret) && !listing.contains(&enc),
            "{listing}"
        );
        assert!(listing.contains("arbos-secret://acme/value"));
        let rq = VaultRequest {
            id: "r1".into(),
            project_id: "p1".into(),
            chat_id: "c1".into(),
            item_id: Some("i1".into()),
            item_name: "acme".into(),
            purpose: "call the API".into(),
            use_kind: "header".into(),
            status: "approved".into(),
            created_at: 0,
            decided_at: Some(1),
            expires_at: 2,
        };
        assert!(!request_json(&rq).to_string().contains(&secret));
        assert_eq!(env_name("demo-api.key"), "ARBOS_SECRET_DEMO_API_KEY");
    }
}
