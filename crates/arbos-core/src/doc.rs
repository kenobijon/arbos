//! The chat doc: one Loro document per chat (crdt-sync §6.2). Text bodies
//! are LoroText and are only appended while streaming; the `commands` list
//! is the client→box outbox. The relay never sees this module: it stores
//! opaque bytes.

use std::collections::HashMap;

use loro::{
    Container, ExportMode, LoroDoc, LoroList, LoroMap, LoroText, LoroValue, ValueOrContainer,
    VersionVector,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{now_ms, ulid};

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
}

fn lv_to_json(v: &LoroValue) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn json_to_lv(v: &Value) -> LoroValue {
    serde_json::from_value(v.clone()).unwrap_or(LoroValue::Null)
}

impl ChatDoc {
    /// A new empty chat doc. `peer` must be unique per device; the box records timestamps (crdt-sync R7).
    pub fn new(peer: u64, record_timestamps: bool) -> Self {
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).expect("peer id");
        doc.set_record_timestamp(record_timestamps);
        Self {
            doc,
            live_text: HashMap::new(),
        }
    }

    /// Write `meta` on a brand-new doc (box side).
    pub fn init_meta(&self, chat_id: &str, project_id: &str, epoch: i64) {
        let meta = self.doc.get_map("meta");
        meta.insert("chatId", chat_id).ok();
        meta.insert("projectId", project_id).ok();
        meta.insert("schemaVersion", SCHEMA_VERSION).ok();
        meta.insert("epoch", epoch).ok();
        self.doc.commit();
    }

    pub fn from_snapshot(
        peer: u64,
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

    /// Client write (crdt-sync §6.2): user message plus a `send` command in one commit.
    pub fn append_user_message(&self, msg_id: &str, text: &str, device_id: &str) -> String {
        let msgs = self.messages_list();
        let m = msgs.push_container(LoroMap::new()).expect("push message");
        let now = now_ms();
        m.insert("id", msg_id).ok();
        m.insert("role", "user").ok();
        m.insert("sender", "user").ok();
        m.insert("createdAt", now).ok();
        m.insert("deviceId", device_id).ok();
        m.insert("status", MessageStatus::Done.as_str()).ok();
        let parts = m.insert_container("parts", LoroList::new()).expect("parts");
        let part = parts.push_container(LoroMap::new()).expect("part");
        part.insert("id", ulid()).ok();
        part.insert("kind", "text").ok();
        let t = part
            .insert_container("text", LoroText::new())
            .expect("text");
        t.insert(0, text).ok();
        let cmd_id = self.append_command(
            "send",
            serde_json::json!({ "messageId": msg_id }),
            device_id,
        );
        self.doc.commit();
        cmd_id
    }

    pub fn append_command(&self, kind: &str, payload: Value, device_id: &str) -> String {
        let cmds = self.commands_list();
        let c = cmds.push_container(LoroMap::new()).expect("push command");
        let id = ulid();
        c.insert("id", id.as_str()).ok();
        c.insert("kind", kind).ok();
        c.insert("payload", json_to_lv(&payload)).ok();
        c.insert("issuedBy", device_id).ok();
        c.insert("issuedAt", now_ms()).ok();
        c.insert("status", CommandStatus::Pending.as_str()).ok();
        self.doc.commit();
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
            self.doc.commit();
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
        self.doc.commit();
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
        self.doc.commit();
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
        self.doc.commit();
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
        self.doc.commit();
        Some(part_id)
    }

    /// Add a `status` part. Codes: `deciding`, `queued`, `working`, `step` (short label of one tool
    /// call), `done`, `failed`, `waiting_user`, `handed_off`. These are the only progress the
    /// transcript renders; tool parts are `hidden`.
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
        self.doc.commit();
        Some(part_id)
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
        self.doc.commit();
        Some(part_id)
    }

    /// Set scalar fields on a message (status, decision, subagentId, model, usage, completedAt, …).
    pub fn set_message_fields(&mut self, msg_id: &str, fields: &[(&str, Value)]) {
        if let Some(m) = self.find_message(msg_id) {
            for (k, v) in fields {
                m.insert(k, json_to_lv(v)).ok();
            }
            self.doc.commit();
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
        let mut box_doc = ChatDoc::new(1, true);
        box_doc.init_meta("chat1", "proj1", 1);
        let mut client = ChatDoc::new(2, false);
        client.import(&box_doc.snapshot()).unwrap();

        let base = client.oplog_vv();
        client.append_user_message("m1", "hello", "dev:mac");
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
}
