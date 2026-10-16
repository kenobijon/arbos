//! People chats (D72) on the box side. This Project's agent can be a member of chats between
//! people; it joins each one as a peer in the room `people/<chat_id>` (the relay admits the
//! owner's box for it) and answers the humans there.
//!
//! Mention to reply (default): a human message is for this agent when its `mentions` carry
//! `agent:<project>`, or the text says `@<slug>` (the Project's name as a handle), or it replies to
//! one of this agent's messages, or it says `@arbos` and this is the only agent in the chat; the
//! owner can switch the agent to "every message". In a people **group** (`kind: group`, or more
//! than one agent), when nothing addresses this agent, the decision model reads recent chat
//! context and chooses whether this agent should reply (or stay quiet) so the right agent
//! continues the thread — or none do. Each human message is labelled with the author's handle
//! and role (`owner` | `guest`) in the prompt — attribution only; in the permissive v1 a guest
//! gets everything the owner gets. The owner's credits pay; audit lines carry `asked_by`.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{json, Value};

use arbos_core::doc::MessageView;
use arbos_core::ids::now_ms;

use crate::agent::Agent;
use crate::sync::SharedHost;

/// The relay's project slot for people rooms.
pub const PEOPLE_PROJECT: &str = "people";

const RESPOND_SYSTEM: &str = r#"You are the reply gate for one agent in a group chat with people and possibly other agents.
Decide whether THIS agent should reply to the latest human message, using the recent chat context.

Reply when the message is aimed at this agent, continues a thread this agent was just in, asks for help this agent can give, or clearly needs an agent and this one is the best fit among those listed.
Stay quiet when people are talking to each other, the message is for another agent, another agent should continue the thread, or no agent reply is needed.

Output: {"respond":bool,"reason":"one line","confidence":0-1}"#;

/// How many prior messages the reply gate sees (oldest → newest).
const RESPOND_RECENT_MAX: usize = 12;

#[derive(Debug, Clone, Default)]
pub struct PeopleChatInfo {
    pub members: Vec<Value>,
    /// `reply_policy`, `restrict_guests` for this agent in this chat.
    pub settings: Value,
    /// This agent's owner (the Project's user).
    pub owner_user: String,
    /// `@slug` this agent answers to here.
    pub slug: String,
    /// `dm` or `group` from the relay.
    pub kind: String,
}

impl PeopleChatInfo {
    fn agents(&self) -> usize {
        self.members
            .iter()
            .filter(|m| {
                m["member"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("agent:"))
            })
            .count()
    }

    fn is_group(&self) -> bool {
        self.kind == "group" || self.agents() > 1
    }

    fn agent_labels(&self) -> Vec<String> {
        self.members
            .iter()
            .filter_map(|m| {
                let member = m["member"].as_str()?;
                let id = member.strip_prefix("agent:")?;
                let handle = m["handle"].as_str().unwrap_or(id);
                Some(format!("@{handle} ({id})"))
            })
            .collect()
    }

    fn handle_of(&self, user_id: &str) -> String {
        self.members
            .iter()
            .find(|m| m["member"].as_str() == Some(&format!("user:{user_id}")))
            .and_then(|m| m["handle"].as_str())
            .unwrap_or(user_id)
            .to_owned()
    }

    fn owner_handle(&self) -> String {
        self.handle_of(&self.owner_user)
    }
}

/// What a reply in a people chat carries: message fields for the clients.
/// Clients show `inReplyTo` / `askedByHandle` as a reply chip; the prose stays clean.
pub struct ReplyAttribution {
    pub fields: Vec<(&'static str, Value)>,
}

/// Who asked the question the current turn answers.
#[derive(Debug, Clone)]
pub struct Asker {
    pub user_id: String,
    pub handle: String,
    pub role: &'static str,
    pub message_id: String,
}

#[derive(Default)]
pub struct PeopleState {
    chats: Mutex<HashMap<String, PeopleChatInfo>>,
    /// chat → the asker of the turn now running or last run there.
    askers: Mutex<HashMap<String, Asker>>,
    /// The chat whose turn holds the turn lock right now (for audit attribution).
    current: Mutex<Option<String>>,
    /// This Project's owner (from the relay's project record), for guest labels in project chats.
    project_owner: Mutex<String>,
}

impl PeopleState {
    pub fn set_project_owner(&self, user_id: &str) {
        *self.project_owner.lock().expect("people") = user_id.to_owned();
    }

    pub fn project_owner(&self) -> String {
        self.project_owner.lock().expect("people").clone()
    }

    pub fn info(&self, chat_id: &str) -> Option<PeopleChatInfo> {
        self.chats.lock().expect("people").get(chat_id).cloned()
    }

    pub fn is_people_chat(&self, chat_id: &str) -> bool {
        self.chats.lock().expect("people").contains_key(chat_id)
    }

    pub fn asker(&self, chat_id: &str) -> Option<Asker> {
        self.askers.lock().expect("people").get(chat_id).cloned()
    }

    /// `asked_by` for an audit line written during the current people-chat turn.
    pub fn current_asker(&self) -> Option<Asker> {
        let chat = self.current.lock().expect("people").clone()?;
        self.asker(&chat)
    }

    pub fn set_current(&self, chat_id: Option<&str>) {
        *self.current.lock().expect("people") = chat_id.map(str::to_owned);
    }
}

/// Is this human message for the agent `project_id` with the given slug and policy?
pub fn addressed(
    msg: &MessageView,
    project_id: &str,
    slug: &str,
    only_agent: bool,
    every_message: bool,
    replied_to_mine: bool,
) -> bool {
    if msg.role != "user" {
        return false;
    }
    let sender_is_human = msg
        .rest
        .get("sender")
        .and_then(Value::as_str)
        .is_none_or(|s| s.starts_with("user:") || s == "user");
    if !sender_is_human {
        return false;
    }
    if every_message || replied_to_mine {
        return true;
    }
    let me = format!("agent:{project_id}");
    if msg
        .rest
        .get("mentions")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|m| m.as_str() == Some(me.as_str())))
    {
        return true;
    }
    let text = msg.text().to_lowercase();
    let has = |token: &str| {
        let t = format!("@{token}");
        text.match_indices(&t).any(|(i, _)| {
            let after = text[i + t.len()..].chars().next();
            after.is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'))
        })
    };
    (!slug.is_empty() && has(slug)) || (only_agent && has("arbos"))
}

/// Recent user/assistant lines before `up_to` for the reply-gate prompt.
fn recent_people_context(messages: &[MessageView], up_to: &str, max: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        if m.id == up_to {
            break;
        }
        if m.role != "user" && m.role != "assistant" {
            continue;
        }
        let text: String = m.text().chars().take(500).collect();
        if text.trim().is_empty() {
            continue;
        }
        let handle = m
            .rest
            .get("senderHandle")
            .and_then(Value::as_str)
            .unwrap_or(if m.role == "assistant" {
                "agent"
            } else {
                "user"
            });
        out.push(json!({ "role": m.role, "handle": handle, "text": text }));
    }
    if out.len() > max {
        out.drain(0..out.len() - max);
    }
    out
}

impl Agent {
    /// The relay's list of people chats this agent is in (`people_chats` of `/box/projects`, or a
    /// single chat after an `people_agent_added` push): remember members and settings, open the
    /// rooms, and leave chats that are gone.
    pub async fn note_people_chats(self: &std::sync::Arc<Self>, list: &[Value], full: bool) {
        let me = format!("agent:{}", self.cfg.project_id);
        let mut keep = Vec::new();
        for c in list {
            let Some(chat_id) = c.get("id").and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let members = c
                .get("members")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mine = members
                .iter()
                .find(|m| m["member"].as_str() == Some(me.as_str()));
            let info = PeopleChatInfo {
                settings: mine
                    .and_then(|m| m.get("settings"))
                    .cloned()
                    .unwrap_or_else(|| json!({ "reply_policy": "mention" })),
                owner_user: mine
                    .and_then(|m| m["owner"].as_str())
                    .unwrap_or_default()
                    .to_owned(),
                slug: mine
                    .and_then(|m| m["handle"].as_str())
                    .unwrap_or_default()
                    .to_owned(),
                kind: c
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("dm")
                    .to_owned(),
                members,
            };
            let fresh = self
                .people
                .chats
                .lock()
                .expect("people")
                .insert(chat_id.clone(), info)
                .is_none();
            keep.push(chat_id.clone());
            if self.sync.host(&chat_id).await.is_none() {
                let room = arbos_core::wire::room_name(PEOPLE_PROJECT, &chat_id);
                match self
                    .sync
                    .open_room_named(&chat_id, &room, PEOPLE_PROJECT)
                    .await
                {
                    Ok(_) => tracing::info!(chat = %chat_id, "joined a people chat"),
                    Err(e) => tracing::warn!(chat = %chat_id, error = %e, "people room"),
                }
            }
            if fresh {
                self.append_audit(
                    "people_agent_joined",
                    json!({ "chat_id": chat_id, "members": self.people.info(&chat_id).map(|i| i.members.len()).unwrap_or(0) }),
                );
            }
        }
        if full {
            let gone: Vec<String> = self
                .people
                .chats
                .lock()
                .expect("people")
                .keys()
                .filter(|k| !keep.contains(k))
                .cloned()
                .collect();
            for chat_id in gone {
                self.people_agent_removed(&chat_id).await;
            }
        }
    }

    /// Fetch one people chat from the relay (after a `people_agent_added` push) and join it.
    pub async fn join_people_chat(self: &std::sync::Arc<Self>, chat_id: &str) -> Result<()> {
        let r = self
            .http_client()
            .get(format!("{}/box/people/{chat_id}", self.cfg.relay_url))
            .bearer_auth(&self.cfg.box_token)
            .send()
            .await?;
        if !r.status().is_success() {
            anyhow::bail!("people chat {chat_id}: HTTP {}", r.status());
        }
        let v: Value = r.json().await?;
        self.note_people_chats(std::slice::from_ref(&v), false)
            .await;
        Ok(())
    }

    /// The owner removed this agent (or the chat was deleted): stop any turn there, leave the
    /// room, soft-invalidate what the agent remembered from the chat, keep the audit.
    pub async fn people_agent_removed(self: &std::sync::Arc<Self>, chat_id: &str) {
        let known = self
            .people
            .chats
            .lock()
            .expect("people")
            .remove(chat_id)
            .is_some();
        if !known {
            return;
        }
        let _ = self.handle_people_stop(chat_id).await;
        self.sync.close_room(chat_id).await;
        let invalidated = self
            .store
            .memory_invalidate_thread(chat_id, now_ms())
            .unwrap_or(0);
        self.people.askers.lock().expect("people").remove(chat_id);
        self.append_audit(
            "people_agent_removed",
            json!({ "chat_id": chat_id, "memory_invalidated": invalidated }),
        );
        tracing::info!(chat = %chat_id, invalidated, "left a people chat");
    }

    /// Should this `send` command drive a turn here? People chats route by mention; a message
    /// not for this agent is left alone (its command stays pending for whoever it is for). In a
    /// group, an unaddressed message still asks the decision model — with recent chat context —
    /// whether this agent should reply.
    pub async fn people_wants(&self, chat_id: &str, host: &SharedHost, user_msg_id: &str) -> bool {
        let Some(info) = self.people.info(chat_id) else {
            return true;
        };
        let (msg, replied_to_mine, recent) = {
            let h = host.lock().await;
            let Some(msg) = h.doc.message(user_msg_id) else {
                return false;
            };
            let me = format!("agent:{}", self.cfg.project_id);
            let replied_to_mine = msg
                .rest
                .get("replyTo")
                .and_then(Value::as_str)
                .and_then(|id| h.doc.message(id))
                .is_some_and(|m| {
                    m.rest.get("sender").and_then(Value::as_str) == Some(me.as_str())
                        || (m.role == "assistant"
                            && m.rest.get("deviceId").and_then(Value::as_str)
                                == Some(self.cfg.device_id.as_str()))
                });
            let recent = recent_people_context(&h.doc.messages(), user_msg_id, RESPOND_RECENT_MAX);
            (msg, replied_to_mine, recent)
        };
        let every = info.settings["reply_policy"].as_str() == Some("every_message");
        if addressed(
            &msg,
            &self.cfg.project_id,
            &info.slug,
            info.agents() == 1,
            every,
            replied_to_mine,
        ) {
            return true;
        }
        if !info.is_group() {
            return false;
        }
        self.people_decide_respond(chat_id, &info, &msg, &recent)
            .await
    }

    /// Decision-model gate for an unaddressed human message in a people group.
    /// Uses recent transcript so the right agent continues a thread (or stays quiet).
    async fn people_decide_respond(
        &self,
        chat_id: &str,
        info: &PeopleChatInfo,
        msg: &MessageView,
        recent: &[Value],
    ) -> bool {
        let text: String = msg.text().chars().take(4000).collect();
        if text.trim().is_empty() {
            return false;
        }
        let asker = msg
            .rest
            .get("senderHandle")
            .and_then(Value::as_str)
            .unwrap_or("someone");
        let user = json!({
            "chat_id": chat_id,
            "kind": info.kind,
            "this_agent": { "project_id": self.cfg.project_id, "slug": info.slug },
            "agents_here": info.agent_labels(),
            "recent": recent,
            "from": asker,
            "text": text,
        })
        .to_string();
        let model = self.settings.models.read().await.decision.id.clone();
        let (v, ms) = match self
            .llm
            .complete_json(&model, RESPOND_SYSTEM, &user, 400)
            .await
        {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(chat = %chat_id, error = %e, "people: respond decision failed");
                let _ = self.store.log_decision(
                    "respond",
                    Some(&msg.id),
                    Some(&json!({ "error": e.to_string() })),
                    Some("SKIP"),
                    &[],
                    Some("decision model failed"),
                    None,
                    Some(&model),
                    None,
                );
                return false;
            }
        };
        let respond = v.get("respond").and_then(Value::as_bool).unwrap_or(false);
        let reason = v.get("reason").and_then(Value::as_str);
        let confidence = v.get("confidence").and_then(Value::as_f64);
        let _ = self.store.log_decision(
            "respond",
            Some(&msg.id),
            Some(&v),
            Some(if respond { "REPLY" } else { "SKIP" }),
            &[],
            reason,
            confidence,
            Some(&model),
            Some(ms),
        );
        tracing::info!(
            chat = %chat_id,
            respond,
            reason = reason.unwrap_or(""),
            "people: respond decision"
        );
        respond
    }

    /// Prelude for the prompt and the asker for attribution, when `chat_id` is a people chat.
    pub async fn people_turn_context(
        &self,
        chat_id: &str,
        host: &SharedHost,
        user_msg_id: &str,
    ) -> Option<String> {
        let info = self.people.info(chat_id)?;
        let msg = host.lock().await.doc.message(user_msg_id)?;
        let sender = msg
            .rest
            .get("sender")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_owned();
        let user_id = sender
            .strip_prefix("user:")
            .unwrap_or(&info.owner_user)
            .to_owned();
        let handle = msg
            .rest
            .get("senderHandle")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| info.handle_of(&user_id));
        let role = if user_id == info.owner_user {
            "owner"
        } else {
            "guest"
        };
        let asker = Asker {
            user_id,
            handle: handle.clone(),
            role,
            message_id: user_msg_id.to_owned(),
        };
        self.people
            .askers
            .lock()
            .expect("people")
            .insert(chat_id.to_owned(), asker);
        self.people.set_current(Some(chat_id));
        let people: Vec<String> = info
            .members
            .iter()
            .filter(|m| m["member"].as_str().is_some_and(|s| s.starts_with("user:")))
            .map(|m| {
                let h = m["handle"].as_str().unwrap_or("?");
                if m["member"].as_str() == Some(&format!("user:{}", info.owner_user)) {
                    format!("@{h} (owner, your Project's user)")
                } else {
                    format!("@{h} (guest)")
                }
            })
            .collect();
        let me_agent = format!("agent:{}", self.cfg.project_id);
        let other_agents: Vec<String> = info
            .members
            .iter()
            .filter(|m| {
                m["member"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("agent:") && s != me_agent)
            })
            .map(|m| {
                let h = m["handle"].as_str().unwrap_or("?");
                match m["description"]
                    .as_str()
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                {
                    Some(d) => {
                        let short: String = d.chars().take(160).collect();
                        format!("@{h} ({short})")
                    }
                    None => format!("@{h}"),
                }
            })
            .collect();
        let others_clause = if other_agents.is_empty() {
            String::new()
        } else {
            format!(
                "; other agents present (they answer when @mentioned or when the reply gate picks them): {}",
                other_agents.join(", ")
            )
        };
        Some(format!(
            "[People chat with {}. You are @{} (the agent of {}'s Project \"{}\"){}. This message is from @{} ({role}); treat it as a request from another person in the conversation and address them by handle. Your reply is read by everyone in the chat.]\n\n",
            people.join(", "),
            info.slug,
            info.owner_handle(),
            self.cfg.project_id,
            others_clause,
            handle,
        ))
    }

    /// Projects together (D72 step 3): in this Project's own chats, a message written by a guest
    /// the owner invited is labelled so the agent knows who is talking. `None` for the owner's
    /// own messages and for people chats (which have their own prelude).
    pub async fn shared_project_guest_note(
        &self,
        chat_id: &str,
        host: &SharedHost,
        user_msg_id: &str,
    ) -> Option<String> {
        if self.people.is_people_chat(chat_id) {
            return None;
        }
        let msg = host.lock().await.doc.message(user_msg_id)?;
        let sender = msg.rest.get("sender").and_then(Value::as_str)?;
        let user_id = sender.strip_prefix("user:")?;
        let owner = self.people.project_owner();
        if owner.is_empty() || user_id == owner {
            return None;
        }
        let handle = msg
            .rest
            .get("senderHandle")
            .and_then(Value::as_str)
            .unwrap_or(user_id);
        self.people.askers.lock().expect("people").insert(
            chat_id.to_owned(),
            Asker {
                user_id: user_id.to_owned(),
                handle: handle.to_owned(),
                role: "guest",
                message_id: user_msg_id.to_owned(),
            },
        );
        self.people.set_current(Some(chat_id));
        Some(format!(
            "[This message is from @{handle}, a person the owner of this Project invited to work in it with you. Address them by handle; they have the same say here as the owner.]\n\n"
        ))
    }

    /// Fields on a reply in a people chat (called right after the assistant message starts).
    pub fn people_reply_fields(&self, chat_id: &str) -> Option<ReplyAttribution> {
        let info = self.people.info(chat_id)?;
        let asker = self.people.asker(chat_id)?;
        let mine = info
            .members
            .iter()
            .find(|m| m["member"].as_str() == Some(&format!("agent:{}", self.cfg.project_id)));
        let avatar = mine.and_then(|m| m["avatar"].as_str()).map(str::to_owned);
        let avatar_color = mine
            .and_then(|m| m["avatar_color"].as_str())
            .map(str::to_owned);
        let fields = vec![
            ("sender", json!(format!("agent:{}", self.cfg.project_id))),
            ("senderHandle", json!(info.slug)),
            ("senderAvatar", json!(avatar)),
            ("senderAvatarColor", json!(avatar_color)),
            ("ownerHandle", json!(info.owner_handle())),
            ("askedBy", json!(format!("user:{}", asker.user_id))),
            ("askedByHandle", json!(asker.handle)),
            ("inReplyTo", json!(asker.message_id)),
        ];
        Some(ReplyAttribution { fields })
    }

    /// The agent was removed while answering here: abort that turn.
    async fn handle_people_stop(self: &std::sync::Arc<Self>, chat_id: &str) -> Result<()> {
        let running = self
            .turn
            .lock()
            .await
            .as_ref()
            .is_some_and(|t| t.chat_id == chat_id);
        if running {
            self.stop_turn(chat_id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Mention routing: which human messages a shared agent answers.

    use serde_json::json;

    use super::*;

    fn msg(text: &str, extra: Value) -> MessageView {
        let mut rest = json!({ "sender": "user:gh_2", "senderHandle": "sam" });
        if let (Some(r), Some(e)) = (rest.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                r.insert(k.clone(), v.clone());
            }
        }
        MessageView {
            id: "m1".into(),
            role: "user".into(),
            status: "done".into(),
            parts: vec![json!({ "kind": "text", "text": text })],
            rest,
        }
    }

    const P: &str = "01HPROJ";

    #[test]
    fn mention_routing() {
        // Resolved mention from the composer.
        assert!(addressed(
            &msg("hey", json!({ "mentions": ["agent:01HPROJ"] })),
            P,
            "research",
            false,
            false,
            false
        ));
        assert!(!addressed(
            &msg("hey", json!({ "mentions": ["agent:OTHER"] })),
            P,
            "research",
            false,
            false,
            false
        ));
        // Text mention by slug, with word boundary.
        assert!(addressed(
            &msg("@research what do you think?", json!({})),
            P,
            "research",
            false,
            false,
            false
        ));
        assert!(addressed(
            &msg("ping @Research!", json!({})),
            P,
            "research",
            false,
            false,
            false
        ));
        assert!(!addressed(
            &msg("@research-2 is another one", json!({})),
            P,
            "research",
            false,
            false,
            false
        ));
        assert!(!addressed(
            &msg("no mention here", json!({})),
            P,
            "research",
            false,
            false,
            false
        ));
        // @arbos only when this is the only agent in the chat.
        assert!(addressed(
            &msg("@arbos help", json!({})),
            P,
            "research",
            true,
            false,
            false
        ));
        assert!(!addressed(
            &msg("@arbos help", json!({})),
            P,
            "research",
            false,
            false,
            false
        ));
        // Reply-to one of my messages, or the every-message policy.
        assert!(addressed(
            &msg("and this?", json!({})),
            P,
            "research",
            false,
            false,
            true
        ));
        assert!(addressed(
            &msg("anything", json!({})),
            P,
            "research",
            false,
            true,
            false
        ));
        // Agents' and other roles' messages never drive a turn.
        let mut from_agent = msg("@research", json!({}));
        from_agent.rest["sender"] = json!("agent:OTHER");
        assert!(!addressed(&from_agent, P, "research", true, true, true));
        let mut assistant = msg("@research", json!({}));
        assistant.role = "assistant".into();
        assert!(!addressed(&assistant, P, "research", true, true, true));
    }

    #[test]
    fn recent_context_stops_at_current_and_caps() {
        let mut lines = Vec::new();
        for i in 0..15 {
            let mut m = msg(&format!("line {i}"), json!({}));
            m.id = format!("m{i}");
            m.rest["senderHandle"] = json!(if i % 2 == 0 { "sam" } else { "bob" });
            if i % 2 == 1 {
                m.role = "assistant".into();
            }
            lines.push(m);
        }
        // Context for "m12": lines m0…m11, capped to the last 5.
        let ctx = recent_people_context(&lines, "m12", 5);
        assert_eq!(ctx.len(), 5);
        assert_eq!(ctx[0]["text"], "line 7");
        assert_eq!(ctx[4]["text"], "line 11");
        assert_eq!(ctx[4]["handle"], "bob");
        // Unknown id → everything, still capped.
        let all = recent_people_context(&lines, "missing", 3);
        assert_eq!(all.len(), 3);
        assert_eq!(all[2]["text"], "line 14");
    }
}
