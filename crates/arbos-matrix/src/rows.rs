//! What the apps render, built from room state and events. [`MessageRow`] is the chat-doc
//! message shape (`crdt-sync.md` §6.2: `id`, `role`, `sender`, `status`, `parts`, `createdAt`)
//! so the existing transcript draws a channel like any chat; [`ChannelRow`] / [`SpaceRow`] feed
//! the left list.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One message as the transcript expects it (JSON in `json`, the keys the apps already read).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRow {
    pub event_id: String,
    /// `life.arbos.id` when the relay or an app wrote it, else the event id.
    pub id: String,
    /// `user:<id>` | `agent:<project>` | `system`.
    pub sender: String,
    pub sender_name: String,
    pub text: String,
    pub created_at: i64,
    /// Full row for the transcript (same shape as a chat-doc message).
    pub json: String,
}

/// A channel in the left list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelRow {
    pub room_id: String,
    /// The relay's channel id (`life.arbos.chat.chat_id`).
    pub channel_id: String,
    pub server_id: String,
    pub name: String,
    /// Only owner/admins post (lock in the list).
    pub restricted: bool,
    /// Can this device post here (its power level meets `events_default`).
    pub can_post: bool,
    pub unread: u64,
    pub last_text: String,
    pub last_sender_name: String,
    pub last_ts: i64,
    pub order: String,
}

/// A server (Space) in the left list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceRow {
    pub room_id: String,
    pub server_id: String,
    pub name: String,
    pub member_count: u64,
    pub unread: u64,
    pub last_text: String,
    pub last_sender_name: String,
    pub last_ts: i64,
    pub channel_ids: Vec<String>,
}

/// `@gh_1:arbos.life` → `user:gh_1`; `@_a_<ulid>` → `agent:<ULID>`; anything else → `system`.
pub fn member_of_mxid(mxid: &str) -> String {
    let local = mxid
        .trim_start_matches('@')
        .split(':')
        .next()
        .unwrap_or_default();
    if let Some(p) = local.strip_prefix("_a_") {
        format!("agent:{}", p.to_ascii_uppercase())
    } else if local.starts_with('_') || local.is_empty() {
        "system".to_owned()
    } else {
        format!("user:{local}")
    }
}

/// Build a [`MessageRow`] from one `m.room.message` event (JSON as the server sent it).
/// `display_name` resolves the sender's name; `me` is this device's member string so the
/// transcript can tell own messages apart (`role: user` for people, `assistant` for agents).
pub fn message_row(
    ev: &Value,
    display_name: &dyn Fn(&str) -> Option<String>,
) -> Option<MessageRow> {
    if ev["type"].as_str() != Some("m.room.message") {
        return None;
    }
    let content = &ev["content"];
    let event_id = ev["event_id"].as_str()?.to_owned();
    let mxid = ev["sender"].as_str().unwrap_or_default();
    let sender = content["life.arbos.sender"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| member_of_mxid(mxid));
    let id = content["life.arbos.id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| event_id.clone());
    let text = content["body"].as_str().unwrap_or_default().to_owned();
    let created_at = ev["origin_server_ts"].as_i64().unwrap_or(0);
    let role = if sender.starts_with("agent:") {
        "assistant"
    } else if sender == "system" {
        "system"
    } else {
        "user"
    };
    // A system line (membership notice, #1386) is nobody's message: no sender
    // name, so the channel list shows "Mira K. added Sam Rivera", not
    // "launchcrew: Mira K. added Sam Rivera" (the owner only carried it).
    let sender_name = if role == "system" {
        String::new()
    } else {
        display_name(mxid).unwrap_or_else(|| {
            mxid.trim_start_matches('@')
                .split(':')
                .next()
                .unwrap_or_default()
                .to_owned()
        })
    };
    let handle = match sender.strip_prefix("user:") {
        Some(_) => sender_name.clone(),
        None => String::new(),
    };
    let mut parts = vec![json!({ "id": format!("{event_id}#text"), "kind": "text", "text": text })];
    if let Some(extra) = content["life.arbos.parts"].as_array() {
        parts.extend(extra.iter().cloned());
    }
    let row = json!({
        "id": id,
        "eventId": event_id,
        "role": role,
        "sender": sender,
        "senderHandle": handle,
        "senderName": sender_name,
        "status": "done",
        "createdAt": created_at,
        "completedAt": created_at,
        "parts": parts,
        "reactions": {},
    });
    Some(MessageRow {
        event_id,
        id,
        sender,
        sender_name,
        text,
        created_at,
        json: row.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mxid_to_member() {
        assert_eq!(member_of_mxid("@gh_12:arbos.life"), "user:gh_12");
        assert_eq!(member_of_mxid("@_a_01jabc:arbos.life"), "agent:01JABC");
        assert_eq!(member_of_mxid("@_arbos:arbos.life"), "system");
    }

    #[test]
    fn row_from_event() {
        let ev = json!({
            "type": "m.room.message", "event_id": "$e1", "sender": "@gh_7:arbos.life", "origin_server_ts": 1700,
            "content": { "msgtype": "m.text", "body": "hi", "life.arbos.id": "01ULID" }
        });
        let row = message_row(&ev, &|m| {
            (m == "@gh_7:arbos.life").then(|| "kenobi".to_owned())
        })
        .unwrap();
        assert_eq!(row.id, "01ULID");
        assert_eq!(row.sender, "user:gh_7");
        assert_eq!(row.sender_name, "kenobi");
        let v: Value = serde_json::from_str(&row.json).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(v["parts"][0]["text"], "hi");
        assert_eq!(v["senderHandle"], "kenobi");
        let agent = json!({
            "type": "m.room.message", "event_id": "$e2", "sender": "@_a_01x:arbos.life", "origin_server_ts": 1800,
            "content": { "msgtype": "m.text", "body": "done" }
        });
        let row = message_row(&agent, &|_| None).unwrap();
        assert_eq!(row.sender, "agent:01X");
        let v: Value = serde_json::from_str(&row.json).unwrap();
        assert_eq!(v["role"], "assistant");
        assert!(message_row(&json!({ "type": "m.room.member" }), &|_| None).is_none());
        // A membership notice: the owner sent it, the content names the system.
        let notice = json!({
            "type": "m.room.message", "event_id": "$e3", "sender": "@gh_7:arbos.life", "origin_server_ts": 1900,
            "content": { "msgtype": "m.notice", "body": "Mira K. added Sam Rivera", "life.arbos.sender": "system", "life.arbos.id": "01N" }
        });
        let row = message_row(&notice, &|_| Some("launchcrew".to_owned())).unwrap();
        assert_eq!(row.sender, "system");
        assert_eq!(row.sender_name, "", "a system line carries no sender name");
        let v: Value = serde_json::from_str(&row.json).unwrap();
        assert_eq!(v["role"], "system");
        assert_eq!(v["senderHandle"], "");
    }
}
