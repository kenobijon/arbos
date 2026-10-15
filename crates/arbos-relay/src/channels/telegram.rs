//! Telegram Bot API adapter: webhook inbound (`POST /channels/telegram/webhook/{secret}`) or long
//! polling (`ARBOS_TELEGRAM_POLL=1`, `getUpdates`), `sendMessage` outbound.
//!
//! Env: `ARBOS_TELEGRAM_BOT_TOKEN`, `ARBOS_TELEGRAM_WEBHOOK_SECRET` (the path segment; also sent
//! by Telegram as `X-Telegram-Bot-Api-Secret-Token` when the webhook was registered with it).

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::{handle_inbound, mock_on, Inbound, Platform};
use crate::state::AppState;

const TOKEN_ENV: &str = "ARBOS_TELEGRAM_BOT_TOKEN";
const SECRET_ENV: &str = "ARBOS_TELEGRAM_WEBHOOK_SECRET";
const POLL_ENV: &str = "ARBOS_TELEGRAM_POLL";
const OFFSET_META: &str = "telegram_poll_offset";

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn api(token: &str, method: &str) -> String {
    format!("https://api.telegram.org/bot{token}/{method}")
}

pub fn status(_state: &AppState) -> (bool, String) {
    match env(TOKEN_ENV) {
        Some(_) if env(POLL_ENV).as_deref() == Some("1") => {
            (true, "bot token · long polling".into())
        }
        Some(_) if env(SECRET_ENV).is_some() => (true, "bot token · webhook".into()),
        Some(_) => (false, format!("set {SECRET_ENV} (webhook) or {POLL_ENV}=1")),
        None => (false, format!("missing {TOKEN_ENV}")),
    }
}

/// One Bot API `Update` → `Inbound` (text messages only).
pub fn inbound_from_update(update: &Value) -> Option<Inbound> {
    let msg = update
        .get("message")
        .or_else(|| update.get("edited_message"))?;
    let text = msg.get("text").and_then(Value::as_str)?.to_owned();
    let chat = msg.get("chat")?;
    let from = msg.get("from")?;
    if from.get("is_bot").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let chat_id = chat.get("id")?.to_string();
    let user_id = from.get("id")?.to_string();
    let name = {
        let first = from.get("first_name").and_then(Value::as_str).unwrap_or("");
        let last = from.get("last_name").and_then(Value::as_str).unwrap_or("");
        let full = format!("{first} {last}").trim().to_owned();
        if full.is_empty() {
            from.get("username")
                .and_then(Value::as_str)
                .unwrap_or("Telegram user")
                .to_owned()
        } else {
            full
        }
    };
    let kind = match chat.get("type").and_then(Value::as_str) {
        Some("private") => "DM".to_owned(),
        _ => chat
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("group")
            .to_owned(),
    };
    let event_key = update
        .get("update_id")
        .map(|u| format!("telegram:{u}"))
        .or_else(|| {
            msg.get("message_id")
                .map(|m| format!("telegram:{chat_id}:{m}"))
        });
    Some(Inbound {
        platform: Platform::Telegram,
        external_chat_id: chat_id,
        external_user_id: user_id,
        sender_name: name.clone(),
        text,
        event_key,
        trusted_sender: true,
        label: format!("Telegram · {kind} · {name}"),
    })
}

/// `POST /channels/telegram/webhook/{secret}`.
pub async fn webhook(
    State(state): State<AppState>,
    Path(secret): Path<String>,
    headers: HeaderMap,
    Json(update): Json<Value>,
) -> Response {
    match env(SECRET_ENV) {
        Some(expected) => {
            let header_ok = headers
                .get("x-telegram-bot-api-secret-token")
                .and_then(|v| v.to_str().ok())
                .is_none_or(|h| h == expected);
            if secret != expected || !header_ok {
                return (StatusCode::UNAUTHORIZED, "bad webhook secret").into_response();
            }
        }
        None if mock_on(&state) => {}
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("{SECRET_ENV} not set"),
            )
                .into_response();
        }
    }
    if let Some(inb) = inbound_from_update(&update) {
        handle_inbound(&state, inb).await;
    }
    StatusCode::OK.into_response()
}

pub async fn send(state: &AppState, chat_id: &str, text: &str) -> anyhow::Result<Value> {
    let token = env(TOKEN_ENV).ok_or_else(|| anyhow::anyhow!("{TOKEN_ENV} not set"))?;
    let v: Value = state
        .http
        .post(api(&token, "sendMessage"))
        .json(&json!({ "chat_id": chat_id, "text": text }))
        .send()
        .await?
        .json()
        .await?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!(
            "telegram: {}",
            v.get("description")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    Ok(json!({ "message_id": v["result"]["message_id"].clone() }))
}

/// Long polling (`ARBOS_TELEGRAM_POLL=1`): `getUpdates` with the persisted offset.
pub async fn spawn_poller(state: AppState) {
    if mock_on(&state) || env(POLL_ENV).as_deref() != Some("1") {
        return;
    }
    let Some(token) = env(TOKEN_ENV) else {
        return;
    };
    tracing::info!("telegram: long polling getUpdates");
    tokio::spawn(async move {
        let mut offset: i64 = state
            .db
            .meta_get(OFFSET_META)
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        loop {
            let resp = state
                .http
                .get(api(&token, "getUpdates"))
                .query(&[("offset", offset.to_string()), ("timeout", "20".into())])
                .timeout(Duration::from_secs(40))
                .send()
                .await;
            let v: Value = match resp {
                Ok(r) => r.json().await.unwrap_or_default(),
                Err(e) => {
                    tracing::warn!(error = %e, "telegram getUpdates");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            for u in v
                .get("result")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                if let Some(id) = u.get("update_id").and_then(Value::as_i64) {
                    offset = offset.max(id + 1);
                }
                if let Some(inb) = inbound_from_update(&u) {
                    handle_inbound(&state, inb).await;
                }
            }
            let _ = state.db.meta_set(OFFSET_META, &offset.to_string());
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}
