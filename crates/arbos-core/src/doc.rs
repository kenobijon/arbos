//! The chat doc: one Loro document per chat (crdt-sync §6.2). Text bodies
//! are LoroText and are only appended while streaming; the `commands` list
//! is the client→box outbox. The relay never sees this module: it stores
//! opaque bytes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use loro::{
    Container, ExportMode, LoroDoc, LoroList, LoroMap, LoroText, LoroValue, ValueOrContainer,
    VersionVector,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approval::{ApprovalRequest, ApprovalState};
use crate::ids::{now_ms, ulid};
use crate::widget::QuestionState;

pub const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Queued,
    Streaming,
    Done,
    Error,
    Cancelled,
}

impl MessageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageStatus::Queued => "queued",
            MessageStatus::Streaming => "streaming",
            MessageStatus::Done => "done",
            MessageStatus::Error => "error",
            MessageStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Pending,
    Taken,
    Done,
    Failed,
}

impl CommandStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CommandStatus::Pending => "pending",
            CommandStatus::Taken => "taken",
            CommandStatus::Done => "done",
            CommandStatus::Failed => "failed",
        }
    }
}

/// Plain view of one command row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandView {
    pub id: String,
    pub kind: String,
    pub payload: Value,
    #[serde(rename = "issuedBy")]
    pub issued_by: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: i64,
    pub status: String,
}

/// Plain view of one message (deep JSON of the map).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageView {
    pub id: String,
    pub role: String,
    pub status: String,
    pub parts: Vec<Value>,
    #[serde(flatten)]
    pub rest: Value,
}

impl MessageView {
    /// Concatenated text of all `text` parts.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter(|p| p.get("kind").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
    }
}

/// A chat doc with handles cached for the streaming hot path.
pub struct ChatDoc {
    pub doc: LoroDoc,
    live_text: HashMap<(String, String), LoroText>,
    /// Nesting depth of [`Self::transact`]. Only the outermost call commits.
    txn_depth: AtomicU32,
}

/// Optional fields for a client user-message write (crdt-sync §6.2).
///
/// Omit empty lists and `None` values — the writer does not store null
/// placeholders, `chatId`, `pending`, or `hiddenParts`.
#[derive(Debug, Clone, Default)]
pub struct UserMessageOpts<'a> {
    pub attachments: &'a [UserAttachment<'a>],
    pub mentions: &'a [String],
    pub reply_to: Option<&'a str>,
    pub sender: Option<&'a str>,
    pub sender_handle: Option<&'a str>,
    pub sender_name: Option<&'a str>,
    pub sender_avatar: Option<&'a str>,
    pub answers_question_id: Option<&'a str>,
}

/// One attachment part on a client user message (`name` / `mime` / optional `kind` / `ref`).
#[derive(Debug, Clone)]
pub struct UserAttachment<'a> {
    pub name: &'a str,
    pub mime: &'a str,
    pub kind: Option<&'a str>,
    pub blob_ref: Option<&'a str>,
}

fn lv_to_json(v: &LoroValue) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn json_to_lv(v: &Value) -> LoroValue {
    serde_json::from_value(v.clone()).unwrap_or(LoroValue::Null)
}

fn number_lv(n: &serde_json::Number) -> LoroValue {
    if let Some(i) = n.as_i64() {
        LoroValue::I64(i)
    } else if let Some(u) = n.as_u64() {
        LoroValue::I64(u as i64)
    } else {
        LoroValue::Double(n.as_f64().unwrap_or(0.0))
    }
}

/// Write `value` under `key` as nested Loro containers (objects → maps, arrays →
/// lists), matching Mac `insert_json` / iOS `LoroJSON.insert`. Used for command
/// `payload` so concurrent key edits merge like the existing clients.
fn insert_nested(map: &LoroMap, key: &str, value: &Value) {
    match value {
        Value::Null => {
            map.insert(key, LoroValue::Null).ok();
        }
        Value::Bool(b) => {
            map.insert(key, *b).ok();
        }
        Value::Number(n) => {
            map.insert(key, number_lv(n)).ok();
        }
        Value::String(s) => {
            map.insert(key, s.as_str()).ok();
        }
        Value::Array(items) => {
            let list = map.insert_container(key, LoroList::new()).expect("list");
            for (i, item) in items.iter().enumerate() {
                insert_nested_list(&list, i, item);
            }
        }
        Value::Object(fields) => {
            let child = map.insert_container(key, LoroMap::new()).expect("map");
            for (k, v) in fields {
                insert_nested(&child, k, v);
            }
        }
    }
}

fn insert_nested_list(list: &LoroList, pos: usize, value: &Value) {
    match value {
        Value::Null => {
            list.insert(pos, LoroValue::Null).ok();
        }
        Value::Bool(b) => {
            list.insert(pos, *b).ok();
        }
        Value::Number(n) => {
            list.insert(pos, number_lv(n)).ok();
        }
        Value::String(s) => {
            list.insert(pos, s.as_str()).ok();
        }
        Value::Array(items) => {
            let child = list.insert_container(pos, LoroList::new()).expect("list");
            for (i, item) in items.iter().enumerate() {
                insert_nested_list(&child, i, item);
            }
        }
        Value::Object(fields) => {
            let child = list.insert_container(pos, LoroMap::new()).expect("map");
            for (k, v) in fields {
                insert_nested(&child, k, v);
            }
        }
    }
}

/// Seed an empty `reactions` map on a new message row so later concurrent
/// `set_reaction` calls share one container (crdt-sync §6.2). Creating the map
/// only on first react races: each peer mints its own map and one reaction is lost.
fn seed_reactions(m: &LoroMap) {
    m.insert_container("reactions", LoroMap::new())
        .expect("reactions");
}

impl ChatDoc {
    /// A new empty chat doc (crdt-sync R7).
    ///
    /// `peer`: `Some(id)` sets that Loro peer (box / `arbosctl` mint a fresh id
    /// per boot). `None` keeps Loro's random peer — today's Mac / iPhone
    /// behaviour. Never derive a peer from a device id.
    pub fn new(peer: Option<u64>, record_timestamps: bool) -> Self {
        let doc = LoroDoc::new();
        if let Some(peer) = peer {
            doc.set_peer_id(peer).expect("peer id");
        }
        doc.set_record_timestamp(record_timestamps);
        Self {
            doc,
            live_text: HashMap::new(),
            txn_depth: AtomicU32::new(0),
        }
    }

    /// Run `f`, then commit once at the outermost nesting level. One commit =
    /// one PUSH batch of local edits. Nested `transact` calls share that commit.
    ///
    /// New client writers do not commit themselves; call them inside `transact`.
    /// Box helpers that commit (`append_user_message_from`, `append_command`,
    /// `append_system_note`, …) defer to the outermost `transact` when one is
    /// open — prefer not nesting them inside `transact` unless you intend a
    /// single shared commit.
    pub fn transact<R>(&self, f: impl FnOnce(&Self) -> R) -> R {
        self.txn_depth.fetch_add(1, Ordering::Relaxed);
        let out = f(self);
        if self.txn_depth.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.doc.commit();
        }
        out
    }

    /// Commit unless an outer [`Self::transact`] is open.
    fn commit_if_idle(&self) {
        if self.txn_depth.load(Ordering::Relaxed) == 0 {
            self.doc.commit();
        }
    }

    /// Write `meta` on a brand-new doc (box side).
    pub fn init_meta(&self, chat_id: &str, project_id: &str, epoch: i64) {
        let meta = self.doc.get_map("meta");
        meta.insert("chatId", chat_id).ok();
        meta.insert("projectId", project_id).ok();
        meta.insert("schemaVersion", SCHEMA_VERSION).ok();
        meta.insert("epoch", epoch).ok();
        self.commit_if_idle();
    }

    pub fn from_snapshot(
        peer: Option<u64>,
        record_timestamps: bool,
        snapshot: &[u8],
    ) -> anyhow::Result<Self> {
        let d = Self::new(peer, record_timestamps);
        d.doc.import(snapshot)?;
        Ok(d)
    }

    pub fn import(&mut self, bytes: &[u8]) -> anyhow::Result<loro::ImportStatus> {
        Ok(self.doc.import(bytes)?)
    }

    pub fn oplog_vv(&self) -> VersionVector {
        self.doc.oplog_vv()
    }

    /// Updates since `from`, after committing pending edits.
    pub fn export_updates_since(&self, from: &VersionVector) -> Vec<u8> {
        self.doc.commit();
        self.doc
            .export(ExportMode::updates(from))
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.doc.commit();
        self.doc.export(ExportMode::Snapshot).unwrap_or_default()
    }

    /// True when every op in `vv` is already in this doc (checkpoint containment check, crdt-sync §7.3).
    pub fn contains_vv(&self, vv: &VersionVector) -> bool {
        self.doc.oplog_vv().includes_vv(vv)
    }

    pub fn messages_list(&self) -> LoroList {
        self.doc.get_list("messages")
    }

    pub fn commands_list(&self) -> LoroList {
        self.doc.get_list("commands")
    }

    pub fn to_json(&self) -> Value {
        lv_to_json(&self.doc.get_deep_value())
    }

    fn find_map(list: &LoroList, id: &str) -> Option<LoroMap> {
        for i in 0..list.len() {
            if let Some(ValueOrContainer::Container(Container::Map(m))) = list.get(i) {
                if let Some(ValueOrContainer::Value(LoroValue::String(s))) = m.get("id") {
                    if s.as_str() == id {
                        return Some(m);
                    }
                }
            }
        }
        None
    }

    pub fn find_message(&self, id: &str) -> Option<LoroMap> {
        Self::find_map(&self.messages_list(), id)
    }

    // -------------------------------------------------------------------------
    // Client writers (crdt-sync §6.2). They never commit — use [`Self::transact`].
    // They write no null placeholders, `chatId`, `pending`, `hiddenParts`, or `call_id`.
    // -------------------------------------------------------------------------

    /// Client write: a finished user row (text + optional attachments / mentions /
    /// reply / author / answer link). Does **not** push a `send` command and does
    /// not commit — pair with [`Self::push_command`] inside [`Self::transact`].
    pub fn append_user_message(
        &self,
        msg_id: &str,
        text: &str,
        device_id: &str,
        opts: &UserMessageOpts<'_>,
    ) {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        let now = now_ms();
        m.insert("id", msg_id).ok();
        m.insert("role", "user").ok();
        m.insert("sender", opts.sender.unwrap_or("user")).ok();
        if let Some(h) = opts.sender_handle {
            m.insert("senderHandle", h).ok();
        }
        if let Some(n) = opts.sender_name {
            m.insert("senderName", n).ok();
        }
        if let Some(a) = opts.sender_avatar {
            m.insert("senderAvatar", a).ok();
        }
        if !opts.mentions.is_empty() {
            let ml = m
                .insert_container("mentions", LoroList::new())
                .expect("mentions");
            for mention in opts.mentions {
                ml.push(mention.as_str()).ok();
            }
        }
        if let Some(r) = opts.reply_to {
            m.insert("replyTo", r).ok();
        }
        if let Some(q) = opts.answers_question_id {
            m.insert("answersQuestionId", q).ok();
        }
        m.insert("status", MessageStatus::Done.as_str()).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("createdAt", now).ok();
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        for att in opts.attachments {
            let part = parts.push_container(LoroMap::new()).expect("part");
            part.insert("id", ulid()).ok();
            part.insert("kind", "attachment").ok();
            let a = part
                .insert_container("attachment", LoroMap::new())
                .expect("attachment");
            a.insert("name", att.name).ok();
            a.insert("mime", att.mime).ok();
            if let Some(k) = att.kind {
                a.insert("kind", k).ok();
            }
            if let Some(r) = att.blob_ref {
                a.insert("ref", r).ok();
            }
        }
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        seed_reactions(&m);
    }

    /// Client write: a system line (`role: system`, one text part). Optional
    /// `callId` only — never `call_id`. Does not commit.
    ///
    /// Today's Swift client ignores `callId` when rendering (it shows the text
    /// only); the field is still written for Mac fold/call UI.
    pub fn append_system_line(
        &self,
        msg_id: &str,
        text: &str,
        device_id: &str,
        call_id: Option<&str>,
    ) {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        let now = now_ms();
        m.insert("id", msg_id).ok();
        m.insert("role", "system").ok();
        m.insert("sender", "system").ok();
        m.insert("status", MessageStatus::Done.as_str()).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("createdAt", now).ok();
        m.insert("completedAt", now).ok();
        if let Some(c) = call_id {
            m.insert("callId", c).ok();
        }
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        seed_reactions(&m);
    }

    /// Client write (D82): the agent's canned first line in a brand-new project
    /// chat. `role: assistant`, `sender: agent:<projectId>`, `greeting: true`.
    /// Does not commit.
    pub fn append_greeting(
        &self,
        msg_id: &str,
        text: &str,
        device_id: &str,
        project_id: &str,
        agent_name: &str,
    ) {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        let now = now_ms();
        m.insert("id", msg_id).ok();
        m.insert("role", "assistant").ok();
        let sender = format!("agent:{project_id}");
        m.insert("sender", sender.as_str()).ok();
        m.insert("senderName", agent_name).ok();
        m.insert("greeting", true).ok();
        m.insert("status", MessageStatus::Done.as_str()).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("createdAt", now).ok();
        m.insert("completedAt", now).ok();
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        seed_reactions(&m);
    }

    /// True when any message has `greeting: true` (D82). Callers can skip a
    /// duplicate greeting write.
    pub fn has_greeting(&self) -> bool {
        let msgs = self.messages_list();
        for i in 0..msgs.len() {
            let Some(ValueOrContainer::Container(Container::Map(m))) = msgs.get(i) else {
                continue;
            };
            if matches!(
                m.get("greeting"),
                Some(ValueOrContainer::Value(LoroValue::Bool(true)))
            ) {
                return true;
            }
        }
        false
    }

    /// Client write: set whether `sender` holds `emoji` on a message.
    /// Shape: `reactions: Map { emoji: Map { sender: true } }`. Does not commit.
    ///
    /// When `on` is false and the reactions map or emoji bucket is missing, this
    /// returns without writing (no empty containers for a no-op clear).
    ///
    /// Core message writers seed an empty `reactions` map so two peers reacting
    /// first (different emojis) share one container and both reactions survive.
    /// A remaining race: two peers that each create the **same** emoji bucket
    /// concurrently on a row that lacks that bucket can still lose one sender;
    /// today's app readers still render whatever map wins. Fix that when reaction
    /// reads move into core (e.g. a shared get-or-create for buckets).
    pub fn set_reaction(&self, msg_id: &str, emoji: &str, sender: &str, on: bool) {
        if emoji.is_empty() || sender.is_empty() {
            return;
        }
        let Some(map) = self.find_message(msg_id) else {
            return;
        };
        let reactions = match map.get("reactions") {
            Some(ValueOrContainer::Container(Container::Map(m))) => m,
            _ if !on => return,
            _ => map
                .insert_container("reactions", LoroMap::new())
                .expect("reactions"),
        };
        let bucket = match reactions.get(emoji) {
            Some(ValueOrContainer::Container(Container::Map(m))) => m,
            _ if !on => return,
            _ => reactions
                .insert_container(emoji, LoroMap::new())
                .expect("emoji bucket"),
        };
        if on {
            bucket.insert(sender, true).ok();
        } else {
            bucket.delete(sender).ok();
        }
    }

    /// Client write: append a `commands` row. Does not commit. Returns the command id.
    ///
    /// `payload` is stored as a nested `LoroMap` (same as Mac / iOS), not a
    /// JSON value blob.
    pub fn push_command(&self, kind: &str, payload: Value, device_id: &str) -> String {
        let cmds = self.commands_list();
        let c = cmds.push_container(LoroMap::new()).expect("push command");
        let id = ulid();
        c.insert("id", id.as_str()).ok();
        c.insert("kind", kind).ok();
        insert_nested(&c, "payload", &payload);
        c.insert("issuedBy", device_id).ok();
        c.insert("issuedAt", now_ms()).ok();
        c.insert("status", CommandStatus::Pending.as_str()).ok();
        id
    }

    /// Client write on the project doc: mirror the model picker into `settings`
    /// (crdt-sync §6.3). Does not commit.
    pub fn set_model_settings(&self, model: &str, effort: &str) {
        let settings = self.doc.get_map("settings");
        settings.insert("agentModel", model).ok();
        settings.insert("agentEffort", effort).ok();
        let models = json_to_lv(&serde_json::json!({
            "talking_model": { "id": model, "effort": effort }
        }));
        settings.insert("models", models).ok();
    }

    /// Box / CLI write: a user-role message from another sender (`routine:<id>`,
    /// `channel:<platform>:<id>`, `project:<id>`) with extra scalar fields
    /// (`routineName`, `senderName`, …), plus its `send` command, in one commit.
    /// Returns the command id.
    pub fn append_user_message_from(
        &self,
        msg_id: &str,
        text: &str,
        device_id: &str,
        sender: &str,
        extra: &[(&str, Value)],
    ) -> String {
        self.append_done_message("user", msg_id, text, device_id, sender, extra);
        let cmd_id = self.push_command(
            "send",
            serde_json::json!({ "messageId": msg_id }),
            device_id,
        );
        self.commit_if_idle();
        cmd_id
    }

    /// Box write (F6 groups, S15): a finished message from another Project (`role: "project"`,
    /// `sender: "project:<id>"`, `senderName`, `senderIcon`, `traceId`). When `with_send` is set a
    /// `send` command follows, so the message also drives a turn (a participant addressed by name).
    #[allow(clippy::too_many_arguments)]
    pub fn append_project_message(
        &self,
        msg_id: &str,
        text: &str,
        device_id: &str,
        sender: &str,
        extra: &[(&str, Value)],
        with_send: bool,
    ) -> Option<String> {
        self.append_done_message("project", msg_id, text, device_id, sender, extra);
        let cmd = with_send.then(|| {
            self.push_command(
                "send",
                serde_json::json!({ "messageId": msg_id }),
                device_id,
            )
        });
        self.commit_if_idle();
        cmd
    }

    fn append_done_message(
        &self,
        role: &str,
        msg_id: &str,
        text: &str,
        device_id: &str,
        sender: &str,
        extra: &[(&str, Value)],
    ) {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        let now = now_ms();
        m.insert("id", msg_id).ok();
        m.insert("role", role).ok();
        m.insert("sender", sender).ok();
        m.insert("createdAt", now).ok();
        m.insert("completedAt", now).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("status", MessageStatus::Done.as_str()).ok();
        for (k, v) in extra {
            m.insert(k, json_to_lv(v)).ok();
        }
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        seed_reactions(&m);
    }

    /// Box write (Calls Phase 2): one finished line of a voice call — what a human said
    /// (`role: "user"`) or what the agent said (`role: "assistant"`) — with no `send` command, so
    /// it never drives a turn. `extra` carries `voice: true`, `callId`, speaker attribution.
    pub fn append_voice_line(
        &self,
        role: &str,
        msg_id: &str,
        text: &str,
        device_id: &str,
        sender: &str,
        extra: &[(&str, Value)],
    ) {
        self.append_done_message(role, msg_id, text, device_id, sender, extra);
        self.commit_if_idle();
    }

    /// Box write: a one-line `system` message with a `note` status part (`code: "note"`).
    /// Neither Mac nor iPhone renders this shape in the transcript today — use
    /// [`Self::append_system_line`] for client-visible system lines. Kept for box
    /// paths (routines / vault / voice) that already write it.
    pub fn append_system_note(&self, msg_id: &str, sender: &str, text: &str, device_id: &str) {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        m.insert("id", msg_id).ok();
        m.insert("role", "system").ok();
        m.insert("sender", sender).ok();
        m.insert("createdAt", now_ms()).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("status", MessageStatus::Done.as_str()).ok();
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "status").ok();
        part.insert("code", "note").ok();
        part.insert("message", text).ok();
        seed_reactions(&m);
        self.commit_if_idle();
    }

    /// Client write: the user's answer to a `question` part as a user message (rendered text)
    /// plus an `answer` command carrying the structured value, in one commit.
    pub fn append_user_answer(
        &self,
        msg_id: &str,
        text: &str,
        question_id: &str,
        value: &Value,
        device_id: &str,
    ) -> String {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        m.insert("id", msg_id).ok();
        m.insert("role", "user").ok();
        m.insert("sender", "user").ok();
        m.insert("createdAt", now_ms()).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("status", MessageStatus::Done.as_str()).ok();
        m.insert("answersQuestionId", question_id).ok();
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        seed_reactions(&m);
        let cmd_id = self.push_command(
            "answer",
            serde_json::json!({ "messageId": msg_id, "questionId": question_id, "value": value, "text": text }),
            device_id,
        );
        self.commit_if_idle();
        cmd_id
    }

    /// Box / CLI write: append a `commands` row and commit (unless inside
    /// [`Self::transact`]). Thin committing wrapper over [`Self::push_command`].
    pub fn append_command(&self, kind: &str, payload: Value, device_id: &str) -> String {
        let id = self.push_command(kind, payload, device_id);
        self.commit_if_idle();
        id
    }

    pub fn commands(&self) -> Vec<CommandView> {
        let v = lv_to_json(&self.commands_list().get_deep_value());
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| serde_json::from_value(c.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn pending_commands(&self) -> Vec<CommandView> {
        self.commands()
            .into_iter()
            .filter(|c| c.status == "pending")
            .collect()
    }

    /// Box side: mark a command `taken` before acting, `done`/`failed` after (mark-before-execute).
    pub fn set_command_status(
        &self,
        cmd_id: &str,
        status: CommandStatus,
        resolution: Option<Value>,
    ) {
        if let Some(c) = Self::find_map(&self.commands_list(), cmd_id) {
            c.insert("status", status.as_str()).ok();
            if let Some(r) = resolution {
                c.insert("resolution", json_to_lv(&r)).ok();
            }
            self.commit_if_idle();
        }
    }

    /// Box write: a new assistant message with one empty streaming text part. Returns `(msg_id, part_id)`.
    pub fn start_assistant_message(
        &mut self,
        msg_id: &str,
        in_reply_to: &str,
        device_id: &str,
    ) -> String {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        m.insert("id", msg_id).ok();
        m.insert("role", "assistant").ok();
        m.insert("sender", "agent").ok();
        m.insert("createdAt", now_ms()).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("inReplyTo", in_reply_to).ok();
        m.insert("status", MessageStatus::Streaming.as_str()).ok();
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part_id = self.add_text_part_inner(&parts, msg_id, "text");
        seed_reactions(&m);
        self.commit_if_idle();
        part_id
    }

    fn add_text_part_inner(&mut self, parts: &LoroList, msg_id: &str, kind: &str) -> String {
        let part = parts.push_container(LoroMap::new()).expect("part");
        let part_id = ulid();
        part.insert("id", part_id.as_str()).ok();
        part.insert("kind", kind).ok();
        let field = if kind == "reasoning" {
            "reasoning"
        } else {
            "text"
        };
        let t = part.insert_container(field, LoroText::new()).expect("text");
        self.live_text
            .insert((msg_id.to_owned(), part_id.clone()), t);
        part_id
    }

    /// Add a `text` or `reasoning` part to an existing message.
    pub fn add_text_part(&mut self, msg_id: &str, kind: &str) -> Option<String> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts") {
            Some(ValueOrContainer::Container(Container::List(l))) => l,
            _ => return None,
        };
        let id = self.add_text_part_inner(&parts, msg_id, kind);
        self.commit_if_idle();
        Some(id)
    }

    /// Append a delta to a live text part. Caller batches commits on the 120 ms tick.
    pub fn append_text(&mut self, msg_id: &str, part_id: &str, delta: &str) {
        let key = (msg_id.to_owned(), part_id.to_owned());
        if !self.live_text.contains_key(&key) {
            if let Some(t) = self.lookup_text(msg_id, part_id) {
                self.live_text.insert(key.clone(), t);
            }
        }
        if let Some(t) = self.live_text.get(&key) {
            let len = t.len_unicode();
            t.insert(len, delta).ok();
        }
    }

    fn lookup_text(&self, msg_id: &str, part_id: &str) -> Option<LoroText> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts")? {
            ValueOrContainer::Container(Container::List(l)) => l,
            _ => return None,
        };
        let p = Self::find_map(&parts, part_id)?;
        for field in ["text", "reasoning"] {
            if let Some(ValueOrContainer::Container(Container::Text(t))) = p.get(field) {
                return Some(t);
            }
        }
        None
    }

    /// Add a tool part: ≤160-char summary inline, the rest by `outputRef` (crdt-sync §6.2).
    #[allow(clippy::too_many_arguments)]
    pub fn add_tool_part(
        &mut self,
        msg_id: &str,
        call_id: &str,
        name: &str,
        input: &Value,
        output_summary: &str,
        output_ref: Option<&str>,
        output_bytes: Option<i64>,
        is_error: bool,
    ) -> Option<String> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts")? {
            ValueOrContainer::Container(Container::List(l)) => l,
            _ => return None,
        };
        let part = parts.push_container(LoroMap::new()).ok()?;
        let part_id = ulid();
        part.insert("id", part_id.as_str()).ok();
        part.insert("kind", "tool").ok();
        part.insert(
            "call",
            json_to_lv(&serde_json::json!({"id": call_id, "name": name, "input": input})),
        )
        .ok();
        let summary: String = output_summary
            .chars()
            .take(crate::TOOL_OUTPUT_SUMMARY_MAX)
            .collect();
        part.insert("outputSummary", summary.as_str()).ok();
        if let Some(r) = output_ref {
            part.insert("outputRef", r).ok();
        }
        if let Some(b) = output_bytes {
            part.insert("outputBytes", b).ok();
        }
        part.insert("isError", is_error).ok();
        // Tool details belong to the Details drawer, not the transcript (demo review item 1).
        part.insert("hidden", true).ok();
        self.commit_if_idle();
        Some(part_id)
    }

    /// Add a part the transcript must not render (`hidden: true`): the raw subagent output
    /// (`kind = "subagent_result"`) or any other drawer-only text. Unknown kinds render empty anyway.
    pub fn add_hidden_part(
        &mut self,
        msg_id: &str,
        kind: &str,
        text: &str,
        extra: &Value,
    ) -> Option<String> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts")? {
            ValueOrContainer::Container(Container::List(l)) => l,
            _ => return None,
        };
        let part = parts.push_container(LoroMap::new()).ok()?;
        let part_id = ulid();
        part.insert("id", part_id.as_str()).ok();
        part.insert("kind", kind).ok();
        part.insert("text", text).ok();
        part.insert("hidden", true).ok();
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                part.insert(k, json_to_lv(v)).ok();
            }
        }
        self.commit_if_idle();
        Some(part_id)
    }

    /// Add an `attachment` part (F10): a file the agent produced, blob-backed. `attachment` is
    /// `{ name, mime, ref, bytes, kind, title }`; the bytes live at `GET /blob/{project}/{ref}`.
    pub fn add_attachment_part(&mut self, msg_id: &str, attachment: &Value) -> Option<String> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts")? {
            ValueOrContainer::Container(Container::List(l)) => l,
            _ => return None,
        };
        let part = parts.push_container(LoroMap::new()).ok()?;
        let part_id = ulid();
        part.insert("id", part_id.as_str()).ok();
        part.insert("kind", "attachment").ok();
        part.insert("attachment", json_to_lv(attachment)).ok();
        self.commit_if_idle();
        Some(part_id)
    }

    /// Add a `status` part. Codes: `working`, `done`, `failed`, `stopped`, `waiting_user`,
    /// `waiting_approval` — one working indicator, nothing that narrates tools. Tool detail lives
    /// in `hidden` tool parts (Details drawer) and T3.
    pub fn add_status_part(&mut self, msg_id: &str, kind: &str, text: &str) -> Option<String> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts")? {
            ValueOrContainer::Container(Container::List(l)) => l,
            _ => return None,
        };
        let part = parts.push_container(LoroMap::new()).ok()?;
        let part_id = ulid();
        part.insert("id", part_id.as_str()).ok();
        part.insert("kind", "status").ok();
        part.insert("code", kind).ok();
        part.insert("message", text).ok();
        self.commit_if_idle();
        Some(part_id)
    }

    /// Add an `approval` part (design §10, feature `approve`): the card stays in the transcript
    /// with its final `state`. `payload` is rendered by the card (draft fields, command).
    pub fn add_approval_part(
        &mut self,
        msg_id: &str,
        approval_id: &str,
        req: &ApprovalRequest,
        expires_at: i64,
    ) -> Option<String> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts")? {
            ValueOrContainer::Container(Container::List(l)) => l,
            _ => return None,
        };
        let part = parts.push_container(LoroMap::new()).ok()?;
        let part_id = ulid();
        part.insert("id", part_id.as_str()).ok();
        part.insert("kind", "approval").ok();
        part.insert("approvalId", approval_id).ok();
        part.insert("title", req.title.as_str()).ok();
        part.insert("summary", req.summary.as_str()).ok();
        part.insert("risk", req.risk.as_str()).ok();
        part.insert("payload", json_to_lv(&req.payload)).ok();
        part.insert("state", ApprovalState::Pending.as_str()).ok();
        part.insert("requestedAt", now_ms()).ok();
        part.insert("expiresAt", expires_at).ok();
        self.commit_if_idle();
        Some(part_id)
    }

    fn approval_part(&self, approval_id: &str) -> Option<(String, LoroMap)> {
        let msgs = self.messages_list();
        for i in 0..msgs.len() {
            let Some(ValueOrContainer::Container(Container::Map(m))) = msgs.get(i) else {
                continue;
            };
            let Some(ValueOrContainer::Container(Container::List(parts))) = m.get("parts") else {
                continue;
            };
            for j in 0..parts.len() {
                let Some(ValueOrContainer::Container(Container::Map(p))) = parts.get(j) else {
                    continue;
                };
                let is_match = matches!(
                    p.get("approvalId"),
                    Some(ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == approval_id
                );
                if is_match {
                    let msg_id = match m.get("id") {
                        Some(ValueOrContainer::Value(LoroValue::String(s))) => s.to_string(),
                        _ => String::new(),
                    };
                    return Some((msg_id, p));
                }
            }
        }
        None
    }

    /// Final state of an approval card. Returns the id of the message holding it.
    pub fn set_approval_state(
        &mut self,
        approval_id: &str,
        state: ApprovalState,
        decided_by: Option<&str>,
        reason: Option<&str>,
    ) -> Option<String> {
        let (msg_id, p) = self.approval_part(approval_id)?;
        p.insert("state", state.as_str()).ok();
        p.insert("decidedAt", now_ms()).ok();
        if let Some(d) = decided_by {
            p.insert("decidedBy", d).ok();
        }
        if let Some(r) = reason {
            p.insert("reason", r).ok();
        }
        self.commit_if_idle();
        Some(msg_id)
    }

    /// Plain JSON of one approval part, if present.
    pub fn approval(&self, approval_id: &str) -> Option<Value> {
        self.approval_part(approval_id)
            .map(|(_, p)| lv_to_json(&p.get_deep_value()))
    }

    /// Every approval part across the chat, in document order.
    pub fn approvals(&self) -> Vec<Value> {
        self.messages()
            .into_iter()
            .flat_map(|m| m.parts.into_iter())
            .filter(|p| p.get("kind").and_then(Value::as_str) == Some("approval"))
            .collect()
    }

    /// Client write: `approve {approvalId}` / `reject {approvalId, reason?}`.
    pub fn append_approval_command(
        &self,
        approve: bool,
        approval_id: &str,
        reason: Option<&str>,
        device_id: &str,
    ) -> String {
        let mut payload = serde_json::json!({ "approvalId": approval_id });
        if let Some(r) = reason {
            payload["reason"] = Value::from(r);
        }
        self.append_command(
            if approve { "approve" } else { "reject" },
            payload,
            device_id,
        )
    }

    /// Add a `question` part (design §10 widgets): `{ kind: "question", questionId, question,
    /// widget?, state: "open" }`. `widget` is `None` for a plain text question (S8 composer).
    pub fn add_question_part(
        &mut self,
        msg_id: &str,
        question_id: &str,
        question: &str,
        widget: Option<&Value>,
    ) -> Option<String> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts")? {
            ValueOrContainer::Container(Container::List(l)) => l,
            _ => return None,
        };
        let part = parts.push_container(LoroMap::new()).ok()?;
        let part_id = ulid();
        part.insert("id", part_id.as_str()).ok();
        part.insert("kind", "question").ok();
        part.insert("questionId", question_id).ok();
        part.insert("question", question).ok();
        if let Some(w) = widget {
            part.insert("widget", json_to_lv(w)).ok();
        }
        part.insert("state", QuestionState::Open.as_str()).ok();
        self.commit_if_idle();
        Some(part_id)
    }

    /// Mark the `question` part with `questionId` answered and record the value. Returns false
    /// when no such part exists in this doc.
    pub fn answer_question_part(&mut self, question_id: &str, value: &Value) -> bool {
        let msgs = self.messages_list();
        for i in 0..msgs.len() {
            let Some(ValueOrContainer::Container(Container::Map(m))) = msgs.get(i) else {
                continue;
            };
            let Some(ValueOrContainer::Container(Container::List(parts))) = m.get("parts") else {
                continue;
            };
            for p in 0..parts.len() {
                let Some(ValueOrContainer::Container(Container::Map(part))) = parts.get(p) else {
                    continue;
                };
                let is_match = matches!(part.get("questionId"), Some(ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == question_id);
                if !is_match {
                    continue;
                }
                part.insert("answer", json_to_lv(value)).ok();
                part.insert("state", QuestionState::Answered.as_str()).ok();
                part.insert("answeredAt", now_ms()).ok();
                m.insert("waitingUser", false).ok();
                self.commit_if_idle();
                return true;
            }
        }
        false
    }

    /// The newest `question` part still `open`, as `(questionId, question, widget)`.
    pub fn open_question(&self) -> Option<(String, String, Option<Value>)> {
        let msgs = self.messages();
        for m in msgs.iter().rev() {
            for p in m.parts.iter().rev() {
                if p.get("kind").and_then(Value::as_str) != Some("question") {
                    continue;
                }
                if p.get("state").and_then(Value::as_str) != Some("open") {
                    continue;
                }
                let qid = p.get("questionId").and_then(Value::as_str)?.to_owned();
                let q = p
                    .get("question")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let w = p.get("widget").filter(|w| !w.is_null()).cloned();
                return Some((qid, q, w));
            }
        }
        None
    }

    pub fn add_error_part(&mut self, msg_id: &str, code: &str, text: &str) -> Option<String> {
        let m = self.find_message(msg_id)?;
        let parts = match m.get("parts")? {
            ValueOrContainer::Container(Container::List(l)) => l,
            _ => return None,
        };
        let part = parts.push_container(LoroMap::new()).ok()?;
        let part_id = ulid();
        part.insert("id", part_id.as_str()).ok();
        part.insert("kind", "error").ok();
        part.insert("code", code).ok();
        part.insert("message", text).ok();
        self.commit_if_idle();
        Some(part_id)
    }

    /// Set scalar fields on a message (status, decision, subagentId, model, usage, completedAt, …).
    pub fn set_message_fields(&mut self, msg_id: &str, fields: &[(&str, Value)]) {
        if let Some(m) = self.find_message(msg_id) {
            for (k, v) in fields {
                m.insert(k, json_to_lv(v)).ok();
            }
            self.commit_if_idle();
        }
    }

    pub fn finish_message(&mut self, msg_id: &str, status: MessageStatus) {
        self.set_message_fields(
            msg_id,
            &[
                ("status", Value::from(status.as_str())),
                ("completedAt", Value::from(now_ms())),
            ],
        );
        self.live_text.retain(|(m, _), _| m != msg_id);
    }

    /// What the agent is doing right now, in plain words, on the streaming assistant row
    /// (agent-flow contract §2). `None` clears it: clients fall back to "<agent> is working".
    pub fn set_activity(&mut self, msg_id: &str, activity: Option<&str>) {
        let v = activity
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Value::from)
            .unwrap_or(Value::Null);
        self.set_message_fields(msg_id, &[("activity", v)]);
    }

    pub fn messages(&self) -> Vec<MessageView> {
        let v = lv_to_json(&self.messages_list().get_deep_value());
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| serde_json::from_value(m.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn message(&self, id: &str) -> Option<MessageView> {
        self.messages().into_iter().find(|m| m.id == id)
    }

    pub fn meta(&self) -> Value {
        lv_to_json(&self.doc.get_map("meta").get_deep_value())
    }

    /// Replace every occurrence of `values` in message text (and a few string fields) with
    /// `[REDACTED:vault]`. Used when the owner scrubs a leaked secret from context. Returns
    /// how many replacements were made. Values shorter than 6 characters are ignored.
    pub fn scrub_values(&mut self, values: &[String]) -> usize {
        let needles: Vec<&str> = values
            .iter()
            .map(String::as_str)
            .filter(|v| v.len() >= 6)
            .collect();
        if needles.is_empty() {
            return 0;
        }
        let mut hits = 0;
        let msgs = self.messages_list();
        for i in 0..msgs.len() {
            let Some(ValueOrContainer::Container(Container::Map(m))) = msgs.get(i) else {
                continue;
            };
            let Some(ValueOrContainer::Container(Container::List(parts))) = m.get("parts") else {
                continue;
            };
            for j in 0..parts.len() {
                let Some(ValueOrContainer::Container(Container::Map(p))) = parts.get(j) else {
                    continue;
                };
                for field in [
                    "text",
                    "reasoning",
                    "outputSummary",
                    "message",
                    "summary",
                    "title",
                ] {
                    match p.get(field) {
                        Some(ValueOrContainer::Container(Container::Text(t))) => {
                            hits += scrub_loro_text(&t, &needles);
                        }
                        Some(ValueOrContainer::Value(LoroValue::String(s))) => {
                            let original = s.to_string();
                            let scrubbed = scrub_plain(&original, &needles);
                            if scrubbed != original {
                                hits += 1;
                                let _ = p.insert(field, scrubbed.as_str());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if hits > 0 {
            self.commit_if_idle();
        }
        hits
    }
}

const SCRUB_REDACTED: &str = "[REDACTED:vault]";

fn scrub_plain(text: &str, needles: &[&str]) -> String {
    let mut out = text.to_owned();
    for v in needles {
        if out.contains(v) {
            out = out.replace(v, SCRUB_REDACTED);
        }
    }
    out
}

fn scrub_loro_text(t: &LoroText, needles: &[&str]) -> usize {
    let original = t.to_string();
    let scrubbed = scrub_plain(&original, needles);
    if scrubbed == original {
        return 0;
    }
    let hits = needles
        .iter()
        .map(|v| original.matches(v).count())
        .sum::<usize>();
    let len = t.len_unicode();
    let _ = t.delete(0, len);
    let _ = t.insert(0, &scrubbed);
    hits.max(1)
}

/// Encode a version vector for `STATE`'s checkpoint frontier payload.
pub fn encode_vv(vv: &VersionVector) -> Vec<u8> {
    vv.encode()
}

pub fn decode_vv(bytes: &[u8]) -> Option<VersionVector> {
    VersionVector::decode(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Doc write rules (crdt-sync §6.2): a user message and its `send` command land in one
    /// commit, streaming appends are RLE text appends, and a second peer converges.
    #[test]
    fn user_message_and_send_command_converge_across_peers() {
        let mut box_doc = ChatDoc::new(Some(1), true);
        box_doc.init_meta("chat1", "proj1", 1);
        let mut client = ChatDoc::new(Some(2), false);
        client.import(&box_doc.snapshot()).unwrap();

        let base = client.oplog_vv();
        client.transact(|d| {
            d.append_user_message("m1", "hello", "dev:mac", &UserMessageOpts::default());
            d.push_command("send", serde_json::json!({ "messageId": "m1" }), "dev:mac");
        });
        let upd = client.export_updates_since(&base);
        box_doc.import(&upd).unwrap();
        let pending = box_doc.pending_commands();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, "send");

        // The peer exports everything the other side lacks (its acked version vector), so the
        // `taken` mark and the streamed reply arrive together.
        let before = client.oplog_vv();
        box_doc.set_command_status(&pending[0].id, CommandStatus::Taken, None);
        let part = box_doc.start_assistant_message("m2", "m1", "box:1");
        box_doc.append_text("m2", &part, "Hi ");
        box_doc.append_text("m2", &part, "there");
        box_doc.finish_message("m2", MessageStatus::Done);
        client
            .import(&box_doc.export_updates_since(&before))
            .unwrap();

        let msgs = client.messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].text(), "Hi there");
        assert_eq!(msgs[1].status, "done");
        assert_eq!(client.commands()[0].status, "taken");
    }

    #[test]
    fn scrub_values_redacts_leaked_secret_from_text() {
        let mut doc = ChatDoc::new(Some(1), true);
        doc.init_meta("chat1", "proj1", 1);
        doc.append_user_message_from(
            "m1",
            "my key is sk-live-abcdefghijklmnopqrstuvwxyz012345",
            "dev:mac",
            "user",
            &[],
        );
        let part = doc.start_assistant_message("m2", "m1", "box:1");
        doc.append_text(
            "m2",
            &part,
            "got it: sk-live-abcdefghijklmnopqrstuvwxyz012345",
        );
        doc.finish_message("m2", MessageStatus::Done);

        let n = doc.scrub_values(&["sk-live-abcdefghijklmnopqrstuvwxyz012345".into()]);
        assert!(n >= 2);
        let msgs = doc.messages();
        assert!(msgs[0].text().contains("[REDACTED:vault]"));
        assert!(!msgs[0].text().contains("sk-live-"));
        assert!(msgs[1].text().contains("[REDACTED:vault]"));
        assert!(!msgs[1].text().contains("sk-live-"));
    }

    /// Optional extras for the iOS `insertUserMessage` mirror.
    #[derive(Default)]
    struct IosUserExtras<'a> {
        attachments: &'a [(&'a str, &'a str, Option<&'a str>, Option<&'a str>)],
        mentions: &'a [&'a str],
        reply_to: Option<&'a str>,
        sender: Option<&'a str>,
        sender_handle: Option<&'a str>,
        sender_name: Option<&'a str>,
        sender_avatar: Option<&'a str>,
    }

    /// Rust mirror of today's iOS `insertUserMessage` (placeholders and all).
    fn ios_mirror_insert_user_message(
        doc: &ChatDoc,
        msg_id: &str,
        text: &str,
        device_id: &str,
        extras: &IosUserExtras<'_>,
    ) {
        let msgs = doc.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push");
        m.insert("id", msg_id).ok();
        m.insert("chatId", "chat1").ok();
        m.insert("role", "user").ok();
        m.insert("sender", extras.sender.unwrap_or("user")).ok();
        if let Some(h) = extras.sender_handle {
            m.insert("senderHandle", h).ok();
        }
        if let Some(n) = extras.sender_name {
            m.insert("senderName", n).ok();
        }
        if let Some(a) = extras.sender_avatar {
            m.insert("senderAvatar", a).ok();
        }
        if !extras.mentions.is_empty() {
            let ml = m
                .insert_container("mentions", LoroList::new())
                .expect("mentions");
            for mention in extras.mentions {
                ml.push(*mention).ok();
            }
        }
        if let Some(r) = extras.reply_to {
            m.insert("replyTo", r).ok();
        }
        m.insert("status", "done").ok();
        m.insert("deviceId", device_id).ok();
        m.insert("createdAt", now_ms()).ok();
        m.insert("waitingUser", false).ok();
        m.insert("pending", false).ok();
        for k in [
            "turn",
            "decision",
            "subagentId",
            "subagentName",
            "inReplyTo",
            "usage",
            "returnContract",
            "model",
            "effort",
            "completedAt",
        ] {
            m.insert(k, LoroValue::Null).ok();
        }
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        for (name, mime, kind, blob_ref) in extras.attachments {
            let part = parts.push_container(LoroMap::new()).expect("part");
            part.insert("id", ulid()).ok();
            part.insert("kind", "attachment").ok();
            let a = part
                .insert_container("attachment", LoroMap::new())
                .expect("attachment");
            a.insert("name", *name).ok();
            a.insert("mime", *mime).ok();
            if let Some(k) = kind {
                a.insert("kind", *k).ok();
            }
            // iOS always writes `ref` — null when there is no blob yet.
            match blob_ref {
                Some(r) => {
                    a.insert("ref", *r).ok();
                }
                None => {
                    a.insert("ref", LoroValue::Null).ok();
                }
            }
        }
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        m.insert_container("hiddenParts", LoroList::new())
            .expect("hiddenParts");
    }

    /// Rust mirror of today's iOS `insertSystemLine` (including dual callId/call_id).
    fn ios_mirror_insert_system_line(
        doc: &ChatDoc,
        msg_id: &str,
        text: &str,
        device_id: &str,
        call_id: &str,
    ) {
        let msgs = doc.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push");
        m.insert("id", msg_id).ok();
        m.insert("chatId", "chat1").ok();
        m.insert("role", "system").ok();
        m.insert("sender", "system").ok();
        m.insert("status", "done").ok();
        m.insert("deviceId", device_id).ok();
        m.insert("createdAt", now_ms()).ok();
        m.insert("completedAt", now_ms()).ok();
        m.insert("waitingUser", false).ok();
        m.insert("pending", false).ok();
        m.insert("callId", call_id).ok();
        m.insert("call_id", call_id).ok();
        for k in [
            "turn",
            "decision",
            "subagentId",
            "subagentName",
            "inReplyTo",
            "usage",
            "returnContract",
            "model",
            "effort",
        ] {
            m.insert(k, LoroValue::Null).ok();
        }
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        m.insert_container("hiddenParts", LoroList::new())
            .expect("hiddenParts");
    }

    /// Rust mirror of today's iOS `insertAssistantGreeting`.
    fn ios_mirror_insert_greeting(
        doc: &ChatDoc,
        msg_id: &str,
        text: &str,
        device_id: &str,
        project_id: &str,
        agent_name: &str,
    ) {
        let msgs = doc.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push");
        m.insert("id", msg_id).ok();
        m.insert("chatId", "chat1").ok();
        m.insert("role", "assistant").ok();
        let sender = format!("agent:{project_id}");
        m.insert("sender", sender.as_str()).ok();
        m.insert("senderName", agent_name).ok();
        m.insert("greeting", true).ok();
        m.insert("status", "done").ok();
        m.insert("deviceId", device_id).ok();
        m.insert("createdAt", now_ms()).ok();
        m.insert("completedAt", now_ms()).ok();
        m.insert("waitingUser", false).ok();
        m.insert("pending", false).ok();
        for k in [
            "turn",
            "decision",
            "subagentId",
            "subagentName",
            "inReplyTo",
            "usage",
            "returnContract",
            "model",
            "effort",
        ] {
            m.insert(k, LoroValue::Null).ok();
        }
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        m.insert_container("hiddenParts", LoroList::new())
            .expect("hiddenParts");
    }

    /// Minimal Mac `insert_message` mirror: JSON object fields via nested containers
    /// (same as `arbos-mac` `insert_json`), including null placeholders serde emits.
    fn mac_mirror_insert_message(doc: &ChatDoc, fields: &Value) {
        let msgs = doc.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push");
        let Value::Object(obj) = fields else {
            panic!("mac message must be an object");
        };
        for (key, value) in obj {
            insert_nested(&m, key, value);
        }
    }

    fn keys_without_nulls(row: &Value) -> std::collections::BTreeSet<String> {
        row.as_object()
            .expect("row object")
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, _)| k.clone())
            .collect()
    }

    #[test]
    fn ios_mirror_rows_decode_through_core() {
        let doc = ChatDoc::new(None, false);
        doc.transact(|d| {
            ios_mirror_insert_user_message(
                d,
                "m1",
                "hello from ios",
                "dev:ios",
                &IosUserExtras {
                    attachments: &[("note.txt", "text/plain", None, None)],
                    mentions: &["agent:p1"],
                    reply_to: Some("prev"),
                    sender: Some("user:sam"),
                    sender_handle: Some("sam"),
                    sender_name: Some("Sam"),
                    sender_avatar: Some("https://example/a.png"),
                },
            );
            ios_mirror_insert_system_line(d, "m2", "Call ended", "dev:ios", "call-1");
            ios_mirror_insert_greeting(d, "m3", "Hi, I'm Arbos.", "dev:ios", "proj1", "Arbos");
        });
        let msgs = doc.messages();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].text(), "hello from ios");
        assert_eq!(msgs[1].role, "system");
        assert_eq!(msgs[1].text(), "Call ended");
        assert_eq!(msgs[2].role, "assistant");
        assert_eq!(msgs[2].text(), "Hi, I'm Arbos.");
        let raw = doc.to_json();
        let user = &raw["messages"][0];
        assert_eq!(user["replyTo"], "prev");
        assert_eq!(user["senderHandle"], "sam");
        assert_eq!(user["mentions"][0], "agent:p1");
        let att = &user["parts"][0]["attachment"];
        assert_eq!(att["name"], "note.txt");
        assert!(att.get("ref").is_some());
        assert!(att["ref"].is_null());
        let row = &raw["messages"][1];
        assert_eq!(row["callId"], "call-1");
        assert_eq!(row["call_id"], "call-1");
        assert!(row.get("hiddenParts").is_some());
        let greet = &raw["messages"][2];
        assert_eq!(greet["greeting"], true);
        assert_eq!(greet["sender"], "agent:proj1");
    }

    #[test]
    fn client_writers_omit_placeholders_and_converge() {
        let a = ChatDoc::new(None, false);
        let mut b = ChatDoc::new(None, false);

        let base_a = a.oplog_vv();
        a.transact(|d| {
            d.append_user_message(
                "m1",
                "hi",
                "dev:a",
                &UserMessageOpts {
                    reply_to: Some("prev"),
                    sender: Some("user:ken"),
                    sender_handle: Some("ken"),
                    sender_name: Some("Ken"),
                    mentions: &["agent:p1".into()],
                    attachments: &[UserAttachment {
                        name: "pic.png",
                        mime: "image/png",
                        kind: Some("image"),
                        blob_ref: Some("blob-1"),
                    }],
                    ..UserMessageOpts::default()
                },
            );
            d.push_command("send", serde_json::json!({ "messageId": "m1" }), "dev:a");
            d.append_system_line("m2", "Sam started a video call", "dev:a", Some("c1"));
            d.append_greeting("m3", "Hi, I'm Arbos.", "dev:a", "proj1", "Arbos");
            d.set_reaction("m1", "👍", "user:ken", true);
            d.set_model_settings("gpt-test", "high");
        });

        let row = &a.to_json()["messages"][0];
        assert!(row.get("chatId").is_none());
        assert!(row.get("pending").is_none());
        assert!(row.get("hiddenParts").is_none());
        assert!(row.get("waitingUser").is_none());
        assert_eq!(row["replyTo"], "prev");
        assert_eq!(row["senderHandle"], "ken");
        assert!(row.get("reactions").is_some());
        let sys = &a.to_json()["messages"][1];
        assert_eq!(sys["callId"], "c1");
        assert!(sys.get("call_id").is_none());
        let greet = &a.to_json()["messages"][2];
        assert_eq!(greet["greeting"], true);
        assert_eq!(greet["sender"], "agent:proj1");
        assert!(a.has_greeting());
        assert_eq!(a.to_json()["settings"]["agentModel"], "gpt-test");
        assert_eq!(a.to_json()["settings"]["agentEffort"], "high");
        // Command payload is a nested map (deep value still looks like JSON).
        assert_eq!(a.to_json()["commands"][0]["payload"]["messageId"], "m1");

        b.import(&a.export_updates_since(&base_a)).unwrap();
        assert_eq!(b.messages().len(), 3);
        assert_eq!(b.messages()[0].text(), "hi");
        assert_eq!(b.messages()[1].text(), "Sam started a video call");
        assert_eq!(b.messages()[2].text(), "Hi, I'm Arbos.");
        assert_eq!(b.pending_commands().len(), 1);
        let reactions = &b.to_json()["messages"][0]["reactions"]["👍"];
        assert_eq!(reactions["user:ken"], true);
        assert_eq!(
            b.to_json()["settings"]["models"]["talking_model"]["id"],
            "gpt-test"
        );
    }

    #[test]
    fn set_reaction_off_is_noop_without_bucket() {
        let doc = ChatDoc::new(Some(1), false);
        doc.transact(|d| {
            d.append_user_message("m1", "hi", "dev", &UserMessageOpts::default());
            d.set_reaction("m1", "👍", "user:a", false);
        });
        let reactions = &doc.to_json()["messages"][0]["reactions"];
        assert!(reactions.as_object().unwrap().is_empty());
    }

    #[test]
    fn nested_transact_commits_once() {
        let doc = ChatDoc::new(Some(1), false);
        let before = doc.oplog_vv();
        doc.transact(|outer| {
            outer.append_user_message("m1", "a", "dev", &UserMessageOpts::default());
            outer.transact(|inner| {
                inner.append_user_message("m2", "b", "dev", &UserMessageOpts::default());
                // Committing helpers defer to the outermost transact.
                let _ = inner.append_command("stop", serde_json::json!({}), "dev");
            });
        });
        assert_eq!(doc.messages().len(), 2);
        assert_eq!(doc.commands().len(), 1);
        // One export covers the whole nested transaction.
        let upd = doc.export_updates_since(&before);
        let mut other = ChatDoc::new(Some(2), false);
        other.import(&upd).unwrap();
        assert_eq!(other.messages().len(), 2);
        assert_eq!(other.commands().len(), 1);
    }

    #[test]
    fn concurrent_reactions_from_two_peers_both_survive() {
        // Shared message with seeded empty reactions, then both peers react before sync.
        let seed = ChatDoc::new(Some(1), false);
        seed.transact(|d| {
            d.append_user_message("m1", "hi", "dev:seed", &UserMessageOpts::default());
        });
        let snap = seed.snapshot();

        let mut a = ChatDoc::from_snapshot(Some(2), false, &snap).unwrap();
        let mut b = ChatDoc::from_snapshot(Some(3), false, &snap).unwrap();
        let base_a = a.oplog_vv();
        let base_b = b.oplog_vv();

        a.transact(|d| d.set_reaction("m1", "👍", "user:a", true));
        b.transact(|d| d.set_reaction("m1", "❤️", "user:b", true));

        let upd_a = a.export_updates_since(&base_a);
        let upd_b = b.export_updates_since(&base_b);
        a.import(&upd_b).unwrap();
        b.import(&upd_a).unwrap();

        for peer in [&a, &b] {
            let r = &peer.to_json()["messages"][0]["reactions"];
            assert_eq!(r["👍"]["user:a"], true);
            assert_eq!(r["❤️"]["user:b"], true);
        }
    }

    #[test]
    fn concurrent_messages_from_two_peers_both_survive() {
        let snap = ChatDoc::new(Some(1), false).snapshot();
        let mut a = ChatDoc::from_snapshot(Some(2), false, &snap).unwrap();
        let mut b = ChatDoc::from_snapshot(Some(3), false, &snap).unwrap();
        let base_a = a.oplog_vv();
        let base_b = b.oplog_vv();

        a.transact(|d| {
            d.append_user_message("m-a", "from a", "dev:a", &UserMessageOpts::default());
        });
        b.transact(|d| {
            d.append_user_message("m-b", "from b", "dev:b", &UserMessageOpts::default());
        });

        let upd_a = a.export_updates_since(&base_a);
        let upd_b = b.export_updates_since(&base_b);
        a.import(&upd_b).unwrap();
        b.import(&upd_a).unwrap();

        for peer in [&a, &b] {
            let ids: std::collections::BTreeSet<_> =
                peer.messages().into_iter().map(|m| m.id).collect();
            assert_eq!(ids, ["m-a".into(), "m-b".into()].into_iter().collect());
        }
    }

    #[test]
    fn core_user_row_keys_match_mac_insert_after_dropping_nulls() {
        // Mac `insert_message` serializes a Message and writes every field (nulls
        // included). After dropping nulls, compare the remaining key set to core's
        // lean writer — ignoring Mac-only placeholders and core's seeded reactions.
        let mac_doc = ChatDoc::new(Some(10), false);
        let created = now_ms();
        mac_mirror_insert_message(
            &mac_doc,
            &serde_json::json!({
                "id": "m1",
                "chatId": "chat1",
                "role": "user",
                "sender": "user",
                "status": "done",
                "turn": null,
                "decision": null,
                "subagentId": null,
                "subagentName": null,
                "handoffMessages": null,
                "inReplyTo": null,
                "routineName": null,
                "senderName": null,
                "senderIcon": null,
                "senderHandle": null,
                "senderAvatar": null,
                "senderAvatarColor": null,
                "mentions": [],
                "replyTo": null,
                "askedByHandle": null,
                "ownerHandle": null,
                "usage": null,
                "returnContract": null,
                "waitingUser": false,
                "waitingApproval": false,
                "model": null,
                "effort": null,
                "createdAt": created,
                "completedAt": null,
                "pending": false,
                "parts": [{ "id": "p1", "kind": "text", "text": "hello" }],
                "hiddenParts": []
            }),
        );
        mac_doc.doc.commit();

        let core_doc = ChatDoc::new(Some(11), false);
        core_doc.transact(|d| {
            d.append_user_message("m1", "hello", "dev:mac", &UserMessageOpts::default());
        });

        let mac_keys = keys_without_nulls(&mac_doc.to_json()["messages"][0]);
        let core_keys = keys_without_nulls(&core_doc.to_json()["messages"][0]);

        let mac_only: std::collections::BTreeSet<&str> = [
            "chatId",
            "waitingUser",
            "waitingApproval",
            "pending",
            "hiddenParts",
            "mentions",
        ]
        .into_iter()
        .collect();
        let core_only: std::collections::BTreeSet<&str> =
            ["deviceId", "reactions"].into_iter().collect();

        let mac_shared: std::collections::BTreeSet<_> = mac_keys
            .iter()
            .filter(|k| !mac_only.contains(k.as_str()))
            .cloned()
            .collect();
        let core_shared: std::collections::BTreeSet<_> = core_keys
            .iter()
            .filter(|k| !core_only.contains(k.as_str()))
            .cloned()
            .collect();
        assert_eq!(mac_shared, core_shared);
    }

    #[test]
    fn none_peer_uses_loro_random_and_some_sets_explicit() {
        let a = ChatDoc::new(None, false);
        let b = ChatDoc::new(None, false);
        let explicit = ChatDoc::new(Some(42), false);
        assert_eq!(explicit.doc.peer_id(), 42);
        assert_ne!(
            a.doc.peer_id(),
            b.doc.peer_id(),
            "two ChatDoc::new(None, ..) must get different Loro peers"
        );
    }

    #[test]
    fn has_greeting_detects_existing_row() {
        let doc = ChatDoc::new(Some(1), false);
        assert!(!doc.has_greeting());
        doc.transact(|d| {
            d.append_greeting("g1", "Hi", "dev", "proj", "Arbos");
        });
        assert!(doc.has_greeting());
    }

    #[test]
    fn append_command_wraps_push_command() {
        let doc = ChatDoc::new(Some(1), false);
        let id = doc.append_command("stop", serde_json::json!({}), "dev");
        assert!(!id.is_empty());
        assert_eq!(doc.commands().len(), 1);
        assert_eq!(doc.commands()[0].kind, "stop");
        // Nested empty payload map.
        assert!(doc.to_json()["commands"][0]["payload"].is_object());
    }
}
