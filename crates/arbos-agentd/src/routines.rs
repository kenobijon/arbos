//! Relay pushes delivered to the box (`GET /projects/{id}/pushes`), and the `routine_run` push
//! (F3): the prompt lands in the chosen chat as a user message with `sender = routine:<id>`, runs
//! as a normal turn, and the run status goes back to the relay.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use arbos_core::doc::CommandView;
use arbos_core::ids::{now_ms, ulid};

use crate::agent::Agent;

const PUSH_POLL: Duration = Duration::from_secs(5);

impl Agent {
    /// Dispatch one push by kind. Unknown kinds are logged into the T3 store as before so
    /// nothing is lost while other features add their handlers.
    pub async fn handle_push(self: &Arc<Self>, kind: &str, payload: Value) {
        match kind {
            "routine_run" => {
                if let Err(e) = self.handle_routine_run(payload).await {
                    tracing::warn!(error = %e, "routine_run push failed");
                }
            }
            // F5: the library changed on the relay; refetch the index now rather than at the
            // next 60 s tick.
            "skills_changed" => self.refresh_skills().await,
            "channel_message" => {
                if let Err(e) = self.handle_channel_message(payload).await {
                    tracing::warn!(error = %e, "channel_message push failed");
                }
            }
            other => {
                tracing::info!(kind = other, push = %payload, "push delivered");
                let _ = self.store.t3_append(
                    "projects",
                    None,
                    None,
                    "push",
                    Some("system"),
                    &json!({ "kind": other, "payload": payload }).to_string(),
                    None,
                );
            }
        }
    }

    /// Poll the relay push queue. Pushes arriving while the box is up (cross-project messages,
    /// routine runs, channel messages) come this way; the ones queued while it slept come with
    /// `box/hello`.
    pub fn spawn_push_poller(self: &Arc<Self>) {
        let agent = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PUSH_POLL).await;
                let resp = agent
                    .http_client()
                    .get(format!(
                        "{}/projects/{}/pushes",
                        agent.cfg.relay_url, agent.cfg.project_id
                    ))
                    .bearer_auth(&agent.cfg.box_token)
                    .send()
                    .await;
                let Ok(r) = resp else { continue };
                if !r.status().is_success() {
                    continue;
                }
                let Ok(v) = r.json::<Value>().await else {
                    continue;
                };
                for p in v.as_array().cloned().unwrap_or_default() {
                    agent.dispatch_push(p);
                }
            }
        });
    }

    /// Each push runs on its own task; `send`-like work still serialises on the turn lock.
    pub fn dispatch_push(self: &Arc<Self>, push: Value) {
        let kind = push
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let payload = push.get("payload").cloned().unwrap_or(Value::Null);
        let agent = self.clone();
        tokio::spawn(async move { agent.handle_push(&kind, payload).await });
    }

    async fn report_run(
        &self,
        routine_id: &str,
        run_id: &str,
        status: &str,
        message_id: Option<&str>,
        note: Option<&str>,
    ) -> Result<()> {
        let r = self
            .http_client()
            .post(format!(
                "{}/projects/{}/routines/{}/runs/{}",
                self.cfg.relay_url, self.cfg.project_id, routine_id, run_id
            ))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "status": status, "messageId": message_id, "note": note }))
            .send()
            .await?;
        if !r.status().is_success() {
            return Err(anyhow!("run report: HTTP {}", r.status()));
        }
        Ok(())
    }

    /// `routine_run { routineId, runId, chatId, prompt, routineName, cause, clientsConnected }`.
    async fn handle_routine_run(self: &Arc<Self>, payload: Value) -> Result<()> {
        let field = |k: &str| {
            payload
                .get(k)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("routine_run push missing {k}"))
        };
        let routine_id = field("routineId")?;
        let run_id = field("runId")?;
        let chat_id = field("chatId")?;
        let prompt = field("prompt")?;
        let name = payload
            .get("routineName")
            .and_then(Value::as_str)
            .unwrap_or("Routine")
            .to_owned();
        let clients_connected = payload
            .get("clientsConnected")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let sender = format!("routine:{routine_id}");
        tracing::info!(routine = %routine_id, run = %run_id, chat = %chat_id, "routine run");

        if let Err(e) = self
            .report_run(&routine_id, &run_id, "running", None, None)
            .await
        {
            tracing::warn!(error = %e, "routine run: report running");
        }

        // The chat may have been created moments ago by the client; the registry poll opens
        // rooms every 3 s, so open it here when it is not hosted yet.
        let host = match self.sync.host(&chat_id).await {
            Some(h) => h,
            None => self.sync.open_room(&chat_id).await?,
        };
        let msg_id = ulid();
        let cmd = {
            let mut h = host.lock().await;
            let cmd_id = h.doc.append_user_message_from(
                &msg_id,
                &prompt,
                &self.cfg.device_id,
                &sender,
                &[
                    ("routineName", json!(name)),
                    ("routineRunId", json!(run_id)),
                ],
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
        let outcome = self.handle_command(&chat_id, cmd).await;

        // Outcome: the turn result plus the assistant reply to our message.
        let (status, reply_id, note) = {
            let h = host.lock().await;
            let reply = h.doc.messages().into_iter().rev().find(|m| {
                m.role == "assistant"
                    && m.rest.get("inReplyTo").and_then(Value::as_str) == Some(msg_id.as_str())
            });
            match (outcome, reply) {
                (Ok(()), Some(m)) if m.status == "done" => {
                    let excerpt: String = m.text().split_whitespace().collect::<Vec<_>>().join(" ");
                    (
                        "done",
                        Some(m.id),
                        Some(excerpt.chars().take(160).collect::<String>()),
                    )
                }
                (Ok(()), Some(m)) => (
                    "failed",
                    Some(m.id),
                    Some(format!("reply ended with status {}", m.status)),
                ),
                (Ok(()), None) => ("failed", None, Some("no reply was written".to_owned())),
                (Err(e), m) => ("failed", m.map(|m| m.id), Some(e.to_string())),
            }
        };
        if let Err(e) = self
            .report_run(
                &routine_id,
                &run_id,
                status,
                reply_id.as_deref(),
                note.as_deref(),
            )
            .await
        {
            tracing::warn!(error = %e, "routine run: report outcome");
        }

        // Nobody was watching: leave a one-line notice in the pinned tab (D49 picks it up).
        if !clients_connected {
            self.notify_pinned_tab(&chat_id, &sender, &name, status)
                .await;
        }
        Ok(())
    }

    async fn notify_pinned_tab(&self, chat_id: &str, sender: &str, name: &str, status: &str) {
        let chats = self.store.chats().unwrap_or_default();
        let Some(pinned) = chats.iter().find(|c| c.is_pinned) else {
            return;
        };
        let chat_title = chats
            .iter()
            .find(|c| c.id == chat_id)
            .map(|c| c.title.clone())
            .unwrap_or_else(|| "its chat".to_owned());
        let text = if pinned.id == chat_id {
            format!("Routine “{name}” ran while you were away ({status}).")
        } else {
            format!("Routine “{name}” ran while you were away ({status}) → open “{chat_title}”.")
        };
        if let Some(host) = self.sync.host(&pinned.id).await {
            let h = host.lock().await;
            h.doc
                .append_system_note(&ulid(), sender, &text, &self.cfg.device_id);
        }
    }
}
