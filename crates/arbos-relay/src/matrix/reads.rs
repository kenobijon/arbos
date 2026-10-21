//! Stage B (design §4): the per-user read flip and the push path through the homeserver.
//!
//! A user with `users.matrix_reads = 1` (and `ARBOS_MATRIX=on`) gets unread counts and the badge
//! from Synapse's own `notification_count`s instead of `chat_events`, and — when APNs is on —
//! alerts from the push gateway (`POST /_matrix/push/v1/notify`) instead of `notify::announce`.
//! Rollback is the flag off: nothing to restore, both stores keep writing.

use serde_json::{json, Value};

use super::client::{Client, MxResult};
use super::Mode;
use crate::db_push::UnreadChat;
use crate::notify::{ChatRef, Sender};
use crate::state::AppState;

/// `app_id` of the relay's pusher on every flipped device.
pub const PUSHER_APP_ID: &str = "life.arbos.relay";

/// Are this user's reads served from the homeserver?
pub fn reads_on(state: &AppState, user_id: &str) -> bool {
    state.matrix.cfg.mode == Mode::On
        && state
            .db
            .matrix_reads_users()
            .map(|u| u.iter().any(|x| x == user_id))
            .unwrap_or(false)
}

/// Unread per chat as Synapse counts it, mapped back to relay chat ids. `None` when the
/// homeserver did not answer (the caller falls back to the relay's own counts).
pub async fn unread_for_user(state: &AppState, user_id: &str) -> Option<Vec<UnreadChat>> {
    let client = Client::new(state.http.clone(), state.matrix.cfg.clone());
    let mxid = state.matrix.cfg.user_mxid(user_id);
    let counts = match client.unread_counts(&mxid).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(user = %user_id, error = %e, "matrix reads: unread from homeserver failed; relay counts used");
            return None;
        }
    };
    let mut out = Vec::new();
    for (room_id, notif, _) in counts {
        let Some(room) = state.db.matrix_room_by_room_id(&room_id).ok().flatten() else {
            continue;
        };
        // Archived chats never count (same rule as the relay's own query).
        if state
            .db
            .chat_archived_at(user_id, &room.chat_id)
            .ok()
            .flatten()
            .is_some()
        {
            continue;
        }
        if notif > 0 {
            out.push(UnreadChat {
                chat_id: room.chat_id,
                project_id: room.project_id,
                unread: notif,
            });
        }
    }
    Some(out)
}

/// Total unread for the badge, from the homeserver when flipped, else the relay.
pub async fn unread_total(state: &AppState, user_id: &str) -> i64 {
    if reads_on(state, user_id) {
        if let Some(chats) = unread_for_user(state, user_id).await {
            return chats.iter().map(|c| c.unread).sum();
        }
    }
    state.db.unread_total(user_id).unwrap_or(0)
}

/// Register (or remove) the relay as push gateway on every APNs device of a user. Called from
/// the worker on the `Pushers` job; the `pushkey` is the Arbos device id.
pub async fn set_pushers(
    state: &AppState,
    client: &Client,
    user_id: &str,
    on: bool,
) -> MxResult<()> {
    let mxid = state.matrix.cfg.user_mxid(user_id);
    let url = format!(
        "{}/_matrix/push/v1/notify",
        state.cfg.public_url.trim_end_matches('/')
    );
    for t in state.db.push_tokens_of_user(user_id).unwrap_or_default() {
        let body = if on {
            json!({
                "app_display_name": "Arbos", "app_id": PUSHER_APP_ID, "append": false,
                "device_display_name": t.device_id, "kind": "http", "lang": "en",
                "pushkey": t.device_id, "data": { "url": url, "format": "event_id_only" },
            })
        } else {
            json!({ "app_id": PUSHER_APP_ID, "kind": null, "pushkey": t.device_id })
        };
        client.set_pusher(&mxid, &body).await?;
    }
    Ok(())
}

/// One `notify` call from Synapse → APNs alerts for the flipped devices it names. Returns the
/// pushkeys that must be rejected (no such device any more) so Synapse drops the pusher.
pub async fn on_notify(state: &AppState, body: &Value) -> Vec<String> {
    let n = &body["notification"];
    let mut rejected = Vec::new();
    if !state.push.is_on() {
        return rejected;
    }
    let (Some(room_id), Some(event_id)) = (n["room_id"].as_str(), n["event_id"].as_str()) else {
        return rejected;
    };
    let Some(room) = state.db.matrix_room_by_room_id(room_id).ok().flatten() else {
        return rejected;
    };
    let client = Client::new(state.http.clone(), state.matrix.cfg.clone());
    let chat = ChatRef {
        chat_id: room.chat_id.clone(),
        project_id: room.project_id.clone(),
    };
    for d in n["devices"].as_array().into_iter().flatten() {
        if d["app_id"].as_str() != Some(PUSHER_APP_ID) {
            continue;
        }
        let Some(device_id) = d["pushkey"].as_str() else {
            continue;
        };
        let Some(user_id) = state.db.device_user(device_id).ok().flatten() else {
            rejected.push(device_id.to_owned());
            continue;
        };
        if !reads_on(state, &user_id) {
            // Flag went off: today's path notifies; Synapse stops calling once rejected.
            rejected.push(device_id.to_owned());
            continue;
        }
        // `event_id_only`: fetch the event as the recipient to build the same alert as today.
        let mxid = state.matrix.cfg.user_mxid(&user_id);
        let Ok(ev) = client.event(room_id, &mxid, event_id).await else {
            continue;
        };
        let content = &ev["content"];
        let Some(sender) = sender_of_event(state, &ev) else {
            continue;
        };
        if sender.user_id.as_deref() == Some(user_id.as_str()) {
            continue;
        }
        let message_id = content["life.arbos.id"].as_str().unwrap_or(event_id);
        let text = content["body"].as_str().unwrap_or("New message");
        let badge = unread_total(state, &user_id).await;
        crate::notify::alert_device(state, &chat, &sender, message_id, text, badge, device_id)
            .await;
    }
    rejected
}

/// The relay `Sender` behind a Matrix event: `life.arbos.sender` when the relay wrote it,
/// else the Matrix sender mapped back.
fn sender_of_event(state: &AppState, ev: &Value) -> Option<Sender> {
    let member = ev["content"]["life.arbos.sender"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            ev["sender"]
                .as_str()
                .and_then(|s| state.matrix.cfg.member_of_mxid(s))
        })?;
    if let Some(u) = member.strip_prefix("user:") {
        return Some(Sender::user(state, u));
    }
    let p = member.strip_prefix("agent:")?;
    let project = state.db.project(p).ok().flatten()?;
    Some(Sender::agent(state, &project))
}
