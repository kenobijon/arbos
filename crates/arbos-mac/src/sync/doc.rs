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
        let json = serde_json::to_value(map.get_deep_value()).ok()?;
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
