//! Relay-backed source: one WebSocket per chat room on `arbos-relay`
//! (`crdt-sync.md` §7–§8). Frames come from `arbos_core::wire`; the chat doc
//! is the same Loro schema `arbos-core::doc` writes on the box side.
//!
//! Registry stub: the project list and chat index come from a JSON file
//! (`ARBOS_RELAY_SEED`) until the relay's `GET /projects` exists. The
//! checkpoint fetch (`GET /room/…/checkpoint`) is also not wired yet: a room
//! whose rows were trimmed below our cursor backfills from seq 0.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use arbos_core::wire::{
    Ack, BoxState as WireBox, ErrorHeader, Frame, FrameType, Hello, RowHeader, RowsReq,
    State as WireState, StatusHeader, WakeHeader,
};
use arbos_core::{wire, PROTO_VERSION};
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, timeout, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use super::doc::ChatDoc;
use super::{ClientCommand, SyncEvent, SyncSource};
use crate::model::{
    new_id, now_ms, Attachment, BoxState, Chat, Id, Link, Message, MessageStatus, Part, Project,
    Role,
};

const HELLO_DEADLINE: Duration = Duration::from_secs(15);
const PRESENCE_EVERY: Duration = Duration::from_secs(15);
const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(16);
const BACKOFF_RESET_AFTER: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Deserialize)]
pub struct RelaySeed {
    pub projects: Vec<Project>,
    pub chats: Vec<Chat>,
}

pub struct RelaySource {
    ws_url: String,
    token: String,
    device_id: String,
    seed: RelaySeed,
}

impl RelaySource {
    pub fn from_env(ws_url: String) -> Self {
        let token = std::env::var("ARBOS_RELAY_TOKEN").unwrap_or_default();
        let device_id =
            std::env::var("ARBOS_DEVICE_ID").unwrap_or_else(|_| format!("mac:{}", new_id()));
        let seed = std::env::var("ARBOS_RELAY_SEED")
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(default_seed);
        Self {
            ws_url,
            token,
            device_id,
            seed,
        }
    }
}

fn default_seed() -> RelaySeed {
    let project_id = std::env::var("ARBOS_PROJECT_ID").unwrap_or_else(|_| new_id());
    let chat_id = std::env::var("ARBOS_CHAT_ID").unwrap_or_else(|_| new_id());
    RelaySeed {
        projects: vec![Project {
            id: project_id.clone(),
            name: "Arbos".into(),
            icon: "Ω".into(),
            color: "#8b7cf6".into(),
            state: BoxState::HibernatedLight,
            state_detail: None,
            wake_progress: None,
            model: "GPT-6.1-Sol".into(),
            effort: crate::model::Effort::High,
            spend_today_usd: 0.0,
            spend_cap_usd: 25.0,
        }],
        chats: vec![Chat {
            id: chat_id,
            project_id,
            title: "Arbos".into(),
            is_pinned: true,
            is_system: false,
            unread: false,
            last_message_at: now_ms(),
        }],
    }
}

enum RoomCmd {
    Wake(&'static str),
}

struct Room {
    cmds: mpsc::UnboundedSender<RoomCmd>,
}

struct Shared {
    docs: Mutex<HashMap<Id, ChatDoc>>,
    rooms: Mutex<HashMap<Id, Room>>,
    projects: Mutex<Vec<Project>>,
    chats: Mutex<Vec<Chat>>,
    /// Client-minted message ids whose push the relay has not `ACK`ed (S3).
    unacked: Mutex<HashSet<Id>>,
    events: mpsc::UnboundedSender<SyncEvent>,
    subscriptions: Mutex<Vec<loro::Subscription>>,
    ws_url: String,
    token: String,
    device_id: String,
}

impl SyncSource for RelaySource {
    fn name(&self) -> &'static str {
        "relay"
    }

    fn run(
        self: Box<Self>,
        events: mpsc::UnboundedSender<SyncEvent>,
        mut commands: mpsc::UnboundedReceiver<ClientCommand>,
    ) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let shared = Arc::new(Shared {
                docs: Mutex::new(HashMap::new()),
                rooms: Mutex::new(HashMap::new()),
                projects: Mutex::new(self.seed.projects.clone()),
                chats: Mutex::new(self.seed.chats.clone()),
                unacked: Mutex::new(HashSet::new()),
                events: events.clone(),
                subscriptions: Mutex::new(Vec::new()),
                ws_url: self.ws_url,
                token: self.token,
                device_id: self.device_id,
            });
            let _ = events.send(SyncEvent::Link(Link::Connecting));
            let _ = events.send(SyncEvent::Projects(self.seed.projects.clone()));
            emit_chats(&shared);
            for chat in &self.seed.chats {
                open_room(&shared, &chat.project_id, &chat.id);
            }

            while let Some(cmd) = commands.recv().await {
                handle_command(&shared, cmd);
            }
            Ok(())
        })
    }
}

fn emit_chats(shared: &Shared) {
    let chats = shared.chats.lock().unwrap().clone();
    let mut by_project: HashMap<Id, Vec<Chat>> = HashMap::new();
    for chat in chats {
        by_project
            .entry(chat.project_id.clone())
            .or_default()
            .push(chat);
    }
    for (project_id, chats) in by_project {
        let _ = shared.events.send(SyncEvent::Chats { project_id, chats });
    }
}

fn apply_pending(shared: &Shared, message: &mut Message) {
    if message.role == Role::User {
        message.pending = shared.unacked.lock().unwrap().contains(&message.id);
    }
}

fn emit_snapshot(shared: &Shared, chat_id: &str) {
    let docs = shared.docs.lock().unwrap();
    let Some(doc) = docs.get(chat_id) else { return };
    let mut messages = doc.read_all();
    for m in &mut messages {
        apply_pending(shared, m);
    }
    let _ = shared.events.send(SyncEvent::ChatSnapshot {
        chat_id: chat_id.to_string(),
        messages,
    });
}

fn open_room(shared: &Arc<Shared>, project_id: &str, chat_id: &str) {
    let doc = ChatDoc::empty(chat_id);
    let (local_tx, local_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (change_tx, change_rx) = mpsc::unbounded_channel::<super::doc::Touched>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<RoomCmd>();

    let sub = doc.doc.subscribe_root(Arc::new(move |event| {
        let touched = ChatDoc::touched(&event);
        if touched.list_changed || !touched.messages.is_empty() {
            let _ = change_tx.send(touched);
        }
    }));
    let local_sub = doc
        .doc
        .subscribe_local_update(Box::new(move |bytes| local_tx.send(bytes.clone()).is_ok()));
    {
        let mut subs = shared.subscriptions.lock().unwrap();
        subs.push(sub);
        subs.push(local_sub);
    }
    shared.docs.lock().unwrap().insert(chat_id.to_string(), doc);
    shared
        .rooms
        .lock()
        .unwrap()
        .insert(chat_id.to_string(), Room { cmds: cmd_tx });

    let shared2 = shared.clone();
    let chat = chat_id.to_string();
    tokio::spawn(forward_changes(shared2, chat, change_rx));

    let shared3 = shared.clone();
    let project = project_id.to_string();
    let chat = chat_id.to_string();
    tokio::spawn(async move {
        room_loop(shared3, project, chat, local_rx, cmd_rx).await;
    });
}

async fn forward_changes(
    shared: Arc<Shared>,
    chat_id: Id,
    mut rx: mpsc::UnboundedReceiver<super::doc::Touched>,
) {
    while let Some(touched) = rx.recv().await {
        if touched.list_changed {
            emit_snapshot(&shared, &chat_id);
            continue;
        }
        let docs = shared.docs.lock().unwrap();
        let Some(doc) = docs.get(&chat_id) else {
            continue;
        };
        for ix in touched.messages {
            if let Some(mut message) = doc.read_message(ix) {
                apply_pending(&shared, &mut message);
                let _ = shared.events.send(SyncEvent::MessageUpsert {
                    chat_id: chat_id.clone(),
                    message,
                });
            }
        }
    }
}

/// Dial, handshake, backfill, then stream — forever, with backoff (§8.6).
async fn room_loop(
    shared: Arc<Shared>,
    project_id: Id,
    chat_id: Id,
    mut local_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut cmd_rx: mpsc::UnboundedReceiver<RoomCmd>,
) {
    let mut cursor: i64 = 0;
    let mut backoff = BACKOFF_MIN;
    // Local updates not yet acked, replayed first on every redial (§7.4).
    let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
    loop {
        let connected_at = Instant::now();
        let outcome = run_session(
            &shared,
            &project_id,
            &chat_id,
            &mut cursor,
            &mut pending,
            &mut local_rx,
            &mut cmd_rx,
        )
        .await;
        if let Err(err) = outcome {
            eprintln!("relay room {project_id}/{chat_id}: {err:#}");
        }
        let _ = shared.events.send(SyncEvent::Link(Link::Reconnecting));
        if connected_at.elapsed() > BACKOFF_RESET_AFTER {
            backoff = BACKOFF_MIN;
        }
        sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn run_session(
    shared: &Arc<Shared>,
    project_id: &str,
    chat_id: &str,
    cursor: &mut i64,
    pending: &mut Vec<(String, Vec<u8>)>,
    local_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    cmd_rx: &mut mpsc::UnboundedReceiver<RoomCmd>,
) -> Result<()> {
    let room = wire::room_name(project_id, chat_id);
    let url = format!(
        "{}/room/{}?token={}",
        shared.ws_url.trim_end_matches('/'),
        room,
        shared.token
    );
    let (ws, _) = timeout(HELLO_DEADLINE, connect_async(&url))
        .await
        .context("dial timed out")??;
    let (mut tx, mut rx) = ws.split();

    let hello = Hello {
        cursor: *cursor,
        device: shared.device_id.clone(),
        room: room.clone(),
        proto: PROTO_VERSION,
    };
    tx.send(binary(Frame::new(
        FrameType::Hello,
        serde_json::to_value(&hello)?,
        Vec::new(),
    )))
    .await?;

    let state = timeout(HELLO_DEADLINE, async {
        while let Some(msg) = rx.next().await {
            if let WsMessage::Binary(bytes) = msg? {
                if let Some(frame) = Frame::decode(bytes)? {
                    if frame.ty == FrameType::State {
                        let header: WireState = serde_json::from_value(frame.header)?;
                        return Ok::<_, anyhow::Error>(Some((header, frame.payload)));
                    }
                }
            }
        }
        Ok(None)
    })
    .await
    .context("STATE did not arrive")??
    .context("socket closed before STATE")?;
    let (state, frontier) = state;
    emit_box_status(shared, project_id, state.box_state, None);

    let contained = {
        let docs = shared.docs.lock().unwrap();
        match (docs.get(chat_id), arbos_core::doc::decode_vv(&frontier)) {
            (Some(doc), Some(vv)) => doc.doc.oplog_vv().includes_vv(&vv),
            (Some(_), None) | (None, _) => false,
        }
    };
    let after = if contained || state.checkpoint_seq == 0 {
        (*cursor).max(state.checkpoint_seq)
    } else {
        eprintln!(
            "relay room {room}: checkpoint {} not contained; checkpoint fetch is not wired, backfilling from 0",
            state.checkpoint_seq
        );
        0
    };
    tx.send(binary(Frame::new(
        FrameType::RowsReq,
        serde_json::to_value(RowsReq {
            after,
            exclude_own: true,
        })?,
        Vec::new(),
    )))
    .await?;

    for (batch_id, bytes) in pending.iter() {
        tx.send(binary(Frame::new(
            FrameType::Push,
            json!({ "batchId": batch_id }),
            bytes.clone(),
        )))
        .await?;
    }

    let mut presence = interval(PRESENCE_EVERY);
    loop {
        tokio::select! {
            msg = rx.next() => {
                let Some(msg) = msg else { anyhow::bail!("socket closed") };
                let WsMessage::Binary(bytes) = msg? else { continue };
                let Some(frame) = Frame::decode(bytes)? else { continue };
                match frame.ty {
                    FrameType::Row => {
                        let header: RowHeader = serde_json::from_value(frame.header)?;
                        let docs = shared.docs.lock().unwrap();
                        if let Some(doc) = docs.get(chat_id) {
                            if let Err(err) = doc.doc.import(&frame.payload) {
                                eprintln!("relay room {room}: import seq {} failed: {err}", header.seq);
                            }
                        }
                        *cursor = (*cursor).max(header.seq);
                    }
                    FrameType::RowsDone => {
                        let head_seq = frame.header.get("headSeq").and_then(|v| v.as_i64()).unwrap_or(*cursor);
                        *cursor = (*cursor).max(head_seq);
                        let _ = shared.events.send(SyncEvent::Link(Link::Live));
                        emit_snapshot(shared, chat_id);
                    }
                    FrameType::Ack => {
                        let header: Ack = serde_json::from_value(frame.header)?;
                        pending.retain(|(id, _)| *id != header.batch_id);
                        *cursor = (*cursor).max(header.seq);
                        let acked: Vec<Id> = shared.unacked.lock().unwrap().drain().collect();
                        if !acked.is_empty() {
                            emit_snapshot(shared, chat_id);
                        }
                    }
                    FrameType::Status => {
                        let header: StatusHeader = serde_json::from_value(frame.header)?;
                        emit_box_status(shared, project_id, header.box_state, header.detail);
                    }
                    FrameType::Error => {
                        let header: ErrorHeader = serde_json::from_value(frame.header)?;
                        if header.code == "lagging" {
                            tx.send(binary(Frame::new(
                                FrameType::RowsReq,
                                serde_json::to_value(RowsReq { after: *cursor, exclude_own: true })?,
                                Vec::new(),
                            ))).await?;
                        } else {
                            anyhow::bail!("relay error {}: {}", header.code, header.message);
                        }
                    }
                    FrameType::ProbeOk | FrameType::Presence | FrameType::State => {}
                    FrameType::Hello | FrameType::RowsReq | FrameType::Push | FrameType::Probe | FrameType::Wake => {
                        // Client→server frames echoed back are ignored.
                    }
                }
            }
            bytes = local_rx.recv() => {
                let Some(bytes) = bytes else { anyhow::bail!("doc dropped") };
                if bytes.len() > wire::ROW_MAX {
                    eprintln!("relay room {room}: local update {} bytes exceeds row cap; dropped", bytes.len());
                    continue;
                }
                let batch_id = new_id();
                tx.send(binary(Frame::new(
                    FrameType::Push,
                    json!({ "batchId": batch_id }),
                    bytes.clone(),
                ))).await?;
                pending.push((batch_id, bytes));
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { anyhow::bail!("room closed") };
                match cmd {
                    RoomCmd::Wake(reason) => {
                        tx.send(binary(Frame::new(
                            FrameType::Wake,
                            serde_json::to_value(WakeHeader { reason: reason.to_string() })?,
                            Vec::new(),
                        ))).await?;
                    }
                }
            }
            _ = presence.tick() => {
                tx.send(binary(Frame::new(
                    FrameType::Presence,
                    json!({ "device": shared.device_id, "at": now_ms() }),
                    Vec::new(),
                ))).await?;
            }
        }
    }
}

fn binary(frame: Frame) -> WsMessage {
    WsMessage::Binary(frame.encode())
}

fn emit_box_status(shared: &Shared, project_id: &str, state: WireBox, detail: Option<String>) {
    let mapped = match state {
        WireBox::Awake => BoxState::Awake,
        WireBox::Waking => BoxState::Waking,
        WireBox::Asleep => BoxState::HibernatedLight,
        WireBox::Error => BoxState::Error,
    };
    {
        let mut projects = shared.projects.lock().unwrap();
        if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
            p.state = mapped;
            p.state_detail = detail.clone();
            p.wake_progress = None;
        }
    }
    let _ = shared.events.send(SyncEvent::ProjectStatus {
        project_id: project_id.to_string(),
        state: mapped,
        detail,
        progress: None,
    });
}

fn project_asleep(shared: &Shared, project_id: &str) -> bool {
    shared
        .projects
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.id == project_id)
        .map(|p| p.state.is_asleep())
        .unwrap_or(false)
}

fn project_of(shared: &Shared, chat_id: &str) -> Option<Id> {
    shared
        .chats
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.id == chat_id)
        .map(|c| c.project_id.clone())
}

fn send_wake(shared: &Shared, project_id: &str, reason: &'static str) {
    let chat_ids: Vec<Id> = shared
        .chats
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.project_id == project_id)
        .map(|c| c.id.clone())
        .collect();
    let rooms = shared.rooms.lock().unwrap();
    if let Some(room) = chat_ids.iter().find_map(|id| rooms.get(id)) {
        let _ = room.cmds.send(RoomCmd::Wake(reason));
    }
}

fn write_user_message(
    shared: &Shared,
    chat_id: &str,
    message_id: &str,
    text: &str,
    attachments: &[String],
    command: &str,
) {
    shared
        .unacked
        .lock()
        .unwrap()
        .insert(message_id.to_string());
    let docs = shared.docs.lock().unwrap();
    let Some(doc) = docs.get(chat_id) else { return };
    let mut parts: Vec<Part> = attachments
        .iter()
        .map(|name| Part::Attachment {
            id: new_id(),
            attachment: Attachment {
                name: name.clone(),
                mime: "application/octet-stream".into(),
                blob_ref: None,
            },
        })
        .collect();
    parts.push(Part::Text {
        id: new_id(),
        text: text.to_string(),
    });
    let msg = Message {
        id: message_id.to_string(),
        chat_id: chat_id.to_string(),
        role: Role::User,
        sender: "user".into(),
        status: MessageStatus::Done,
        turn: None,
        model: None,
        effort: None,
        created_at: now_ms(),
        completed_at: None,
        pending: false,
        parts,
    };
    let _ = doc.insert_message(&msg);
    let _ = doc.push_command(
        command,
        json!({ "messageId": message_id }),
        &shared.device_id,
    );
    doc.commit();
}

fn handle_command(shared: &Arc<Shared>, cmd: ClientCommand) {
    match cmd {
        ClientCommand::OpenChat { chat_id } => {
            let mut chats = shared.chats.lock().unwrap();
            if let Some(c) = chats.iter_mut().find(|c| c.id == chat_id) {
                c.unread = false;
            }
            drop(chats);
            emit_chats(shared);
        }
        ClientCommand::Send {
            chat_id,
            message_id,
            text,
            attachments,
        } => {
            write_user_message(shared, &chat_id, &message_id, &text, &attachments, "send");
            if let Some(project_id) = project_of(shared, &chat_id) {
                if project_asleep(shared, &project_id) {
                    send_wake(shared, &project_id, "send");
                }
            }
        }
        ClientCommand::Answer {
            chat_id,
            message_id,
            text,
        } => write_user_message(shared, &chat_id, &message_id, &text, &[], "answer"),
        ClientCommand::Stop { chat_id } => {
            let docs = shared.docs.lock().unwrap();
            if let Some(doc) = docs.get(&chat_id) {
                let _ = doc.push_command("stop", json!({}), &shared.device_id);
                doc.commit();
            }
        }
        ClientCommand::Retry {
            chat_id,
            message_id,
        } => {
            let docs = shared.docs.lock().unwrap();
            if let Some(doc) = docs.get(&chat_id) {
                let _ = doc.push_command(
                    "retry",
                    json!({ "messageId": message_id }),
                    &shared.device_id,
                );
                doc.commit();
            }
        }
        ClientCommand::NewChat {
            project_id,
            chat_id,
        } => {
            shared.chats.lock().unwrap().push(Chat {
                id: chat_id.clone(),
                project_id: project_id.clone(),
                title: "New chat".into(),
                is_pinned: false,
                is_system: false,
                unread: false,
                last_message_at: now_ms(),
            });
            emit_chats(shared);
            open_room(shared, &project_id, &chat_id);
            let _ = shared.events.send(SyncEvent::ChatSnapshot {
                chat_id,
                messages: Vec::new(),
            });
        }
        ClientCommand::CloseChat { chat_id } => {
            shared
                .chats
                .lock()
                .unwrap()
                .retain(|c| c.id != chat_id || c.is_pinned);
            shared.rooms.lock().unwrap().remove(&chat_id);
            emit_chats(shared);
        }
        ClientCommand::MarkRead { chat_id } => {
            let mut chats = shared.chats.lock().unwrap();
            if let Some(c) = chats.iter_mut().find(|c| c.id == chat_id) {
                c.unread = false;
            }
            drop(chats);
            emit_chats(shared);
        }
        ClientCommand::SetModel {
            project_id,
            model,
            effort,
        } => {
            // Project-doc `settings` write lands with the project-doc room; local echo for now.
            let mut projects = shared.projects.lock().unwrap();
            if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
                p.model = model;
                p.effort = effort;
            }
            let snapshot = projects.clone();
            drop(projects);
            let _ = shared.events.send(SyncEvent::Projects(snapshot));
        }
        ClientCommand::Wake { project_id } => send_wake(shared, &project_id, "open"),
    }
}
