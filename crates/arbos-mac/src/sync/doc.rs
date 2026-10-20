//! Chat doc on Loro, shaped as `crdt-sync.md` §6.2:
//!
//! ```text
//! meta:      Map { chatId, projectId, schemaVersion, epoch }
//! messages:  List<Map { id, role, ..., parts: List<Map { kind, text: Text, ... }> }>
//! commands:  List<Map { id, kind, payload, issuedBy, issuedAt, status }>
//! ```
//!
//! Text bodies are `LoroText` and are appended while streaming. Reading goes
//! through `get_deep_value` → JSON → [`Message`], so the schema stays additive.

use std::collections::BTreeSet;

use anyhow::{Context as _, Result};
use loro::event::DiffEvent;
use loro::{Container, Index, LoroDoc, LoroList, LoroMap, LoroText, LoroValue, ValueOrContainer};
use serde_json::Value;

use crate::model::{now_ms, Id, Message, MessageStatus, Part};

pub const SCHEMA_VERSION: i64 = 1;

/// What a root-level diff touched, mapped to message indexes.
#[derive(Debug, Default)]
pub struct Touched {
    /// The `messages` list itself changed (insert/delete): re-read everything.
    pub list_changed: bool,
    pub messages: BTreeSet<usize>,
}

pub struct ChatDoc {
    pub chat_id: Id,
    pub doc: LoroDoc,
}

impl ChatDoc {
    pub fn new(chat_id: &str, project_id: &str) -> Self {
        let doc = LoroDoc::new();
        let meta = doc.get_map("meta");
        let _ = meta.insert("chatId", chat_id);
        let _ = meta.insert("projectId", project_id);
        let _ = meta.insert("schemaVersion", SCHEMA_VERSION);
        let _ = meta.insert("epoch", 1i64);
        doc.commit();
        Self {
            chat_id: chat_id.to_string(),
            doc,
        }
    }

    /// A replica with no local `meta`: the box writes it and it arrives by sync.
    pub fn empty(chat_id: &str) -> Self {
        Self {
            chat_id: chat_id.to_string(),
            doc: LoroDoc::new(),
        }
    }

    pub fn messages(&self) -> LoroList {
        self.doc.get_list("messages")
    }

    pub fn commands(&self) -> LoroList {
        self.doc.get_list("commands")
    }

    pub fn len(&self) -> usize {
        self.messages().len()
    }

    pub fn commit(&self) {
        self.doc.commit();
    }

    pub fn index_of(&self, message_id: &str) -> Option<usize> {
        let list = self.messages();
        (0..list.len()).find(|&ix| {
            message_map(&list, ix)
                .and_then(|m| m.get("id"))
                .and_then(|v| match v {
                    ValueOrContainer::Value(LoroValue::String(s)) => Some(s.as_str() == message_id),
                    ValueOrContainer::Value(_) | ValueOrContainer::Container(_) => None,
                })
                .unwrap_or(false)
        })
    }

    pub fn read_message(&self, ix: usize) -> Option<Message> {
        let map = message_map(&self.messages(), ix)?;
        let mut json = serde_json::to_value(map.get_deep_value()).ok()?;
        split_hidden_parts(&mut json);
        serde_json::from_value(json).ok()
    }

    pub fn read_all(&self) -> Vec<Message> {
        (0..self.len())
            .filter_map(|ix| self.read_message(ix))
            .collect()
    }

    /// Append a message as nested containers. Does not commit.
    pub fn insert_message(&self, message: &Message) -> Result<usize> {
        let list = self.messages();
        let ix = list.len();
        let map = list.insert_container(ix, LoroMap::new())?;
        let json = serde_json::to_value(message)?;
        let Value::Object(fields) = json else {
            anyhow::bail!("message did not serialize to an object");
        };
        for (key, value) in fields {
            insert_json(&map, &key, value, false)?;
        }
        Ok(ix)
    }

    /// Toggle one emoji reaction for `sender` on a message — the iPhone's shape,
    /// `reactions: Map { emoji: Map { sender: true } }`, several emojis per
    /// message. Does not commit.
    pub fn toggle_reaction(&self, message_id: &str, emoji: &str, sender: &str) -> Result<()> {
        if emoji.is_empty() || sender.is_empty() {
            return Ok(());
        }
        let list = self.messages();
        let Some(ix) = self.index_of(message_id) else {
            return Ok(());
        };
        let map = message_map(&list, ix).context("message row is not a map")?;
        let reactions = match map.get("reactions") {
            Some(ValueOrContainer::Container(Container::Map(m))) => m,
            _ => map.insert_container("reactions", LoroMap::new())?,
        };
        let bucket = match reactions.get(emoji) {
            Some(ValueOrContainer::Container(Container::Map(m))) => m,
            _ => reactions.insert_container(emoji, LoroMap::new())?,
        };
        match bucket.get(sender) {
            Some(ValueOrContainer::Value(LoroValue::Bool(true))) => bucket.delete(sender)?,
            _ => bucket.insert(sender, true)?,
        }
        Ok(())
    }

    /// Append a `commands` row (`send`, `stop`, `answer`, `retry`, `retitle`).
    pub fn push_command(&self, kind: &str, payload: Value, device: &str) -> Result<()> {
        let list = self.commands();
        let map = list.insert_container(list.len(), LoroMap::new())?;
        map.insert("id", crate::model::new_id())?;
        map.insert("kind", kind)?;
        insert_json(&map, "payload", payload, false)?;
        map.insert("issuedBy", device)?;
        map.insert("issuedAt", now_ms() as i64)?;
        map.insert("status", "pending")?;
        Ok(())
    }

    pub fn set_field(&self, ix: usize, key: &str, value: Value) -> Result<()> {
        let map = message_map(&self.messages(), ix).context("message index out of range")?;
        insert_json(&map, key, value, false)
    }

    pub fn set_status(&self, ix: usize, status: MessageStatus) -> Result<()> {
        self.set_field(ix, "status", serde_json::to_value(status)?)?;
        if matches!(
            status,
            MessageStatus::Done | MessageStatus::Error | MessageStatus::Cancelled
        ) {
            self.set_field(ix, "completedAt", Value::from(now_ms()))?;
        }
        Ok(())
    }

    /// Append a part. Returns the part index. Does not commit.
    pub fn push_part(&self, ix: usize, part: &Part) -> Result<usize> {
        let parts = parts_list(&self.messages(), ix).context("message has no parts list")?;
        let pix = parts.len();
        let map = parts.insert_container(pix, LoroMap::new())?;
        let Value::Object(fields) = serde_json::to_value(part)? else {
            anyhow::bail!("part did not serialize to an object");
        };
        for (key, value) in fields {
            insert_json(&map, &key, value, true)?;
        }
        Ok(pix)
    }

    /// Replace a part in place (same index). Used to fold a status row into
    /// the reply that follows it, or to flip a tool card's status.
    pub fn replace_part(&self, ix: usize, pix: usize, part: &Part) -> Result<()> {
        let parts = parts_list(&self.messages(), ix).context("message has no parts list")?;
        parts.delete(pix, 1)?;
        let map = parts.insert_container(pix, LoroMap::new())?;
        let Value::Object(fields) = serde_json::to_value(part)? else {
            anyhow::bail!("part did not serialize to an object");
        };
        for (key, value) in fields {
            insert_json(&map, &key, value, true)?;
        }
        Ok(())
    }

    /// Box-side write (the fixture plays the box): flip the `question` part with `question_id`
    /// to `answered` and record the value. Returns the message index it lives in.
    pub fn answer_question(&self, question_id: &str, value: &Value) -> Option<usize> {
        let list = self.messages();
        for ix in 0..list.len() {
            let Some(parts) = parts_list(&list, ix) else {
                continue;
            };
            for pix in 0..parts.len() {
                let Some(map) = container_map(parts.get(pix)) else {
                    continue;
                };
                let hit = matches!(map.get("questionId"), Some(ValueOrContainer::Value(LoroValue::String(s))) if s.as_str() == question_id);
                if !hit {
                    continue;
                }
                let _ = insert_json(&map, "answer", value.clone(), true);
                let _ = map.insert("state", "answered");
                let _ = map.insert("answeredAt", now_ms() as i64);
                let _ = self.set_field(ix, "waitingUser", Value::Bool(false));
                return Some(ix);
            }
        }
        None
    }

    pub fn set_part_field(&self, ix: usize, pix: usize, key: &str, value: Value) -> Result<()> {
        let parts = parts_list(&self.messages(), ix).context("message has no parts list")?;
        let map = container_map(parts.get(pix)).context("part index out of range")?;
        insert_json(&map, key, value, true)
    }

    /// Append streamed text to a text/reasoning part. Does not commit.
    pub fn append_text(&self, ix: usize, pix: usize, text: &str) -> Result<()> {
        let parts = parts_list(&self.messages(), ix).context("message has no parts list")?;
        let map = container_map(parts.get(pix)).context("part index out of range")?;
        match map.get("text") {
            Some(ValueOrContainer::Container(Container::Text(t))) => {
                t.push_str(text)?;
            }
            Some(ValueOrContainer::Container(_)) | Some(ValueOrContainer::Value(_)) | None => {
                let t = map.insert_container("text", LoroText::new())?;
                t.push_str(text)?;
            }
        }
        Ok(())
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
}

/// Move `parts[*]` with `hidden: true` into `hiddenParts` so the transcript
/// never sees them while the Details drawer still can.
fn split_hidden_parts(json: &mut Value) {
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

fn message_map(list: &LoroList, ix: usize) -> Option<LoroMap> {
    container_map(list.get(ix))
}

fn parts_list(list: &LoroList, ix: usize) -> Option<LoroList> {
    match message_map(list, ix)?.get("parts") {
        Some(ValueOrContainer::Container(Container::List(l))) => Some(l),
        Some(ValueOrContainer::Container(_)) | Some(ValueOrContainer::Value(_)) | None => None,
    }
}

fn container_map(v: Option<ValueOrContainer>) -> Option<LoroMap> {
    match v {
        Some(ValueOrContainer::Container(Container::Map(m))) => Some(m),
        Some(ValueOrContainer::Container(_)) | Some(ValueOrContainer::Value(_)) | None => None,
    }
}

/// Write a JSON value under `key`. Objects become `LoroMap`, arrays `LoroList`.
/// Inside a part (`in_part`), the `text` / `reasoning` string becomes a
/// `LoroText` so it can be appended while streaming.
fn insert_json(map: &LoroMap, key: &str, value: Value, in_part: bool) -> Result<()> {
    match value {
        Value::Null => {
            map.insert(key, LoroValue::Null)?;
        }
        Value::Bool(b) => {
            map.insert(key, b)?;
        }
        Value::Number(n) => {
            map.insert(key, number_value(&n))?;
        }
        Value::String(s) => {
            if in_part && (key == "text" || key == "reasoning") {
                let t = map.insert_container(key, LoroText::new())?;
                t.push_str(&s)?;
            } else {
                map.insert(key, s)?;
            }
        }
        Value::Array(items) => {
            let list = map.insert_container(key, LoroList::new())?;
            let child_is_part = key == "parts";
            for (i, item) in items.into_iter().enumerate() {
                insert_json_list(&list, i, item, child_is_part)?;
            }
        }
        Value::Object(fields) => {
            let child = map.insert_container(key, LoroMap::new())?;
            for (k, v) in fields {
                insert_json(&child, &k, v, false)?;
            }
        }
    }
    Ok(())
}

fn insert_json_list(list: &LoroList, pos: usize, value: Value, is_part: bool) -> Result<()> {
    match value {
        Value::Null => {
            list.insert(pos, LoroValue::Null)?;
        }
        Value::Bool(b) => {
            list.insert(pos, b)?;
        }
        Value::Number(n) => {
            list.insert(pos, number_value(&n))?;
        }
        Value::String(s) => {
            list.insert(pos, s)?;
        }
        Value::Array(items) => {
            let child = list.insert_container(pos, LoroList::new())?;
            for (i, item) in items.into_iter().enumerate() {
                insert_json_list(&child, i, item, false)?;
            }
        }
        Value::Object(fields) => {
            let child = list.insert_container(pos, LoroMap::new())?;
            for (k, v) in fields {
                insert_json(&child, &k, v, is_part)?;
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod core_writer_tests {
    use super::*;
    use crate::model::{Part, Role};
    use arbos_core::doc::{ChatDoc as CoreDoc, UserAttachment, UserMessageOpts};

    fn message_from_core_row(row: &serde_json::Value) -> Message {
        let mut json = row.clone();
        split_hidden_parts(&mut json);
        serde_json::from_value(json).expect("core row deserializes as Message")
    }

    #[test]
    fn mac_sync_doc_rows_decode_through_core() {
        let mac = ChatDoc::empty("chat1");
        let text = "hello from mac";
        let msg = Message {
            id: "m1".into(),
            chat_id: "chat1".into(),
            role: Role::User,
            sender: "user".into(),
            status: MessageStatus::Done,
            turn: None,
            decision: None,
            subagent_id: None,
            subagent_name: None,
            handoff_messages: None,
            reactions: Default::default(),
            in_reply_to: None,
            routine_name: None,
            sender_name: None,
            sender_icon: None,
            sender_handle: None,
            sender_avatar: None,
            sender_avatar_color: None,
            mentions: Vec::new(),
            reply_to: None,
            asked_by_handle: None,
            owner_handle: None,
            usage: None,
            return_contract: None,
            waiting_user: false,
            waiting_approval: false,
            model: None,
            effort: None,
            created_at: now_ms(),
            completed_at: None,
            call_id: None,
            activity: None,
            pending: false,
            parts: vec![Part::Text {
                id: "p1".into(),
                text: text.into(),
            }],
            hidden_parts: Vec::new(),
            greeting: false,
        };
        mac.insert_message(&msg).unwrap();
        mac.commit();

        let mut core = CoreDoc::new(None, false);
        core.import(&mac.doc.export(loro::ExportMode::Snapshot).unwrap())
            .unwrap();
        let msgs = core.messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text(), text);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn core_client_writers_deserialize_as_mac_message() {
        let core = CoreDoc::new(None, false);
        let plain = "plain text for mac";
        core.transact(|d| {
            d.append_user_message(
                "m1",
                plain,
                "dev:mac",
                &UserMessageOpts {
                    attachments: &[UserAttachment {
                        name: "a.png",
                        mime: "image/png",
                        kind: Some("image"),
                        blob_ref: Some("ref1"),
                    }],
                    reply_to: Some("other"),
                    ..UserMessageOpts::default()
                },
            );
            d.append_system_line("m2", "Call ended · 1 min", "dev:mac", Some("call-9"));
            d.append_greeting("m3", "Hi, I'm Arbos.", "dev:mac", "proj", "Arbos");
            d.set_reaction("m1", "heart", "user:1", true);
        });

        let rows = core.to_json()["messages"].as_array().cloned().unwrap();
        assert_eq!(rows.len(), 3);

        let user = message_from_core_row(&rows[0]);
        assert_eq!(user.plain_text(), plain);
        assert_eq!(user.role, Role::User);
        assert_eq!(user.reply_to.as_deref(), Some("other"));
        assert!(user
            .reactions
            .get("heart")
            .unwrap()
            .get("user:1")
            .copied()
            .unwrap());
        assert_eq!(user.parts.len(), 2);

        let sys = message_from_core_row(&rows[1]);
        assert_eq!(sys.plain_text(), "Call ended · 1 min");
        assert_eq!(sys.role, Role::System);
        assert_eq!(sys.call_id.as_deref(), Some("call-9"));

        let greet = message_from_core_row(&rows[2]);
        assert_eq!(greet.plain_text(), "Hi, I'm Arbos.");
        assert!(greet.greeting);
        assert_eq!(greet.sender, "agent:proj");
    }
}
