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
use loro::{Container, LoroList, LoroMap, LoroText, LoroValue, ValueOrContainer};
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
    pub core: arbos_core::doc::ChatDoc,
}

impl ChatDoc {
    pub fn new(chat_id: &str, project_id: &str) -> Self {
        let core = arbos_core::doc::ChatDoc::new(None, false);
        let meta = core.doc.get_map("meta");
        let _ = meta.insert("chatId", chat_id);
        let _ = meta.insert("projectId", project_id);
        let _ = meta.insert("schemaVersion", SCHEMA_VERSION);
        let _ = meta.insert("epoch", 1i64);
        core.doc.commit();
        Self {
            chat_id: chat_id.to_string(),
            core,
        }
    }

    /// A replica with no local `meta`: the box writes it and it arrives by sync.
    pub fn empty(chat_id: &str) -> Self {
        Self {
            chat_id: chat_id.to_string(),
            core: arbos_core::doc::ChatDoc::new(None, false),
        }
    }

    pub fn messages(&self) -> LoroList {
        self.core.doc.get_list("messages")
    }

    pub fn len(&self) -> usize {
        self.messages().len()
    }

    pub fn commit(&self) {
        self.core.doc.commit();
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
        arbos_core::doc::project_hidden_parts(&mut json);
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

    /// Fixture helper: append a `commands` row. Does not commit.
    /// Production relay calls `core.push_command` inside `core.transact`.
    pub fn push_command(&self, kind: &str, payload: Value, device: &str) -> Result<()> {
        let _ = self.core.push_command(kind, payload, device);
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

    /// Map a root diff to message indexes (delegates to `arbos_core::doc::touched`).
    pub fn touched(event: &DiffEvent<'_>) -> Touched {
        let t = arbos_core::doc::touched(event);
        Touched {
            list_changed: t.list_changed,
            messages: t.messages,
        }
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
    use arbos_core::doc::{UserAttachment, UserMessageOpts};
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn message_from_core_row(row: &serde_json::Value) -> Message {
        let mut json = row.clone();
        arbos_core::doc::project_hidden_parts(&mut json);
        serde_json::from_value(json).expect("core row deserializes as Message")
    }

    /// Drop nulls and Mac-only placeholders core never writes (chatId, pending,
    /// hiddenParts, false/empty defaults that serde still emits on Message).
    fn client_row_keys(row: &Value) -> BTreeSet<String> {
        let Some(obj) = row.as_object() else {
            return BTreeSet::new();
        };
        obj.iter()
            .filter(|(k, v)| {
                if v.is_null() {
                    return false;
                }
                match k.as_str() {
                    "chatId" | "pending" | "hiddenParts" => false,
                    "waitingUser" | "waitingApproval" | "greeting" => v.as_bool() != Some(false),
                    "mentions" | "reactions" => !v
                        .as_array()
                        .map(|a| a.is_empty())
                        .unwrap_or(v.as_object().map(|o| o.is_empty()).unwrap_or(false)),
                    _ => true,
                }
            })
            .map(|(k, _)| k.clone())
            .collect()
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

        let mut core = arbos_core::doc::ChatDoc::new(None, false);
        core.import(&mac.core.doc.export(loro::ExportMode::Snapshot).unwrap())
            .unwrap();
        let msgs = core.messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text(), text);
        assert_eq!(msgs[0].role, "user");
    }

    #[test]
    fn core_client_writers_deserialize_as_mac_message() {
        let core = arbos_core::doc::ChatDoc::new(None, false);
        let plain = "plain text for mac";
        core.transact(|d| {
            d.append_user_message(
                "m1",
                plain,
                "dev:mac",
                &UserMessageOpts {
                    attachments: vec![UserAttachment {
                        name: "a.png".into(),
                        mime: "image/png".into(),
                        kind: Some("image".into()),
                        blob_ref: Some("ref1".into()),
                    }],
                    reply_to: Some("other".into()),
                    ..UserMessageOpts::default()
                },
            )
            .unwrap();
            d.append_system_line("m2", "Call ended · 1 min", "dev:mac", Some("call-9"))
                .unwrap();
            d.append_greeting("m3", "Hi, I'm Arbos.", "dev:mac", "proj", "Arbos")
                .unwrap();
            d.set_reaction("m1", "heart", "user:1", true).unwrap();
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

    #[test]
    fn core_user_row_keys_omit_mac_placeholders() {
        // Core omits chatId / pending / hiddenParts / false defaults that
        // insert_message still serializes from a full Message struct.
        let mac = ChatDoc::empty("chat1");
        let legacy = Message {
            id: "legacy".into(),
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
                text: "hi".into(),
            }],
            hidden_parts: Vec::new(),
            greeting: false,
        };
        mac.insert_message(&legacy).unwrap();
        mac.commit();
        let legacy_row =
            serde_json::to_value(message_map(&mac.messages(), 0).unwrap().get_deep_value())
                .unwrap();

        mac.core.transact(|d| {
            d.append_user_message("core1", "hi", "dev:mac", &UserMessageOpts::default())
                .unwrap();
        });
        let core_row =
            serde_json::to_value(message_map(&mac.messages(), 1).unwrap().get_deep_value())
                .unwrap();

        let legacy_keys = client_row_keys(&legacy_row);
        let core_keys = client_row_keys(&core_row);
        // Shared essentials after dropping Mac placeholders.
        for key in ["id", "role", "sender", "status", "createdAt", "parts"] {
            assert!(core_keys.contains(key), "core missing {key}");
            assert!(legacy_keys.contains(key), "legacy missing {key}");
        }
        assert!(
            core_keys.contains("deviceId"),
            "core always writes deviceId"
        );
        assert!(
            !core_row.as_object().unwrap().contains_key("chatId"),
            "core omits chatId placeholder"
        );
        assert!(
            !core_row.as_object().unwrap().contains_key("pending"),
            "core omits pending placeholder"
        );
        assert!(
            !core_row.as_object().unwrap().contains_key("hiddenParts"),
            "core omits hiddenParts placeholder"
        );
    }

    #[test]
    fn mac_style_send_via_core_transact_is_one_commit() {
        let mac = ChatDoc::empty("chat1");
        let commits = Arc::new(AtomicUsize::new(0));
        let commits_cb = commits.clone();
        let _sub = mac.core.doc.subscribe_local_update(Box::new(move |_| {
            commits_cb.fetch_add(1, Ordering::SeqCst);
            true
        }));
        mac.core.transact(|d| {
            d.append_user_message("m1", "hello", "dev:mac", &UserMessageOpts::default())
                .unwrap();
            d.push_command("send", serde_json::json!({ "messageId": "m1" }), "dev:mac");
            d.push_command(
                "retitle",
                serde_json::json!({ "title": "hello" }),
                "dev:mac",
            );
        });
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert_eq!(mac.len(), 1);
        assert_eq!(mac.core.doc.get_list("commands").len(), 2);
    }
}
