//! The chat doc: one Loro document per chat (crdt-sync §6.2). Text bodies
//! are LoroText and are only appended while streaming; the `commands` list
//! is the client→box outbox. The relay never sees this module: it stores
//! opaque bytes.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};

use loro::event::DiffEvent;
use loro::{
    Container, ExportMode, Index, LoroDoc, LoroList, LoroMap, LoroText, LoroValue,
    ValueOrContainer, VersionVector,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approval::{ApprovalRequest, ApprovalState};
use crate::ids::{now_ms, ulid};
use crate::widget::QuestionState;

pub const SCHEMA_VERSION: i64 = 1;

/// Error from a client chat-doc writer (UniFFI-friendly owned message).
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{0}")]
    Message(String),
}

impl CoreError {
    pub fn msg(s: impl Into<String>) -> Self {
        Self::Message(s.into())
    }
}

pub type CoreResult<T> = Result<T, CoreError>;

fn loro_err<T, E: std::fmt::Display>(r: Result<T, E>) -> CoreResult<T> {
    r.map_err(|e| CoreError::msg(e.to_string()))
}

/// What a root-level diff touched, mapped to message indexes.
#[derive(Debug, Default)]
pub struct Touched {
    /// The `messages` list itself changed (insert/delete): re-read everything.
    pub list_changed: bool,
    pub messages: BTreeSet<usize>,
    /// Anything outside `messages` (project doc maps, `commands`, `meta`, …).
    /// Mirrors Swift `ChatDoc.Touched.other` so UniFFI clients lose nothing.
    pub other: bool,
}

/// Map a root diff to message indexes. Paths look like
/// `[(messages, Key("messages")), (msg, Seq(i)), (parts, Key("parts")), ...]`.
pub fn touched(event: &DiffEvent<'_>) -> Touched {
    let mut out = Touched::default();
    for diff in &event.events {
        let mut saw_messages = false;
        let mut found = None;
        for (_, index) in diff.path.iter() {
            match index {
                Index::Key(k) if !saw_messages => {
                    if k.as_str() == "messages" {
                        saw_messages = true;
                    } else {
                        break;
                    }
                }
                Index::Seq(i) if saw_messages => {
                    found = Some(*i);
                    break;
                }
                Index::Key(_) | Index::Seq(_) | Index::Node(_) => {}
            }
        }
        if !saw_messages {
            // A diff whose target is the root `messages` list itself (empty path
            // to that container) is a list change; anything else is `other`.
            match diff.target {
                loro::ContainerID::Root { name, .. } if name.as_str() == "messages" => {
                    out.list_changed = true;
                }
                _ => out.other = true,
            }
            continue;
        }
        match found {
            Some(i) => {
                out.messages.insert(i);
            }
            None => out.list_changed = true,
        }
    }
    out
}

/// Reader projection: move `parts[*]` with `hidden: true` into `hiddenParts`
/// so the transcript never sees them while a details drawer still can.
/// Writers never create `hiddenParts`.
pub fn project_hidden_parts(json: &mut Value) {
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    let Some(Value::Array(parts)) = obj.get_mut("parts") else {
        return;
    };
    let mut hidden = Vec::new();
    parts.retain(|p| {
        let is_hidden = p.get("hidden").and_then(Value::as_bool).unwrap_or(false);
        if is_hidden {
            hidden.push(p.clone());
        }
        !is_hidden
    });
    if !hidden.is_empty() {
        obj.insert("hiddenParts".into(), Value::Array(hidden));
    }
}

/// Separator in flat reaction keys: `"emoji|sender"` under `reactions`.
pub const REACTION_KEY_SEP: char = '|';

/// Build the CRDT key for one sender's hold of an emoji.
pub fn reaction_key(emoji: &str, sender: &str) -> String {
    format!("{emoji}{REACTION_KEY_SEP}{sender}")
}

/// Reader projection: expand flat `reactions` keys (`"emoji|sender" → true`)
/// into `emoji → { sender → true }` for UI / serde. Also folds legacy nested
/// emoji-bucket maps so both shapes read the same. Call before deserializing
/// a message row.
pub fn project_reactions(json: &mut Value) {
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    let Some(Value::Object(raw)) = obj.get("reactions").cloned() else {
        return;
    };
    let mut nested = serde_json::Map::new();
    for (key, value) in raw {
        if let Value::Object(bucket) = value {
            // Legacy: emoji → { sender → true }.
            if key.contains(REACTION_KEY_SEP) {
                continue;
            }
            let entry = nested
                .entry(key)
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Value::Object(dst) = entry {
                for (sender, flag) in bucket {
                    if flag.as_bool() == Some(true) {
                        dst.insert(sender, Value::Bool(true));
                    }
                }
            }
            continue;
        }
        if value.as_bool() != Some(true) {
            continue;
        }
        let Some((emoji, sender)) = key.split_once(REACTION_KEY_SEP) else {
            continue;
        };
        if emoji.is_empty() || sender.is_empty() {
            continue;
        }
        let entry = nested
            .entry(emoji.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Value::Object(dst) = entry {
            dst.insert(sender.to_string(), Value::Bool(true));
        }
    }
    obj.insert("reactions".into(), Value::Object(nested));
}

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
    /// Nesting depth for [`Self::transact`]. Only the outermost call commits.
    txn_depth: Cell<u32>,
}

/// Optional fields for a client user-message write (crdt-sync §6.2).
///
/// Omit empty lists and `None` values — the writer does not store null
/// placeholders, `chatId`, `pending`, or `hiddenParts`.
#[derive(Debug, Clone, Default)]
pub struct UserMessageOpts {
    pub attachments: Vec<UserAttachment>,
    pub mentions: Vec<String>,
    pub reply_to: Option<String>,
    pub sender: Option<String>,
    pub sender_handle: Option<String>,
    pub sender_name: Option<String>,
    pub sender_avatar: Option<String>,
    pub answers_question_id: Option<String>,
}

/// One attachment part on a client user message (`name` / `mime` / optional `kind` / `ref`).
#[derive(Debug, Clone)]
pub struct UserAttachment {
    pub name: String,
    pub mime: String,
    pub kind: Option<String>,
    pub blob_ref: Option<String>,
}

fn lv_to_json(v: &LoroValue) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn json_to_lv(v: &Value) -> LoroValue {
    serde_json::from_value(v.clone()).unwrap_or(LoroValue::Null)
}

fn number_value(n: &serde_json::Number) -> LoroValue {
    if let Some(i) = n.as_i64() {
        LoroValue::I64(i)
    } else if let Some(u) = n.as_u64() {
        LoroValue::I64(u as i64)
    } else {
        LoroValue::Double(n.as_f64().unwrap_or(0.0))
    }
}

/// Write a JSON value under `key`. Objects become `LoroMap`, arrays `LoroList`.
/// (Command payloads use this; no LoroText special case for part text fields.)
fn insert_json_map(map: &LoroMap, key: &str, value: Value) -> CoreResult<()> {
    match value {
        Value::Null => {
            loro_err(map.insert(key, LoroValue::Null))?;
        }
        Value::Bool(b) => {
            loro_err(map.insert(key, b))?;
        }
        Value::Number(n) => {
            loro_err(map.insert(key, number_value(&n)))?;
        }
        Value::String(s) => {
            loro_err(map.insert(key, s))?;
        }
        Value::Array(items) => {
            let list = loro_err(map.insert_container(key, LoroList::new()))?;
            for (i, item) in items.into_iter().enumerate() {
                insert_json_list(&list, i, item)?;
            }
        }
        Value::Object(fields) => {
            let child = loro_err(map.insert_container(key, LoroMap::new()))?;
            for (k, v) in fields {
                insert_json_map(&child, &k, v)?;
            }
        }
    }
    Ok(())
}

fn insert_json_list(list: &LoroList, pos: usize, value: Value) -> CoreResult<()> {
    match value {
        Value::Null => {
            loro_err(list.insert(pos, LoroValue::Null))?;
        }
        Value::Bool(b) => {
            loro_err(list.insert(pos, b))?;
        }
        Value::Number(n) => {
            loro_err(list.insert(pos, number_value(&n)))?;
        }
        Value::String(s) => {
            loro_err(list.insert(pos, s))?;
        }
        Value::Array(items) => {
            let child = loro_err(list.insert_container(pos, LoroList::new()))?;
            for (i, item) in items.into_iter().enumerate() {
                insert_json_list(&child, i, item)?;
            }
        }
        Value::Object(fields) => {
            let child = loro_err(list.insert_container(pos, LoroMap::new()))?;
            for (k, v) in fields {
                insert_json_map(&child, &k, v)?;
            }
        }
    }
    Ok(())
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
            txn_depth: Cell::new(0),
        }
    }

    /// Run `f`, then commit once when this is the outermost `transact`.
    /// Nested calls only decrement depth; they do not commit.
    /// New client writers do not commit themselves; call them inside `transact`.
    /// Depth is restored by a drop guard so a panic inside `f` cannot leave it stuck.
    pub fn transact<R>(&self, f: impl FnOnce(&Self) -> R) -> R {
        struct DepthGuard<'a>(&'a Cell<u32>);
        impl Drop for DepthGuard<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get().saturating_sub(1));
            }
        }
        self.txn_depth.set(self.txn_depth.get() + 1);
        let guard = DepthGuard(&self.txn_depth);
        let out = f(self);
        drop(guard);
        self.commit_now();
        out
    }

    /// Commit pending ops only when no `transact` is open.
    fn commit_now(&self) {
        if self.txn_depth.get() == 0 {
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
        self.commit_now();
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
    ///
    /// While a [`Self::transact`] is open this returns empty and does not call
    /// Loro `export` (export is a commit barrier and would flush the batch).
    pub fn export_updates_since(&self, from: &VersionVector) -> Vec<u8> {
        if self.txn_depth.get() > 0 {
            return Vec::new();
        }
        self.doc.commit();
        self.doc
            .export(ExportMode::updates(from))
            .unwrap_or_default()
    }

    /// Full snapshot after committing pending edits.
    ///
    /// While a [`Self::transact`] is open this returns empty and does not call
    /// Loro `export` (export is a commit barrier and would flush the batch).
    pub fn snapshot(&self) -> Vec<u8> {
        if self.txn_depth.get() > 0 {
            return Vec::new();
        }
        self.doc.commit();
        self.doc.export(ExportMode::Snapshot).unwrap_or_default()
    }

    /// True when every op in `vv` is already in this doc (checkpoint containment check, crdt-sync §7.3).
    pub fn contains_vv(&self, vv: &VersionVector) -> bool {
        self.doc.oplog_vv().includes_vv(vv)
    }

    /// §7.3 containment on raw STATE frontier bytes (`decode_vv` then [`Self::contains_vv`]).
    /// False when the frontier does not decode.
    pub fn contains_frontier(&self, frontier: &[u8]) -> bool {
        match decode_vv(frontier) {
            Some(vv) => self.contains_vv(&vv),
            None => false,
        }
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
        opts: &UserMessageOpts,
    ) -> CoreResult<()> {
        let msgs = self.messages_list();
        let m = loro_err(msgs.push_container(LoroMap::new()))?;
        let now = now_ms();
        loro_err(m.insert("id", msg_id))?;
        loro_err(m.insert("role", "user"))?;
        loro_err(m.insert("sender", opts.sender.as_deref().unwrap_or("user")))?;
        if let Some(h) = opts.sender_handle.as_deref() {
            loro_err(m.insert("senderHandle", h))?;
        }
        if let Some(n) = opts.sender_name.as_deref() {
            loro_err(m.insert("senderName", n))?;
        }
        if let Some(a) = opts.sender_avatar.as_deref() {
            loro_err(m.insert("senderAvatar", a))?;
        }
        if !opts.mentions.is_empty() {
            let ml = loro_err(m.insert_container("mentions", LoroList::new()))?;
            for mention in &opts.mentions {
                loro_err(ml.push(mention.as_str()))?;
            }
        }
        if let Some(r) = opts.reply_to.as_deref() {
            loro_err(m.insert("replyTo", r))?;
        }
        if let Some(q) = opts.answers_question_id.as_deref() {
            loro_err(m.insert("answersQuestionId", q))?;
        }
        loro_err(m.insert("status", MessageStatus::Done.as_str()))?;
        loro_err(m.insert("deviceId", device_id))?;
        loro_err(m.insert("createdAt", now))?;
        loro_err(m.insert_container("reactions", LoroMap::new()))?;
        let parts = loro_err(m.insert_container("parts", LoroList::new()))?;
        for att in &opts.attachments {
            let part = loro_err(parts.push_container(LoroMap::new()))?;
            loro_err(part.insert("id", ulid()))?;
            loro_err(part.insert("kind", "attachment"))?;
            let a = loro_err(part.insert_container("attachment", LoroMap::new()))?;
            loro_err(a.insert("name", att.name.as_str()))?;
            loro_err(a.insert("mime", att.mime.as_str()))?;
            if let Some(k) = att.kind.as_deref() {
                loro_err(a.insert("kind", k))?;
            }
            if let Some(r) = att.blob_ref.as_deref() {
                loro_err(a.insert("ref", r))?;
            }
        }
        let part = loro_err(parts.push_container(LoroMap::new()))?;
        loro_err(part.insert("id", ulid()))?;
        loro_err(part.insert("kind", "text"))?;
        let t = loro_err(part.insert_container("text", LoroText::new()))?;
        loro_err(t.insert(0, text))?;
        Ok(())
    }

    /// Client write: a system line (`role: system`, one text part). Optional
    /// `callId` only — never `call_id`. Does not commit.
    pub fn append_system_line(
        &self,
        msg_id: &str,
        text: &str,
        device_id: &str,
        call_id: Option<&str>,
    ) -> CoreResult<()> {
        let msgs = self.messages_list();
        let m = loro_err(msgs.push_container(LoroMap::new()))?;
        let now = now_ms();
        loro_err(m.insert("id", msg_id))?;
        loro_err(m.insert("role", "system"))?;
        loro_err(m.insert("sender", "system"))?;
        loro_err(m.insert("status", MessageStatus::Done.as_str()))?;
        loro_err(m.insert("deviceId", device_id))?;
        loro_err(m.insert("createdAt", now))?;
        loro_err(m.insert("completedAt", now))?;
        if let Some(c) = call_id {
            loro_err(m.insert("callId", c))?;
        }
        loro_err(m.insert_container("reactions", LoroMap::new()))?;
        let parts = loro_err(m.insert_container("parts", LoroList::new()))?;
        let part = loro_err(parts.push_container(LoroMap::new()))?;
        loro_err(part.insert("id", ulid()))?;
        loro_err(part.insert("kind", "text"))?;
        let t = loro_err(part.insert_container("text", LoroText::new()))?;
        loro_err(t.insert(0, text))?;
        Ok(())
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
    ) -> CoreResult<()> {
        let msgs = self.messages_list();
        let m = loro_err(msgs.push_container(LoroMap::new()))?;
        let now = now_ms();
        loro_err(m.insert("id", msg_id))?;
        loro_err(m.insert("role", "assistant"))?;
        let sender = format!("agent:{project_id}");
        loro_err(m.insert("sender", sender.as_str()))?;
        loro_err(m.insert("senderName", agent_name))?;
        loro_err(m.insert("greeting", true))?;
        loro_err(m.insert("status", MessageStatus::Done.as_str()))?;
        loro_err(m.insert("deviceId", device_id))?;
        loro_err(m.insert("createdAt", now))?;
        loro_err(m.insert("completedAt", now))?;
        loro_err(m.insert_container("reactions", LoroMap::new()))?;
        let parts = loro_err(m.insert_container("parts", LoroList::new()))?;
        let part = loro_err(parts.push_container(LoroMap::new()))?;
        loro_err(part.insert("id", ulid()))?;
        loro_err(part.insert("kind", "text"))?;
        let t = loro_err(part.insert_container("text", LoroText::new()))?;
        loro_err(t.insert(0, text))?;
        Ok(())
    }

    /// True when any message has `greeting: true` (D82 first-line marker).
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
    ///
    /// CRDT shape: flat keys under `reactions` — `"emoji|sender" → true` — so
    /// concurrent first reacts on the same emoji merge as map-key inserts
    /// instead of racing on nested emoji-bucket creation. Readers call
    /// [`project_reactions`] to view `emoji → { sender → true }`. Does not
    /// commit. Turning a reaction off is a no-op when the reactions map is
    /// missing (does not create containers).
    pub fn set_reaction(
        &self,
        msg_id: &str,
        emoji: &str,
        sender: &str,
        on: bool,
    ) -> CoreResult<()> {
        if emoji.is_empty() || sender.is_empty() {
            return Ok(());
        }
        let Some(map) = self.find_message(msg_id) else {
            return Ok(());
        };
        let reactions = match map.get("reactions") {
            Some(ValueOrContainer::Container(Container::Map(m))) => m,
            _ if on => loro_err(map.insert_container("reactions", LoroMap::new()))?,
            _ => return Ok(()),
        };
        let key = reaction_key(emoji, sender);
        if on {
            loro_err(reactions.insert(key.as_str(), true))?;
        } else {
            // Flat key (current shape). Also clear a legacy nested bucket entry
            // if one is still present from older writers.
            let _ = reactions.delete(key.as_str());
            if let Some(ValueOrContainer::Container(Container::Map(bucket))) = reactions.get(emoji)
            {
                let _ = bucket.delete(sender);
            }
        }
        Ok(())
    }

    /// Client write: append a `commands` row. Does not commit. Returns the command id.
    /// Object payloads become a nested `LoroMap` (arrays → `LoroList`).
    pub fn push_command(&self, kind: &str, payload: Value, device_id: &str) -> String {
        let cmds = self.commands_list();
        let c = cmds.push_container(LoroMap::new()).expect("push command");
        let id = ulid();
        c.insert("id", id.as_str()).ok();
        c.insert("kind", kind).ok();
        insert_json_map(&c, "payload", payload).expect("payload");
        c.insert("issuedBy", device_id).ok();
        c.insert("issuedAt", now_ms()).ok();
        c.insert("status", CommandStatus::Pending.as_str()).ok();
        id
    }

    /// Client write on the project doc: mirror the model picker into `settings`
    /// (crdt-sync §6.3). Does not commit.
    pub fn set_model_settings(&self, model: &str, effort: &str) -> CoreResult<()> {
        let settings = self.doc.get_map("settings");
        loro_err(settings.insert("agentModel", model))?;
        loro_err(settings.insert("agentEffort", effort))?;
        insert_json_map(
            &settings,
            "models",
            serde_json::json!({
                "talking_model": { "id": model, "effort": effort }
            }),
        )?;
        Ok(())
    }

    /// Box / CLI write: a user-role message from another sender (`routine:<id>`,
    /// `channel:<platform>:<id>`, `project:<id>`) with extra scalar fields
    /// (`routineName`, `senderName`, …), plus its `send` command.
    /// Honors an open [`Self::transact`] (only the outermost commits).
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
        self.commit_now();
        cmd_id
    }

    /// Box write (F6 groups, S15): a finished message from another Project (`role: "project"`,
    /// `sender: "project:<id>"`, `senderName`, `senderIcon`, `traceId`). When `with_send` is set a
    /// `send` command follows, so the message also drives a turn (a participant addressed by name).
    /// Honors an open [`Self::transact`] (only the outermost commits).
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
        self.commit_now();
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
        m.insert_container("reactions", LoroMap::new())
            .expect("reactions");
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
    }

    /// Box write (Calls Phase 2): one finished line of a voice call — what a human said
    /// (`role: "user"`) or what the agent said (`role: "assistant"`) — with no `send` command, so
    /// it never drives a turn. `extra` carries `voice: true`, `callId`, speaker attribution.
    /// Honors an open [`Self::transact`] (only the outermost commits).
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
        self.commit_now();
    }

    /// Box write: a one-line `system` message with a `note` status part (`code: "note"`).
    /// Neither Mac nor iPhone renders this shape in the transcript today — use
    /// [`Self::append_system_line`] for client-visible system lines. Kept for box
    /// paths (routines / vault / voice) that already write it.
    /// Honors an open [`Self::transact`] (only the outermost commits).
    pub fn append_system_note(&self, msg_id: &str, sender: &str, text: &str, device_id: &str) {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        m.insert("id", msg_id).ok();
        m.insert("role", "system").ok();
        m.insert("sender", sender).ok();
        m.insert("createdAt", now_ms()).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("status", MessageStatus::Done.as_str()).ok();
        m.insert_container("reactions", LoroMap::new())
            .expect("reactions");
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "status").ok();
        part.insert("code", "note").ok();
        part.insert("message", text).ok();
        self.commit_now();
    }

    /// Client write: the user's answer to a `question` part as a user message (rendered text)
    /// plus an `answer` command carrying the structured value.
    /// Honors an open [`Self::transact`] (only the outermost commits).
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
        m.insert_container("reactions", LoroMap::new())
            .expect("reactions");
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        let cmd_id = self.push_command(
            "answer",
            serde_json::json!({ "messageId": msg_id, "questionId": question_id, "value": value, "text": text }),
            device_id,
        );
        self.commit_now();
        cmd_id
    }

    /// Box / CLI write: append a command row and commit (unless inside [`Self::transact`]).
    /// Honors an open `transact` (only the outermost commits). Returns the command id.
    pub fn append_command(&self, kind: &str, payload: Value, device_id: &str) -> String {
        let id = self.push_command(kind, payload, device_id);
        self.commit_now();
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
            self.commit_now();
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
        m.insert_container("reactions", LoroMap::new())
            .expect("reactions");
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part_id = self.add_text_part_inner(&parts, msg_id, "text");
        self.commit_now();
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
        self.commit_now();
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
        self.commit_now();
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
        self.commit_now();
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
        self.commit_now();
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
        self.commit_now();
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
        self.commit_now();
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
        self.commit_now();
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
        self.commit_now();
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
                self.commit_now();
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
        self.commit_now();
        Some(part_id)
    }

    /// Set scalar fields on a message (status, decision, subagentId, model, usage, completedAt, …).
    pub fn set_message_fields(&mut self, msg_id: &str, fields: &[(&str, Value)]) {
        if let Some(m) = self.find_message(msg_id) {
            for (k, v) in fields {
                m.insert(k, json_to_lv(v)).ok();
            }
            self.commit_now();
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
            self.commit_now();
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
            d.append_user_message("m1", "hello", "dev:mac", &UserMessageOpts::default())
                .unwrap();
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

    /// Optional extras for the iOS-shaped golden mirror (attachments, mentions, reply, author).
    #[derive(Default)]
    struct IosMirrorUserExtras<'a> {
        attachments: &'a [(&'a str, &'a str, Option<&'a str>, Option<&'a str>)],
        mentions: &'a [&'a str],
        reply_to: Option<&'a str>,
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
        extras: &IosMirrorUserExtras<'_>,
    ) {
        let msgs = doc.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push");
        m.insert("id", msg_id).ok();
        m.insert("chatId", "chat1").ok();
        m.insert("role", "user").ok();
        m.insert("sender", "user").ok();
        m.insert("status", "done").ok();
        m.insert("deviceId", device_id).ok();
        m.insert("createdAt", now_ms()).ok();
        m.insert("waitingUser", false).ok();
        m.insert("pending", false).ok();
        if let Some(r) = extras.reply_to {
            m.insert("replyTo", r).ok();
        }
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
            if let Some(r) = blob_ref {
                a.insert("ref", *r).ok();
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

    /// Rust mirror of a D82 greeting row shaped like iOS/Mac seed data.
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

    #[test]
    fn ios_mirror_rows_decode_through_core() {
        let doc = ChatDoc::new(None, false);
        doc.transact(|d| {
            ios_mirror_insert_user_message(
                d,
                "m1",
                "hello from ios",
                "dev:ios",
                &IosMirrorUserExtras {
                    attachments: &[
                        ("pic.png", "image/png", Some("image"), Some("blob-1")),
                        ("notes.txt", "text/plain", None, None),
                    ],
                    mentions: &["agent:p1", "user:sam"],
                    reply_to: Some("m0"),
                    sender_handle: Some("ken"),
                    sender_name: Some("Ken"),
                    sender_avatar: Some("avatar-1"),
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
        assert_eq!(user["replyTo"], "m0");
        assert_eq!(user["senderHandle"], "ken");
        assert_eq!(user["senderName"], "Ken");
        assert_eq!(user["senderAvatar"], "avatar-1");
        assert_eq!(user["mentions"][0], "agent:p1");
        assert_eq!(user["mentions"][1], "user:sam");
        assert_eq!(user["parts"][0]["kind"], "attachment");
        assert_eq!(user["parts"][0]["attachment"]["name"], "pic.png");
        assert_eq!(user["parts"][0]["attachment"]["ref"], "blob-1");
        assert_eq!(user["parts"][1]["attachment"]["name"], "notes.txt");
        assert!(user["parts"][1]["attachment"].get("ref").is_none());
        let row = &raw["messages"][1];
        assert_eq!(row["callId"], "call-1");
        assert_eq!(row["call_id"], "call-1");
        assert!(row.get("hiddenParts").is_some());
        let greet = &raw["messages"][2];
        assert_eq!(greet["greeting"], true);
        assert_eq!(greet["sender"], "agent:proj1");
        assert!(doc.has_greeting());
        let view = doc.message("m1").unwrap();
        assert_eq!(view.text(), "hello from ios");
        assert_eq!(
            view.rest.get("senderHandle").and_then(Value::as_str),
            Some("ken")
        );
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
                    reply_to: Some("prev".into()),
                    sender: Some("user:ken".into()),
                    sender_handle: Some("ken".into()),
                    sender_name: Some("Ken".into()),
                    mentions: vec!["agent:p1".into()],
                    attachments: vec![UserAttachment {
                        name: "pic.png".into(),
                        mime: "image/png".into(),
                        kind: Some("image".into()),
                        blob_ref: Some("blob-1".into()),
                    }],
                    ..UserMessageOpts::default()
                },
            )
            .unwrap();
            d.push_command("send", serde_json::json!({ "messageId": "m1" }), "dev:a");
            d.append_system_line("m2", "Sam started a video call", "dev:a", Some("c1"))
                .unwrap();
            d.append_greeting("m3", "Hi, I'm Arbos.", "dev:a", "proj1", "Arbos")
                .unwrap();
            d.set_reaction("m1", "👍", "user:ken", true).unwrap();
            d.set_model_settings("gpt-test", "high").unwrap();
        });

        let row = &a.to_json()["messages"][0];
        assert!(row.get("chatId").is_none());
        assert!(row.get("pending").is_none());
        assert!(row.get("hiddenParts").is_none());
        assert!(row.get("waitingUser").is_none());
        assert!(row.get("reactions").is_some());
        assert_eq!(row["replyTo"], "prev");
        assert_eq!(row["senderHandle"], "ken");
        let sys = &a.to_json()["messages"][1];
        assert_eq!(sys["callId"], "c1");
        assert!(sys.get("call_id").is_none());
        let greet = &a.to_json()["messages"][2];
        assert_eq!(greet["greeting"], true);
        assert_eq!(greet["sender"], "agent:proj1");
        assert!(a.has_greeting());
        assert_eq!(a.to_json()["settings"]["agentModel"], "gpt-test");
        assert_eq!(a.to_json()["settings"]["agentEffort"], "high");

        b.import(&a.export_updates_since(&base_a)).unwrap();
        assert_eq!(b.messages().len(), 3);
        assert_eq!(b.messages()[0].text(), "hi");
        assert_eq!(b.messages()[1].text(), "Sam started a video call");
        assert_eq!(b.messages()[2].text(), "Hi, I'm Arbos.");
        assert_eq!(b.pending_commands().len(), 1);
        let mut row = b.to_json()["messages"][0].clone();
        project_reactions(&mut row);
        assert_eq!(row["reactions"]["👍"]["user:ken"], true);
        // Storage is flat keys under reactions.
        assert_eq!(
            b.to_json()["messages"][0]["reactions"][reaction_key("👍", "user:ken").as_str()],
            true
        );
        assert_eq!(
            b.to_json()["settings"]["models"]["talking_model"]["id"],
            "gpt-test"
        );
    }

    #[test]
    fn none_peer_uses_loro_random_and_some_sets_explicit() {
        let random = ChatDoc::new(None, false);
        let explicit = ChatDoc::new(Some(42), false);
        assert_eq!(explicit.doc.peer_id(), 42);
        // Random peer is non-zero in practice; just ensure construction works.
        let _ = random.doc.peer_id();
    }

    #[test]
    fn none_peer_twice_yields_distinct_peer_ids() {
        let a = ChatDoc::new(None, false);
        let b = ChatDoc::new(None, false);
        assert_ne!(a.doc.peer_id(), b.doc.peer_id());
    }

    #[test]
    fn concurrent_reactions_and_messages_converge() {
        let mut a = ChatDoc::new(Some(10), false);
        let mut b = ChatDoc::new(Some(20), false);
        a.transact(|d| {
            d.append_user_message("shared", "base", "dev:a", &UserMessageOpts::default())
                .unwrap();
        });
        b.import(&a.snapshot()).unwrap();

        let base_a = a.oplog_vv();
        let base_b = b.oplog_vv();
        a.transact(|d| {
            d.set_reaction("shared", "👍", "user:a", true).unwrap();
            d.set_reaction("shared", "🎉", "user:a", true).unwrap();
            d.append_user_message(
                "from-a",
                "hello from a",
                "dev:a",
                &UserMessageOpts::default(),
            )
            .unwrap();
        });
        b.transact(|d| {
            d.set_reaction("shared", "❤️", "user:b", true).unwrap();
            d.set_reaction("shared", "👍", "user:b", true).unwrap();
            d.append_user_message(
                "from-b",
                "hello from b",
                "dev:b",
                &UserMessageOpts::default(),
            )
            .unwrap();
        });

        let upd_a = a.export_updates_since(&base_a);
        let upd_b = b.export_updates_since(&base_b);
        a.import(&upd_b).unwrap();
        b.import(&upd_a).unwrap();

        for peer in [&a, &b] {
            let json = peer.to_json();
            let mut shared = json["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["id"] == "shared")
                .cloned()
                .unwrap();
            project_reactions(&mut shared);
            assert_eq!(shared["reactions"]["👍"]["user:a"], true);
            assert_eq!(shared["reactions"]["👍"]["user:b"], true);
            assert_eq!(shared["reactions"]["🎉"]["user:a"], true);
            assert_eq!(shared["reactions"]["❤️"]["user:b"], true);
            let ids: Vec<_> = peer.messages().into_iter().map(|m| m.id).collect();
            assert!(ids.contains(&"from-a".into()));
            assert!(ids.contains(&"from-b".into()));
            assert!(ids.contains(&"shared".into()));
        }
    }

    #[test]
    fn concurrent_same_emoji_first_react_both_survive() {
        // Pins the flat "emoji|sender" shape: two peers reacting on=true for the
        // same emoji on a fresh reactions map both keep their sender after merge.
        let mut a = ChatDoc::new(Some(11), false);
        let mut b = ChatDoc::new(Some(22), false);
        a.transact(|d| {
            d.append_user_message("m", "hi", "dev:a", &UserMessageOpts::default())
                .unwrap();
        });
        b.import(&a.snapshot()).unwrap();
        let base_a = a.oplog_vv();
        let base_b = b.oplog_vv();
        a.transact(|d| d.set_reaction("m", "👍", "user:a", true).unwrap());
        b.transact(|d| d.set_reaction("m", "👍", "user:b", true).unwrap());
        a.import(&b.export_updates_since(&base_b)).unwrap();
        b.import(&a.export_updates_since(&base_a)).unwrap();
        for peer in [&a, &b] {
            let mut row = peer.to_json()["messages"][0].clone();
            project_reactions(&mut row);
            assert_eq!(row["reactions"]["👍"]["user:a"], true);
            assert_eq!(row["reactions"]["👍"]["user:b"], true);
        }
    }

    #[test]
    fn set_reaction_off_is_noop_when_reactions_missing() {
        let doc = ChatDoc::new(None, false);
        // Legacy-shaped row with no reactions map.
        let msgs = doc.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push");
        m.insert("id", "legacy").ok();
        m.insert("role", "user").ok();
        m.insert("sender", "user").ok();
        m.insert("status", "done").ok();
        m.insert("deviceId", "dev").ok();
        m.insert("createdAt", now_ms()).ok();
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, "hi").ok();
        doc.doc.commit();

        doc.set_reaction("legacy", "👍", "user:x", false).unwrap();
        let row = &doc.to_json()["messages"][0];
        assert!(row.get("reactions").is_none());
    }

    #[test]
    fn snapshot_and_export_skip_commit_while_transact_open() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let doc = ChatDoc::new(Some(3), false);
        let commits = Arc::new(AtomicUsize::new(0));
        let commits_cb = commits.clone();
        let _sub = doc.doc.subscribe_local_update(Box::new(move |_| {
            commits_cb.fetch_add(1, Ordering::SeqCst);
            true
        }));
        let before = doc.oplog_vv();
        doc.transact(|d| {
            d.append_user_message("m1", "hi", "dev", &UserMessageOpts::default())
                .unwrap();
            assert!(
                d.snapshot().is_empty(),
                "snapshot must not export (commit) inside transact"
            );
            assert!(
                d.export_updates_since(&before).is_empty(),
                "export_updates_since must not export (commit) inside transact"
            );
            assert_eq!(
                commits.load(Ordering::SeqCst),
                0,
                "snapshot/export must not commit inside transact"
            );
        });
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert!(!doc.snapshot().is_empty());
    }

    #[test]
    fn nested_transact_commits_only_at_outermost() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let doc = ChatDoc::new(Some(7), false);
        let commits = Arc::new(AtomicUsize::new(0));
        let commits_cb = commits.clone();
        let _sub = doc.doc.subscribe_local_update(Box::new(move |_| {
            commits_cb.fetch_add(1, Ordering::SeqCst);
            true
        }));
        let before = doc.oplog_vv();
        doc.transact(|outer| {
            outer
                .append_user_message("m1", "hi", "dev", &UserMessageOpts::default())
                .unwrap();
            // Inner append_command used to commit immediately; with re-entrant
            // transact it must wait for the outer frame.
            outer.transact(|inner| {
                let _ =
                    inner.append_command("send", serde_json::json!({ "messageId": "m1" }), "dev");
            });
            assert_eq!(
                commits.load(Ordering::SeqCst),
                0,
                "no commit until outermost transact returns"
            );
        });
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        let upd = doc.export_updates_since(&before);
        let mut peer = ChatDoc::new(Some(8), false);
        peer.import(&upd).unwrap();
        assert_eq!(peer.messages().len(), 1);
        assert_eq!(peer.pending_commands().len(), 1);
    }

    #[test]
    fn push_command_payload_is_nested_map_container() {
        let doc = ChatDoc::new(None, false);
        doc.transact(|d| {
            d.push_command(
                "send",
                serde_json::json!({ "messageId": "m1", "nested": { "ok": true } }),
                "dev",
            );
        });
        let cmds = doc.commands_list();
        let Some(ValueOrContainer::Container(Container::Map(c))) = cmds.get(0) else {
            panic!("expected command map");
        };
        match c.get("payload") {
            Some(ValueOrContainer::Container(Container::Map(payload))) => {
                match payload.get("nested") {
                    Some(ValueOrContainer::Container(Container::Map(_))) => {}
                    other => panic!("expected nested map container, got {other:?}"),
                }
            }
            other => panic!("expected payload map container, got {other:?}"),
        }
        assert_eq!(doc.commands()[0].payload["messageId"], "m1");
        assert_eq!(doc.commands()[0].payload["nested"]["ok"], true);
    }

    #[test]
    fn project_hidden_parts_moves_hidden_into_hidden_parts() {
        let mut json = serde_json::json!({
            "id": "m1",
            "parts": [
                { "kind": "text", "text": "visible" },
                { "kind": "tool", "hidden": true, "call": { "id": "t1" } },
            ]
        });
        project_hidden_parts(&mut json);
        assert_eq!(json["parts"].as_array().unwrap().len(), 1);
        assert_eq!(json["parts"][0]["kind"], "text");
        assert_eq!(json["hiddenParts"].as_array().unwrap().len(), 1);
        assert_eq!(json["hiddenParts"][0]["kind"], "tool");
    }
}
