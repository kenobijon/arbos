//! Room health (#1470 slice 3, from #1518): is a chat's history on the relay whole, and do the
//! replicas agree with it? Arbonites showed a room can be *silently* short — a stale checkpoint
//! deleted rows, later rows were stored but parked by Loro as pending, `export` answered a short
//! list with no warning and the agents never saw their mentions. One call tells the truth:
//!
//! `GET /chats/{id}/health` (members of the chat, or the admin) →
//! head/floor, the checkpoint, stored vs pending rows, projected vs announced vs mirrored message
//! counts, what each replica last told the relay (HELLO cursor, catch-up, history heals), and a
//! `divergent` flag with short reasons. The rebuilt doc is cached per room head.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use arbos_core::doc::ChatDoc;
use arbos_core::ids::now_ms;

use crate::auth::Principal;
use crate::people;
use crate::signin::api_error;
use crate::state::AppState;

/// What one replica (device or box) last told the relay about a room.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReplicaInfo {
    /// Cursor the replica announced in HELLO (what it had before catch-up).
    pub hello_cursor: i64,
    /// Row it was walked to by its last ROWS_DONE.
    pub caught_up_to: i64,
    pub last_seen_at: i64,
    /// History heals pushed (#1540): count and bytes — ops the replica held and the relay lacked.
    pub heals: u32,
    pub heal_bytes: u64,
    pub last_heal_at: i64,
}

/// Replica facts per room, in memory (reset by a relay restart; the next HELLO refills them).
pub type Replicas = std::sync::Mutex<HashMap<String, ReplicaInfo>>;

pub fn note_hello(room: &crate::state::Room, device: &str, cursor: i64) {
    let mut r = room.replicas.lock().unwrap();
    let e = r.entry(device.to_owned()).or_default();
    e.hello_cursor = cursor;
    e.last_seen_at = now_ms();
}

pub fn note_caught_up(room: &crate::state::Room, device: &str, seq: i64) {
    let mut r = room.replicas.lock().unwrap();
    let e = r.entry(device.to_owned()).or_default();
    e.caught_up_to = e.caught_up_to.max(seq);
    e.last_seen_at = now_ms();
}

pub fn note_heal(room: &crate::state::Room, device: &str, bytes: usize) {
    let mut r = room.replicas.lock().unwrap();
    let e = r.entry(device.to_owned()).or_default();
    e.heals += 1;
    e.heal_bytes += bytes as u64;
    e.last_heal_at = now_ms();
    e.last_seen_at = e.last_heal_at;
}

/// The rebuilt doc's verdict on the stored rows.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DocHealth {
    pub head: i64,
    pub floor: i64,
    pub checkpoint_seq: i64,
    pub checkpoint_at: i64,
    pub checkpoint_size: i64,
    pub rows_stored: i64,
    /// Rows Loro parked (their deps are not in the checkpoint or the rows before them).
    pub rows_pending: i64,
    pub first_pending_seq: Option<i64>,
    /// Rows that failed to decode at all.
    pub rows_bad: i64,
    /// Messages in the rebuilt doc (what `export` and a fresh device see).
    pub projected: i64,
}

/// Rebuild checkpoint + rows and count what applies. Cached on the room by head.
pub fn doc_health(state: &AppState, room: &crate::state::Room, room_name: &str) -> anyhow::Result<DocHealth> {
    let st = state.db.room_state(room_name)?;
    if let Some((h, cached)) = room.health_cache.lock().unwrap().as_ref() {
        if *h == st.head {
            return Ok(cached.clone());
        }
    }
    let (checkpoint_at, mut doc) = match state.db.checkpoint_full(room_name)? {
        Some((_, at, bytes)) => (at, ChatDoc::from_snapshot(None, false, &bytes)?),
        None => (0, ChatDoc::new(None, false)),
    };
    let mut after = 0;
    let mut pending = 0;
    let mut bad = 0;
    let mut first_pending = None;
    let mut stored = 0;
    loop {
        let rows = state.db.rows_after(room_name, after, None, 256)?;
        let n = rows.len();
        for r in rows {
            after = r.seq;
            stored += 1;
            match doc.import(&r.bytes) {
                Ok(status) if status.pending.is_some() => {
                    pending += 1;
                    first_pending.get_or_insert(r.seq);
                }
                Ok(_) => {}
                Err(_) => bad += 1,
            }
        }
        if n < 256 {
            break;
        }
    }
    let h = DocHealth {
        head: st.head,
        floor: st.floor,
        checkpoint_seq: st.checkpoint_seq,
        checkpoint_at,
        checkpoint_size: st.checkpoint_size,
        rows_stored: stored,
        rows_pending: pending,
        first_pending_seq: first_pending,
        rows_bad: bad,
        projected: doc.messages_list().len() as i64,
    };
    *room.health_cache.lock().unwrap() = Some((st.head, h.clone()));
    Ok(h)
}

/// Lines the relay knows about that are not user/agent messages (join notices, system lines):
/// `chat_events` counts only announced messages, the mirror carries system lines too, so the
/// three counters agree only within this margin.
const SYSTEM_LINE_MARGIN: i64 = 25;

/// The full health block for one chat.
pub async fn health_of(state: &AppState, chat_id: &str, room_name: &str) -> Value {
    let room = state.room(room_name).await;
    let doc = match doc_health(state, &room, room_name) {
        Ok(d) => d,
        Err(e) => return json!({ "chat_id": chat_id, "error": e.to_string() }),
    };
    let announced = state.db.chat_events_count(chat_id).unwrap_or(0);
    let mirror = state.db.matrix_events_count(chat_id).unwrap_or(0);
    let replicas: Vec<Value> = {
        let r = room.replicas.lock().unwrap();
        let mut v: Vec<(String, ReplicaInfo)> = r.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        v.sort_by(|a, b| b.1.last_seen_at.cmp(&a.1.last_seen_at));
        v.into_iter()
            .map(|(device, i)| {
                json!({
                    "device": device,
                    "hello_cursor": i.hello_cursor,
                    "caught_up_to": i.caught_up_to,
                    "rows_behind": (doc.head - i.caught_up_to.max(i.hello_cursor)).max(0),
                    "last_seen_at": i.last_seen_at,
                    "heals": i.heals,
                    "ops_beyond_relay_bytes": i.heal_bytes,
                    "last_heal_at": if i.last_heal_at > 0 { Some(i.last_heal_at) } else { None },
                })
            })
            .collect()
    };
    let mut reasons: Vec<String> = Vec::new();
    if doc.rows_pending > 0 {
        reasons.push(format!(
            "{} of {} stored rows do not apply to the rebuilt doc (first seq {}): their ops' dependencies are not on the relay",
            doc.rows_pending,
            doc.rows_stored,
            doc.first_pending_seq.unwrap_or_default()
        ));
    }
    if doc.rows_bad > 0 {
        reasons.push(format!("{} stored rows do not decode", doc.rows_bad));
    }
    if doc.projected + SYSTEM_LINE_MARGIN < announced {
        reasons.push(format!(
            "announced messages ({announced}) exceed the rebuilt doc ({}): messages were lost from the relay's history",
            doc.projected
        ));
    }
    if doc.projected + SYSTEM_LINE_MARGIN < mirror {
        reasons.push(format!(
            "the Matrix mirror ({mirror}) has more messages than the rebuilt doc ({})",
            doc.projected
        ));
    }
    let healed_recently = replicas
        .iter()
        .any(|r| r["heals"].as_u64().unwrap_or(0) > 0);
    if healed_recently && doc.rows_pending == 0 {
        reasons.push("a replica pushed history the relay lacked; the doc is whole now".into());
    }
    let divergent = doc.rows_pending > 0
        || doc.rows_bad > 0
        || doc.projected + SYSTEM_LINE_MARGIN < announced
        || doc.projected + SYSTEM_LINE_MARGIN < mirror;
    json!({
        "chat_id": chat_id,
        "room": room_name,
        "head_seq": doc.head,
        "floor": doc.floor,
        "checkpoint": if doc.checkpoint_seq > 0 {
            json!({ "seq": doc.checkpoint_seq, "at": doc.checkpoint_at, "size": doc.checkpoint_size })
        } else {
            Value::Null
        },
        "rows": { "stored": doc.rows_stored, "pending": doc.rows_pending, "first_pending_seq": doc.first_pending_seq, "bad": doc.rows_bad },
        "messages": { "projected": doc.projected, "announced": announced, "mirror": mirror, "margin": SYSTEM_LINE_MARGIN },
        "replicas": replicas,
        "divergent": divergent,
        "reasons": reasons,
        "checked_at": now_ms(),
        "rule": "divergent = stored rows that do not apply, or fewer projected messages than announced/mirrored (beyond the system-line margin). `export` is the projected doc; pending rows exist on the relay but no fresh device can show them until a replica that holds their dependencies reconnects (history heal, #1540).",
    })
}

/// `GET /chats/{id}/health` — people chats by membership, project chats by project access.
pub async fn chat_health(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
) -> Response {
    let room = if state.db.people_chat(&id).ok().flatten().is_some() {
        if !people::room_access(&state, &principal, &id) {
            return api_error(StatusCode::FORBIDDEN, "not_member", "you are not in this chat");
        }
        people::room_of(&id)
    } else {
        match state.db.chat_project_id(&id).ok().flatten() {
            Some(project) if state.allows(&principal, &project) => {
                arbos_core::wire::room_name(&project, &id)
            }
            Some(_) => return api_error(StatusCode::FORBIDDEN, "forbidden", "not your chat"),
            None => return api_error(StatusCode::NOT_FOUND, "not_found", "no such chat"),
        }
    };
    Json(health_of(&state, &id, &room).await).into_response()
}
