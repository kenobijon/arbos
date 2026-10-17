//! Voice link (Calls Phase 2, D75): the box keeps one WebSocket to the relay (`WS /box/voice`)
//! for every workspace it serves. The relay forwards what the voice worker needs from the agent:
//!
//! * `voice_context` — the context pack for a call: the voice system prompt (instructions,
//!   project memory, working set), the chat's recent lines, the people on the call;
//! * `voice_tool` — a spoken request to do work or look something up. It runs as a normal turn
//!   of the pi main session in the chat the call belongs to (same tools, same D64/D66 limits):
//!   the request lands as a user row marked `voice: true`, the usual ack / tool cards / result
//!   rows follow, and the result text goes back to be spoken;
//! * `voice_rows` — transcript lines (what people said, what the agent said) and system lines
//!   ("Arbos joined the call") written into the chat doc as finished rows, never as turns.
//!
//! Frames are JSON text; every request carries an `id` answered by `{type: "result", id, …}`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use arbos_core::doc::CommandView;
use arbos_core::ids::{now_ms, ulid};

use crate::agent::Agent;
use crate::workspace::Supervisor;

/// How many recent chat lines the worker gets on join.
const RECENT_LINES: usize = 12;
const RECENT_LINE_CHARS: usize = 400;
/// Project memory in the voice prompt is clipped: the model hears, it does not read.
const MEMORY_CHARS: usize = 3000;
const INSTRUCTIONS_CHARS: usize = 4000;
/// A turn that handed its work to a subagent: how long the caller waits for the outcome row.
const SUBAGENT_WAIT: std::time::Duration = std::time::Duration::from_secs(150);

/// Keep the link up for the life of the box (reconnect with backoff).
pub fn spawn_voice_link(sup: Arc<Supervisor>) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = std::time::Instant::now();
            match session(&sup).await {
                Ok(()) => tracing::info!("voice link closed"),
                Err(e) => tracing::warn!(error = %e, "voice link failed"),
            }
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(1);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
}

async fn session(sup: &Arc<Supervisor>) -> Result<()> {
    let url = format!(
        "{}/box/voice?token={}",
        sup.cfg.relay_url.replacen("http", "ws", 1),
        sup.cfg.box_token
    );
    let (ws, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .map_err(|_| anyhow!("dial timeout"))??;
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    out_tx
        .send(json!({ "type": "hello", "proto": arbos_core::PROTO_VERSION }).to_string())
        .await
        .ok();
    tracing::info!("voice link up");
    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(msg) = incoming else { return Ok(()) };
                let text = match msg? {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => return Ok(()),
                    Message::Ping(p) => {
                        sink.send(Message::Pong(p)).await?;
                        continue;
                    }
                    _ => continue,
                };
                let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                let sup = sup.clone();
                let out = out_tx.clone();
                tokio::spawn(async move { handle_frame(sup, frame, out).await });
            }
            reply = out_rx.recv() => {
                let Some(text) = reply else { return Ok(()) };
                sink.send(Message::Text(text.into())).await?;
            }
        }
    }
}

async fn handle_frame(sup: Arc<Supervisor>, frame: Value, out: mpsc::Sender<String>) {
    let kind = frame["type"].as_str().unwrap_or_default().to_owned();
    let id = frame["id"].as_str().unwrap_or_default().to_owned();
    let project_id = frame["project_id"].as_str().unwrap_or_default();
    let chat_id = frame["chat_id"].as_str().unwrap_or_default().to_owned();
    let call_id = frame["call_id"].as_str().unwrap_or_default().to_owned();
    let reply = |v: Value| {
        let out = out.clone();
        async move {
            let _ = out.send(v.to_string()).await;
        }
    };
    match kind.as_str() {
        "ping" => reply(json!({ "type": "pong" })).await,
        "voice_context" | "voice_tool" | "voice_rows" => {
            let Some(ws) = sup.get(project_id).await else {
                if !id.is_empty() {
                    reply(json!({ "type": "result", "id": id, "ok": false, "error": format!("no workspace for project {project_id}") })).await;
                }
                return;
            };
            let agent = ws.agent.clone();
            match kind.as_str() {
                "voice_context" => {
                    let ctx = agent.voice_context(&chat_id, &frame["humans"]).await;
                    reply(json!({ "type": "result", "id": id, "ok": true, "context": ctx })).await;
                }
                "voice_tool" => {
                    let request = frame["request"].as_str().unwrap_or_default().to_owned();
                    let t0 = std::time::Instant::now();
                    match agent
                        .voice_request(&chat_id, &frame["speaker"], &request, &call_id)
                        .await
                    {
                        Ok((text, message_id)) => {
                            tracing::info!(call = %call_id, chat = %chat_id, ms = t0.elapsed().as_millis() as i64, "voice request done");
                            reply(json!({ "type": "result", "id": id, "ok": true, "text": text, "message_id": message_id })).await;
                        }
                        Err(e) => {
                            tracing::warn!(call = %call_id, chat = %chat_id, error = %e, "voice request failed");
                            reply(json!({ "type": "result", "id": id, "ok": false, "error": e.to_string() })).await;
                        }
                    }
                }
                _ => {
                    let rows = frame["rows"].as_array().cloned().unwrap_or_default();
                    agent.voice_rows(&chat_id, &call_id, &rows).await;
                }
            }
        }
        other => tracing::debug!(kind = other, "voice link: unknown frame"),
    }
}

fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_owned();
    }
    let cut: String = t.chars().take(max).collect();
    format!("{cut}…")
}

/// `@handle` or the name of a `sender{}`-shaped speaker.
fn speaker_label(speaker: &Value) -> String {
    let handle = speaker["handle"].as_str().unwrap_or_default();
    if !handle.is_empty() {
        return format!("@{handle}");
    }
    speaker["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "someone on the call".to_owned())
}

impl Agent {
    /// The context pack for a call in `chat_id`: the voice system prompt and recent lines.
    pub async fn voice_context(&self, chat_id: &str, humans: &Value) -> Value {
        let st = self.settings.state.read().await.clone();
        let mut system = String::new();
        if !st.instructions.trim().is_empty() {
            system.push_str("## Owner instructions\n");
            system.push_str(&clip(&st.instructions, INSTRUCTIONS_CHARS));
            system.push_str("\n\n");
        }
        let memory = self.memory.core_memory_text();
        if !memory.trim().is_empty() {
            system.push_str("## What you remember\n");
            system.push_str(&clip(&memory, MEMORY_CHARS));
            system.push_str("\n\n");
        }
        let ws = self.working_set_text(chat_id);
        if !ws.trim().is_empty() {
            system.push_str("## Working set of this chat\n");
            system.push_str(&clip(&ws, MEMORY_CHARS));
            system.push_str("\n\n");
        }
        if let Some(list) = humans.as_array() {
            if !list.is_empty() {
                system.push_str("## People on the call\n");
                for h in list {
                    let role = h["role"].as_str().unwrap_or("member");
                    let name = h["name"].as_str().unwrap_or_default();
                    system.push_str(&format!("- {} — {name} ({role})\n", speaker_label(h)));
                }
            }
        }
        let recent = match self.sync.host(chat_id).await {
            Some(host) => {
                let h = host.lock().await;
                let msgs = h.doc.messages();
                let mut out: Vec<Value> = msgs
                    .iter()
                    .filter(|m| m.role == "user" || m.role == "assistant")
                    .filter(|m| m.status == "done")
                    .map(|m| {
                        let text = clip(&m.text(), RECENT_LINE_CHARS);
                        let handle = m.rest.get("senderHandle").and_then(Value::as_str);
                        let from = match (m.role.as_str(), handle) {
                            ("assistant", _) => format!("{} (you)", st.name),
                            ("user", Some(h)) => format!("@{h}"),
                            _ => "the user".to_owned(),
                        };
                        json!({ "from": from, "text": text, "voice": m.rest.get("voice").and_then(Value::as_bool).unwrap_or(false) })
                    })
                    .filter(|v| !v["text"].as_str().unwrap_or_default().is_empty())
                    .collect();
                if out.len() > RECENT_LINES {
                    out.drain(0..out.len() - RECENT_LINES);
                }
                out
            }
            None => Vec::new(),
        };
        json!({
            "name": st.name,
            "system": system,
            "recent": recent,
            "talking_model": self.settings.models.read().await.talking.id,
        })
    }

    /// A spoken request becomes a turn in `chat_id`: user row (`voice: true`, addressed to this
    /// agent), the normal turn, and the result row's text back. Blocks until the turn ends.
    pub async fn voice_request(
        self: &Arc<Self>,
        chat_id: &str,
        speaker: &Value,
        request: &str,
        call_id: &str,
    ) -> Result<(String, String)> {
        if request.trim().is_empty() {
            return Err(anyhow!("empty request"));
        }
        let host = match self.sync.host(chat_id).await {
            Some(h) => h,
            None => self.sync.open_room(chat_id).await?,
        };
        let people = self.people.is_people_chat(chat_id);
        let speaker_id = speaker["id"].as_str().unwrap_or_default();
        let handle = speaker["handle"].as_str().unwrap_or_default();
        let name = speaker["name"].as_str().unwrap_or_default();
        let sender = if people && !speaker_id.is_empty() {
            format!("user:{speaker_id}")
        } else {
            "user".to_owned()
        };
        let mut extra: Vec<(&str, Value)> = vec![
            ("voice", json!(true)),
            ("source", json!("voice")),
            ("callId", json!(call_id)),
            (
                "mentions",
                json!([format!("agent:{}", self.cfg.project_id)]),
            ),
        ];
        if !handle.is_empty() {
            extra.push(("senderHandle", json!(handle)));
        }
        if !name.is_empty() {
            extra.push(("senderName", json!(name)));
        }
        if let Some(role) = speaker["role"].as_str() {
            extra.push(("senderRole", json!(role)));
        }
        let msg_id = ulid();
        let cmd = {
            let mut h = host.lock().await;
            let cmd_id = h.doc.append_user_message_from(
                &msg_id,
                request.trim(),
                &self.cfg.device_id,
                &sender,
                &extra,
            );
            h.mark_seen(&cmd_id);
            CommandView {
                id: cmd_id,
                kind: "send".into(),
                payload: json!({ "messageId": msg_id }),
                issued_by: self.cfg.device_id.clone(),
                issued_at: now_ms(),
                status: "pending".into(),
            }
        };
        tracing::info!(call = %call_id, chat = %chat_id, from = %speaker_label(speaker), "voice request → turn");
        let outcome = self.handle_command(chat_id, cmd).await;
        let pick = |h: &crate::sync::DocHost| -> Option<arbos_core::doc::MessageView> {
            let replies: Vec<_> = h
                .doc
                .messages()
                .into_iter()
                .filter(|m| {
                    m.role == "assistant"
                        && m.rest.get("inReplyTo").and_then(Value::as_str) == Some(msg_id.as_str())
                        && m.rest.get("voice").and_then(Value::as_bool) != Some(true)
                })
                .collect();
            // The result row is the last one; the ack is spoken live by the worker already.
            replies
                .iter()
                .rev()
                .find(|m| m.rest.get("turnRole").and_then(Value::as_str) != Some("ack"))
                .or(replies.last())
                .cloned()
        };
        let mut result = pick(&*host.lock().await);
        // A turn that handed the work to a subagent ends after its ack; the outcome lands later
        // as a separate assistant row. Wait for it (bounded) so the caller hears the answer.
        if outcome.is_ok()
            && result
                .as_ref()
                .is_some_and(|m| m.rest.get("turnRole").and_then(Value::as_str) == Some("ack"))
        {
            let ack_id = result.as_ref().map(|m| m.id.clone()).unwrap_or_default();
            let deadline = std::time::Instant::now() + SUBAGENT_WAIT;
            while std::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let h = host.lock().await;
                let msgs = h.doc.messages();
                let after_ack = msgs
                    .iter()
                    .position(|m| m.id == ack_id)
                    .map(|i| &msgs[i + 1..])
                    .unwrap_or(&[]);
                if let Some(m) = after_ack.iter().rev().find(|m| {
                    m.role == "assistant"
                        && m.status == "done"
                        && m.rest.get("voice").and_then(Value::as_bool) != Some(true)
                        && !m.text().trim().is_empty()
                }) {
                    result = Some(m.clone());
                    break;
                }
            }
        }
        match (outcome, result) {
            (Ok(()), Some(m)) if m.status == "done" => Ok((m.text(), m.id.clone())),
            (Ok(()), Some(m)) if m.status == "error" => Err(anyhow!(
                "the agent's turn failed: {}",
                m.parts
                    .iter()
                    .find_map(|p| p.get("text").and_then(Value::as_str))
                    .unwrap_or("unknown error")
            )),
            (Ok(()), Some(m)) => Ok((m.text(), m.id.clone())),
            (Ok(()), None) => Err(anyhow!("the agent wrote no reply")),
            (Err(e), _) => Err(e),
        }
    }

    /// Transcript and system lines of a call → finished rows in the chat doc (no turn).
    pub async fn voice_rows(&self, chat_id: &str, call_id: &str, rows: &[Value]) {
        let host = match self.sync.host(chat_id).await {
            Some(h) => h,
            None => match self.sync.open_room(chat_id).await {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(chat = %chat_id, error = %e, "voice rows: no room");
                    return;
                }
            },
        };
        let people = self.people.is_people_chat(chat_id);
        let me = format!("agent:{}", self.cfg.project_id);
        let my_slug = self.people.info(chat_id).map(|i| i.slug);
        let h = host.lock().await;
        // The agent's spoken lines answer the latest human line of the call.
        let mut last_user_line: Option<String> = h
            .doc
            .messages()
            .iter()
            .rev()
            .find(|m| {
                m.role == "user" && m.rest.get("callId").and_then(Value::as_str) == Some(call_id)
            })
            .map(|m| m.id.clone());
        for row in rows {
            let text = row["text"].as_str().unwrap_or_default().trim();
            let kind = row["kind"].as_str().unwrap_or("transcript");
            let id = ulid();
            if kind == "system" {
                if !text.is_empty() {
                    h.doc
                        .append_system_note(&id, &me, text, &self.cfg.device_id);
                }
                continue;
            }
            if text.is_empty() {
                continue;
            }
            let speaker = &row["speaker"];
            let is_agent = speaker["kind"].as_str() == Some("agent") || speaker.is_null();
            let mut extra: Vec<(&str, Value)> = vec![
                ("voice", json!(true)),
                ("source", json!("voice")),
                ("callId", json!(call_id)),
            ];
            if let Some(at) = row["at"].as_i64() {
                extra.push(("spokenAt", json!(at)));
            }
            if row["truncated"].as_bool().unwrap_or(false) {
                extra.push(("truncated", json!(true)));
            }
            if is_agent {
                if let Some(reply_to) = &last_user_line {
                    extra.push(("inReplyTo", json!(reply_to)));
                }
                extra.push(("turnRole", json!("result")));
                if people {
                    extra.push(("senderHandle", json!(my_slug.clone().unwrap_or_default())));
                }
                let sender = if people {
                    me.clone()
                } else {
                    "agent".to_owned()
                };
                h.doc.append_voice_line(
                    "assistant",
                    &id,
                    text,
                    &self.cfg.device_id,
                    &sender,
                    &extra,
                );
            } else {
                let speaker_id = speaker["id"].as_str().unwrap_or_default();
                let handle = speaker["handle"].as_str().unwrap_or_default();
                let name = speaker["name"].as_str().unwrap_or_default();
                let sender = if people && !speaker_id.is_empty() {
                    format!("user:{speaker_id}")
                } else {
                    "user".to_owned()
                };
                if !handle.is_empty() {
                    extra.push(("senderHandle", json!(handle)));
                }
                if !name.is_empty() {
                    extra.push(("senderName", json!(name)));
                }
                h.doc
                    .append_voice_line("user", &id, text, &self.cfg.device_id, &sender, &extra);
                last_user_line = Some(id.clone());
            }
            let _ = self.store.t3_append(
                chat_id,
                None,
                Some(chat_id),
                if is_agent { "assistant" } else { "user" },
                Some(if is_agent { "agent" } else { "user" }),
                text,
                Some(&json!({ "message_id": id, "voice": true, "call_id": call_id })),
            );
        }
    }
}
