//! Slack adapter: Events API inbound (`POST /channels/slack/events`, signing-secret check, URL
//! verification handshake) and `chat.postMessage` outbound with the bot token.
//!
//! Env: `ARBOS_SLACK_SIGNING_SECRET`, `ARBOS_SLACK_BOT_TOKEN` (xoxb-…).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use arbos_core::ids::now_ms;

use super::{handle_inbound, mock_on, Inbound, Platform};
use crate::state::AppState;

const SIGNING_SECRET_ENV: &str = "ARBOS_SLACK_SIGNING_SECRET";
const BOT_TOKEN_ENV: &str = "ARBOS_SLACK_BOT_TOKEN";
/// Slack replays are refused beyond this window (their guidance: 5 minutes).
const MAX_SKEW_S: i64 = 300;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub fn status(_state: &AppState) -> (bool, String) {
    let missing: Vec<&str> = [SIGNING_SECRET_ENV, BOT_TOKEN_ENV]
        .into_iter()
        .filter(|k| env(k).is_none())
        .collect();
    if missing.is_empty() {
        (true, "Events API + chat.postMessage".into())
    } else {
        (false, format!("missing {}", missing.join(", ")))
    }
}

/// `v0=` + hex(HMAC-SHA256(secret, "v0:{ts}:{body}")), compared byte for byte.
pub fn verify_signature(secret: &str, ts: &str, body: &[u8], signature: &str) -> bool {
    let Ok(ts_n) = ts.parse::<i64>() else {
        return false;
    };
    if (now_ms() / 1000 - ts_n).abs() > MAX_SKEW_S {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(b"v0:");
    mac.update(ts.as_bytes());
    mac.update(b":");
    mac.update(body);
    let expected = format!("v0={}", hex::encode(mac.finalize().into_bytes()));
    constant_eq(expected.as_bytes(), signature.as_bytes())
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `POST /channels/slack/events`.
pub async fn events(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let hdr = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    match env(SIGNING_SECRET_ENV) {
        Some(secret) => {
            if !verify_signature(
                &secret,
                &hdr("x-slack-request-timestamp"),
                &body,
                &hdr("x-slack-signature"),
            ) {
                return (StatusCode::UNAUTHORIZED, "bad slack signature").into_response();
            }
        }
        None if mock_on(&state) => {}
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("{SIGNING_SECRET_ENV} not set"),
            )
                .into_response();
        }
    }
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    match v.get("type").and_then(Value::as_str) {
        Some("url_verification") => {
            let challenge = v.get("challenge").and_then(Value::as_str).unwrap_or("");
            Json(json!({ "challenge": challenge })).into_response()
        }
        Some("event_callback") => {
            let event = v.get("event").cloned().unwrap_or(Value::Null);
            let ev_type = event.get("type").and_then(Value::as_str).unwrap_or("");
            let is_bot = event.get("bot_id").is_some()
                || event.get("subtype").is_some()
                || event.get("user").is_none();
            if !matches!(ev_type, "message" | "app_mention") || is_bot {
                return StatusCode::OK.into_response();
            }
            let channel = event
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let user = event
                .get("user")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let text = event
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let event_id = v
                .get("event_id")
                .and_then(Value::as_str)
                .map(|id| format!("slack:{id}"))
                .or_else(|| {
                    event
                        .get("ts")
                        .and_then(Value::as_str)
                        .map(|ts| format!("slack:{channel}:{ts}"))
                });
            let kind = match event.get("channel_type").and_then(Value::as_str) {
                Some("im") => "DM".to_owned(),
                _ => format!("#{channel}"),
            };
            let sender_name = user_name(&state, &user)
                .await
                .unwrap_or_else(|| user.clone());
            let state2 = state.clone();
            // Slack retries after 3 s without a 200; answer first, work after.
            tokio::spawn(async move {
                handle_inbound(
                    &state2,
                    Inbound {
                        platform: Platform::Slack,
                        external_chat_id: channel,
                        external_user_id: user,
                        sender_name: sender_name.clone(),
                        text,
                        event_key: event_id,
                        trusted_sender: true,
                        label: format!("Slack · {kind} · {sender_name}"),
                    },
                )
                .await;
            });
            StatusCode::OK.into_response()
        }
        _ => StatusCode::OK.into_response(),
    }
}

async fn user_name(state: &AppState, user: &str) -> Option<String> {
    let token = env(BOT_TOKEN_ENV)?;
    let v: Value = state
        .http
        .get("https://slack.com/api/users.info")
        .bearer_auth(token)
        .query(&[("user", user)])
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v["user"]["real_name"]
        .as_str()
        .or_else(|| v["user"]["name"].as_str())
        .map(str::to_owned)
}

pub async fn send(state: &AppState, channel: &str, text: &str) -> anyhow::Result<Value> {
    let token = env(BOT_TOKEN_ENV).ok_or_else(|| anyhow::anyhow!("{BOT_TOKEN_ENV} not set"))?;
    let v: Value = state
        .http
        .post("https://slack.com/api/chat.postMessage")
        .bearer_auth(token)
        .json(&json!({ "channel": channel, "text": text }))
        .send()
        .await?
        .json()
        .await?;
    if v.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!(
            "slack: {}",
            v.get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    Ok(json!({ "ts": v.get("ts").cloned().unwrap_or(Value::Null) }))
}
