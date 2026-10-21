//! Shadow-compare (design §4 stage B): per chat, does the homeserver say the same as the relay?
//! Message ids and count, member set, and per-user unread counts. Pure read; the report is the
//! gate for the read flip and for every ring (zero divergence for 7 days on ring 0).

use std::collections::{BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;

use arbos_core::doc::message_text;

use super::backfill::load_doc;
use super::client::Client;
use super::worker::room_spec;
use crate::state::AppState;

#[derive(Debug, Default, Clone, Serialize)]
pub struct ChatDiff {
    pub chat_id: String,
    pub project_id: Option<String>,
    pub room_id: Option<String>,
    /// Finished messages with text in the Loro doc.
    pub relay_messages: usize,
    /// Events the relay has recorded as mirrored.
    pub mirrored_messages: usize,
    /// Of the newest 100 relay messages, which are not in the room.
    pub missing: Vec<String>,
    pub members_relay: Vec<String>,
    pub members_room: Vec<String>,
    pub members_equal: bool,
    pub ok: bool,
    pub note: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct UnreadDiff {
    pub user_id: String,
    pub chat_id: String,
    pub relay: i64,
    pub homeserver: i64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Report {
    pub chats: usize,
    pub diverged: usize,
    pub no_room: usize,
    pub diffs: Vec<ChatDiff>,
    pub unread_diffs: Vec<UnreadDiff>,
    pub unread_checked: usize,
}

fn relay_message_ids(state: &AppState, chat_id: &str, project_id: Option<&str>) -> Vec<String> {
    let Ok(Some(doc)) = load_doc(state, chat_id, project_id) else {
        return Vec::new();
    };
    doc.projected_rows()
        .iter()
        .filter(|r| !matches!(r["status"].as_str(), Some("queued" | "streaming")))
        .filter(|r| !message_text(r).trim().is_empty())
        .filter_map(|r| r["id"].as_str().map(str::to_owned))
        .collect()
}

/// Compare one chat. `only_chat` limits the run; `None` compares every mirrored room.
pub async fn run(state: &AppState, only_chat: Option<&str>, with_unread: bool) -> Report {
    let client = Client::new(state.http.clone(), state.matrix.cfg.clone());
    let cfg = &state.matrix.cfg;
    let rooms = state.db.matrix_rooms_all().unwrap_or_default();
    let mut rep = Report::default();
    let mut room_of_chat: HashMap<String, String> = HashMap::new();
    for room in rooms.iter().filter(|r| r.closed_at.is_none()) {
        if only_chat.is_some_and(|c| c != room.chat_id) {
            continue;
        }
        rep.chats += 1;
        room_of_chat.insert(room.chat_id.clone(), room.room_id.clone());
        let mut d = ChatDiff {
            chat_id: room.chat_id.clone(),
            project_id: room.project_id.clone(),
            room_id: Some(room.room_id.clone()),
            ..Default::default()
        };
        let ids = relay_message_ids(state, &room.chat_id, room.project_id.as_deref());
        d.relay_messages = ids.len();
        let mirrored: BTreeSet<String> = state
            .db
            .matrix_events_of(&room.chat_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(m, _, _)| m)
            .collect();
        d.mirrored_messages = mirrored.len();
        d.missing = ids
            .iter()
            .rev()
            .take(100)
            .filter(|id| !mirrored.contains(*id))
            .cloned()
            .collect();
        match room_spec(state, &room.chat_id, room.project_id.as_deref()) {
            Some(spec) => {
                d.members_relay = spec.members.iter().map(|m| m.mxid.clone()).collect();
                d.members_relay.sort();
                match client.joined_members(&room.room_id, &spec.creator).await {
                    Ok(mut joined) => {
                        joined.retain(|m| cfg.member_of_mxid(m).is_some());
                        joined.sort();
                        d.members_room = joined;
                    }
                    Err(e) => d.note = Some(format!("joined_members: {e}")),
                }
                // The newest event in the room should be the newest the relay knows.
                if d.note.is_none() && !ids.is_empty() {
                    match client.messages_back(&room.room_id, &spec.creator, 20).await {
                        Ok(chunk) => {
                            let newest_room = chunk
                                .iter()
                                .filter(|e| e["type"] == "m.room.message")
                                .find_map(|e| {
                                    e["content"]["life.arbos.id"].as_str().map(str::to_owned)
                                });
                            if newest_room.as_deref() != ids.last().map(String::as_str)
                                && d.missing.is_empty()
                            {
                                d.note = Some(format!(
                                    "newest in room {:?} vs relay {:?}",
                                    newest_room,
                                    ids.last()
                                ));
                            }
                        }
                        Err(e) => d.note = Some(format!("messages: {e}")),
                    }
                }
            }
            None => d.note = Some("chat gone on the relay, room still open".into()),
        }
        d.members_equal = d.members_relay == d.members_room;
        d.ok = d.missing.is_empty() && d.members_equal && d.note.is_none();
        if !d.ok {
            rep.diverged += 1;
        }
        rep.diffs.push(d);
    }
    if only_chat.is_none() {
        // Chats with history but no room yet.
        for u in state.db.list_users().unwrap_or_default() {
            for cm in state
                .db
                .chats_of_member(&format!("user:{}", u.id))
                .unwrap_or_default()
            {
                if !room_of_chat.contains_key(&cm.chat.id)
                    && !relay_message_ids(state, &cm.chat.id, None).is_empty()
                {
                    room_of_chat.insert(cm.chat.id.clone(), String::new());
                    rep.no_room += 1;
                }
            }
        }
    }
    if with_unread {
        for u in state.db.list_users().unwrap_or_default() {
            let relay: HashMap<String, i64> = state
                .db
                .unread_for_user(&u.id)
                .unwrap_or_default()
                .into_iter()
                .map(|c| (c.chat_id, c.unread))
                .collect();
            let Ok(hs) = client.unread_counts(&cfg.user_mxid(&u.id)).await else {
                continue;
            };
            for (room_id, notif, _hl) in hs {
                let Some(chat) = state.db.matrix_room_by_room_id(&room_id).ok().flatten() else {
                    continue;
                };
                if only_chat.is_some_and(|c| c != chat.chat_id) {
                    continue;
                }
                rep.unread_checked += 1;
                let r = relay.get(&chat.chat_id).copied().unwrap_or(0);
                // Synapse may lag by one during a write (design §4 stage B table).
                if !(r == notif || notif + 1 == r) {
                    rep.unread_diffs.push(UnreadDiff {
                        user_id: u.id.clone(),
                        chat_id: chat.chat_id,
                        relay: r,
                        homeserver: notif,
                    });
                }
            }
        }
    }
    rep
}

pub fn to_json(rep: &Report) -> Value {
    serde_json::to_value(rep).unwrap_or(Value::Null)
}
