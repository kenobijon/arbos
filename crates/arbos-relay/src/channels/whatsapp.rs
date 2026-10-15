//! WhatsApp Cloud API adapter: webhook verification (`GET /channels/whatsapp/webhook`), inbound
//! text messages (`POST`), outbound text through `graph.facebook.com`.
//!
//! Env: `ARBOS_WHATSAPP_TOKEN` (system-user access token), `ARBOS_WHATSAPP_PHONE_ID` (phone
//! number id), `ARBOS_WHATSAPP_VERIFY_TOKEN` (your own string, pasted in the Meta dashboard),
//! optional `ARBOS_WHATSAPP_APP_SECRET` to check `X-Hub-Signature-256`.

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use super::{handle_inbound, mock_on, Inbound, Platform};
use crate::state::AppState;

const TOKEN_ENV: &str = "ARBOS_WHATSAPP_TOKEN";
const PHONE_ID_ENV: &str = "ARBOS_WHATSAPP_PHONE_ID";
const VERIFY_ENV: &str = "ARBOS_WHATSAPP_VERIFY_TOKEN";
const APP_SECRET_ENV: &str = "ARBOS_WHATSAPP_APP_SECRET";
const GRAPH: &str = "https://graph.facebook.com/v21.0";

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub fn status(_state: &AppState) -> (bool, String) {
    let missing: Vec<&str> = [TOKEN_ENV, PHONE_ID_ENV, VERIFY_ENV]
        .into_iter()
        .filter(|k| env(k).is_none())
        .collect();
    if missing.is_empty() {
        (true, "Cloud API webhook + messages".into())
    } else {
        (false, format!("missing {}", missing.join(", ")))
    }
}

/// `GET /channels/whatsapp/webhook?hub.mode=subscribe&hub.verify_token=…&hub.challenge=…`.
pub async fn verify(
    State(state): State<AppState>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let mode = q.get("hub.mode").map(String::as_str);
    let token = q.get("hub.verify_token").map(String::as_str);
    let challenge = q.get("hub.challenge").cloned().unwrap_or_default();
    let expected = env(VERIFY_ENV);
    let ok = mode == Some("subscribe")
        && match expected.as_deref() {
            Some(e) => token == Some(e),
            None => mock_on(&state),
        };
    if ok {
        challenge.into_response()
    } else {
        (StatusCode::FORBIDDEN, "verify token mismatch").into_response()
    }
}

fn signature_ok(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    let expected = hex::encode(mac.finalize().into_bytes());
    expected.len() == hex_sig.len()
        && expected
            .bytes()
            .zip(hex_sig.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Every text message in one webhook payload (`entry[].changes[].value.messages[]`).
pub fn inbound_from_payload(v: &Value) -> Vec<Inbound> {
    let mut out = Vec::new();
    for entry in v
        .get("entry")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for change in entry
            .get("changes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let value = change.get("value").cloned().unwrap_or(Value::Null);
            let contacts = value
                .get("contacts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let name_of = |wa_id: &str| {
                contacts
                    .iter()
                    .find(|c| c.get("wa_id").and_then(Value::as_str) == Some(wa_id))
                    .and_then(|c| c["profile"]["name"].as_str())
                    .map(str::to_owned)
            };
            for m in value
                .get("messages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if m.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(from) = m.get("from").and_then(Value::as_str) else {
                    continue;
                };
                let Some(text) = m["text"]["body"].as_str() else {
                    continue;
                };
                let name = name_of(from).unwrap_or_else(|| format!("+{from}"));
                out.push(Inbound {
                    platform: Platform::Whatsapp,
                    // A WhatsApp conversation is identified by the counterpart's number.
                    external_chat_id: from.to_owned(),
                    external_user_id: from.to_owned(),
                    sender_name: name.clone(),
                    text: text.to_owned(),
                    event_key: m
                        .get("id")
                        .and_then(Value::as_str)
                        .map(|id| format!("whatsapp:{id}")),
                    trusted_sender: true,
                    label: format!("WhatsApp · {name}"),
                });
            }
        }
    }
    out
}

/// `POST /channels/whatsapp/webhook`.
pub async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(secret) = env(APP_SECRET_ENV) {
        let sig = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !signature_ok(&secret, &body, sig) {
            return (StatusCode::UNAUTHORIZED, "bad signature").into_response();
        }
    } else if !mock_on(&state) && env(VERIFY_ENV).is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{VERIFY_ENV} not set"),
        )
            .into_response();
    }
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    for inb in inbound_from_payload(&v) {
        handle_inbound(&state, inb).await;
    }
    StatusCode::OK.into_response()
}

pub async fn send(state: &AppState, to: &str, text: &str) -> anyhow::Result<Value> {
    let token = env(TOKEN_ENV).ok_or_else(|| anyhow::anyhow!("{TOKEN_ENV} not set"))?;
    let phone_id = env(PHONE_ID_ENV).ok_or_else(|| anyhow::anyhow!("{PHONE_ID_ENV} not set"))?;
    let r = state
        .http
        .post(format!("{GRAPH}/{phone_id}/messages"))
        .bearer_auth(token)
        .json(&json!({
            "messaging_product": "whatsapp", "recipient_type": "individual", "to": to,
            "type": "text", "text": { "preview_url": false, "body": text }
        }))
        .send()
        .await?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!(
            "whatsapp: HTTP {status} {}",
            v["error"]["message"].as_str().unwrap_or("")
        );
    }
    Ok(json!({ "messages": v.get("messages").cloned().unwrap_or(Value::Null) }))
}
