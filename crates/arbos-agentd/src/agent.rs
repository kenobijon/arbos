//! The project agent driver: one `pi --mode rpc` child per Project (D65), one pi
//! session per chat, the `route` tool validated here, act limits (D66), streams
//! folded into the chat doc, subagents through the `HarnessAdapter` registry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};

use arbos_core::doc::{CommandStatus, CommandView, MessageStatus};
use arbos_core::ids::ulid;
use arbos_core::route::{pick_harness, validate, Harness, ValidRoute};
use arbos_core::rpc::{AssistantEvent, Command as RpcCommand, Event};
use arbos_core::{ACT_MAX_WALL_SECS, TOOL_OUTPUT_SUMMARY_MAX};

use crate::config::Config;
use crate::harness::{
    AgentEvent, ClaudeCodeAdapter, HarnessAdapter, PiAdapter, PrimeAdapter, StartRequest,
};
use crate::llm::{extract_return_contract, Llm};
use crate::memory::Memory;
use crate::rpc_child::RpcChild;
use crate::store::Store;
use crate::sync::{SharedHost, SyncHub};

/// Streams the `reply` string out of the `route` tool's partial JSON arguments.
#[derive(Default)]
pub struct ReplyExtractor {
    buf: String,
    pos: usize,
    in_string: bool,
    done: bool,
    pending_escape: Option<String>,
}

impl ReplyExtractor {
    /// Feed one `toolcall_delta`; returns newly decoded reply text.
    pub fn feed(&mut self, delta: &str) -> String {
        if self.done {
            return String::new();
        }
        self.buf.push_str(delta);
        let mut out = String::new();
        if !self.in_string {
            let Some(k) = self.buf.find("\"reply\"") else {
                return out;
            };
            let after = &self.buf[k + 7..];
            let Some(colon) = after.find(':') else {
                return out;
            };
            let rest = &after[colon + 1..];
            let trimmed = rest.trim_start();
            if trimmed.is_empty() {
                return out;
            }
            if !trimmed.starts_with('"') {
                self.done = true; // null reply
                return out;
            }
            let q = self.buf.len() - trimmed.len();
            self.pos = q + 1;
            self.in_string = true;
        }
        let bytes = self.buf.as_bytes();
        let mut i = self.pos;
        let mut esc = self.pending_escape.take().unwrap_or_default();
        while i < bytes.len() {
            let c = bytes[i] as char;
            if !esc.is_empty() {
                esc.push(c);
                if esc == "\\u" || (esc.starts_with("\\u") && esc.len() < 6) {
                    i += 1;
                    continue;
                }
                match esc.as_str() {
                    "\\n" => out.push('\n'),
                    "\\t" => out.push('\t'),
                    "\\r" => out.push('\r'),
                    "\\\"" => out.push('"'),
                    "\\\\" => out.push('\\'),
                    "\\/" => out.push('/'),
                    s if s.starts_with("\\u") && s.len() == 6 => {
                        if let Some(ch) = u32::from_str_radix(&s[2..], 16)
                            .ok()
                            .and_then(char::from_u32)
                        {
                            out.push(ch);
                        }
                    }
                    other => out.push_str(&other[1..]),
                }
                esc.clear();
                i += 1;
                continue;
            }
            if c == '\\' {
                esc.push('\\');
                i += 1;
                continue;
            }
            if c == '"' {
                self.done = true;
                i += 1;
                break;
            }
            // Multi-byte UTF-8: copy whole chars.
            let ch_len = utf8_len(bytes[i]);
            if i + ch_len > bytes.len() {
                break;
            }
            out.push_str(&self.buf[i..i + ch_len]);
            i += ch_len;
        }
        self.pos = i;
        if !esc.is_empty() {
            self.pending_escape = Some(esc);
        }
        out
    }
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[derive(Debug, Clone, Default)]
pub struct TurnMetrics {
    pub ttft_ms: Option<i64>,
    pub route_ms: Option<i64>,
    pub tool_calls: i64,
    pub usage: Option<Value>,
}

pub struct TurnState {
    pub chat_id: String,
    pub user_msg_id: String,
    pub assistant_msg_id: String,
    pub text_part_id: String,
    pub started: Instant,
    pub route: Option<ValidRoute>,
    pub route_attempts: u32,
    pub route_failed: bool,
    pub escalate: bool,
    pub asked: Option<String>,
    pub metrics: TurnMetrics,
    pub streamed_text: String,
    pub act_transcript: Vec<String>,
    pub reply_extractor: ReplyExtractor,
    pub route_call_id: Option<String>,
    pub text_part_dirty: bool,
}

pub struct Agent {
    pub cfg: Arc<Config>,
    pub store: Store,
    pub sync: SyncHub,
    pub llm: Arc<Llm>,
    pub memory: Arc<Memory>,
    pi: Mutex<Option<Arc<RpcChild>>>,
    current_chat: Mutex<Option<String>>,
    pub turn: Arc<Mutex<Option<TurnState>>>,
    adapters: HashMap<Harness, Arc<dyn HarnessAdapter>>,
    pub turn_running: AtomicBool,
    pub pi_ready: AtomicBool,
    pub ext_loaded: AtomicBool,
    turn_lock: Mutex<()>,
    http: reqwest::Client,
}

impl Agent {
    pub fn new(cfg: Arc<Config>, store: Store, sync: SyncHub) -> Arc<Self> {
        let llm = Arc::new(Llm::new(&cfg));
        let memory = Arc::new(Memory::new(cfg.clone(), store.clone(), llm.clone()));
        let mut adapters: HashMap<Harness, Arc<dyn HarnessAdapter>> = HashMap::new();
        adapters.insert(
            Harness::Prime,
            Arc::new(PrimeAdapter::new(cfg.clone(), store.clone())),
        );
        adapters.insert(
            Harness::Pi,
            Arc::new(PiAdapter::new(cfg.clone(), store.clone())),
        );
        adapters.insert(
            Harness::ClaudeCode,
            Arc::new(ClaudeCodeAdapter::new(cfg.clone(), store.clone())),
        );
        Arc::new(Self {
            cfg,
            store,
            sync,
            llm,
            memory,
            pi: Mutex::new(None),
            current_chat: Mutex::new(None),
            turn: Arc::new(Mutex::new(None)),
            adapters,
            turn_running: AtomicBool::new(false),
            pi_ready: AtomicBool::new(false),
            ext_loaded: AtomicBool::new(false),
            turn_lock: Mutex::new(()),
            http: reqwest::Client::new(),
        })
    }

    pub fn adapter(&self, h: Harness) -> Arc<dyn HarnessAdapter> {
        self.adapters.get(&h).cloned().expect("registered adapter")
    }

    // ---- pi child -----------------------------------------------------------------------------

    fn system_prompt_path(&self) -> std::path::PathBuf {
        self.cfg.state_dir.join("pi").join("system-prompt.md")
    }

    /// Router prompt (byte-stable, first) + project instructions + T0/T1 (design §6.9 layout).
    pub fn write_system_prompt(&self) -> Result<()> {
        let mut s = String::from(include_str!("../prompts/router.md"));
        if !self.cfg.instructions.trim().is_empty() {
            s.push_str("\n\n## Project instructions\n\n");
            s.push_str(self.cfg.instructions.trim());
            s.push('\n');
        }
        let core = self.memory.core_memory_text();
        if !core.trim().is_empty() {
            s.push_str("\n\n## Project memory\n\n");
            s.push_str(&core);
        }
        std::fs::create_dir_all(self.system_prompt_path().parent().unwrap())?;
        std::fs::write(self.system_prompt_path(), s)?;
        Ok(())
    }

    /// Deterministic pi session file for a chat: `<sessions>/<ts>_<chatId>.jsonl` (one session per chat).
    fn session_file_for(&self, chat_id: &str) -> Result<std::path::PathBuf> {
        if let Some(p) = self.store.chat(chat_id)?.and_then(|c| c.pi_session_path) {
            if std::path::Path::new(&p).exists() {
                return Ok(p.into());
            }
        }
        let dir = self.cfg.pi_sessions_dir();
        std::fs::create_dir_all(&dir)?;
        if let Some(existing) = crate::harness::find_session_file(&dir, chat_id) {
            self.store
                .set_chat_session_path(chat_id, &existing.display().to_string())?;
            return Ok(existing);
        }
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ");
        let path = dir.join(format!("{ts}_{chat_id}.jsonl"));
        let header = json!({ "type": "session", "version": 3, "id": chat_id, "timestamp": chrono::Utc::now().to_rfc3339(), "cwd": self.cfg.work_dir });
        std::fs::write(&path, format!("{header}\n"))?;
        self.store
            .set_chat_session_path(chat_id, &path.display().to_string())?;
        Ok(path)
    }

    pub async fn start_pi(self: &Arc<Self>, initial_chat: &str) -> Result<()> {
        self.write_system_prompt()?;
        let session = self.session_file_for(initial_chat)?;
        let mut args = PiAdapter::common_args(
            &self.cfg,
            &self.cfg.pi_sessions_dir(),
            &self.cfg.talking_model,
            &self.system_prompt_path(),
        );
        args.extend(["--session".into(), session.display().to_string()]);
        let log = self.cfg.state_dir.join("logs").join("pi-main.log");
        std::fs::create_dir_all(log.parent().unwrap())?;
        std::fs::create_dir_all(&self.cfg.work_dir)?;
        let t0 = Instant::now();
        let child = RpcChild::spawn(
            "pi-main",
            &self.cfg.pi_bin,
            &args,
            &PiAdapter::envs(&self.cfg, initial_chat, "main"),
            &self.cfg.work_dir,
            &log,
        )
        .await?;
        let state = child
            .call_timeout(
                RpcCommand::GetState {
                    id: "boot-state".into(),
                },
                Duration::from_secs(30),
            )
            .await?;
        if !state.success {
            return Err(anyhow!("pi get_state failed: {:?}", state.error));
        }
        tracing::info!(ms = t0.elapsed().as_millis() as i64, "pi main agent ready");
        *self.pi.lock().await = Some(child);
        *self.current_chat.lock().await = Some(initial_chat.to_owned());
        self.pi_ready.store(true, Ordering::SeqCst);
        self.store.upsert_session(
            initial_chat,
            None,
            Some(&session.display().to_string()),
            &self.cfg.work_dir.display().to_string(),
            &self.cfg.talking_model,
            "pi",
            "main",
        )?;
        let _ = self
            .post_timing("pi_boot_ms", t0.elapsed().as_millis() as i64, None)
            .await;
        // The extension writes an `extension_loaded` audit line on load.
        if let Ok(s) = std::fs::read_to_string(self.cfg.audit_path()) {
            if s.contains("\"extension_loaded\"") {
                self.ext_loaded.store(true, Ordering::SeqCst);
            }
        }
        // Supervise: respawn on exit (sandbox-image.md §3.3).
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let alive = me
                    .pi
                    .lock()
                    .await
                    .as_ref()
                    .map(|c| c.is_alive())
                    .unwrap_or(false);
                if !alive && !me.turn_running.load(Ordering::SeqCst) {
                    me.pi_ready.store(false, Ordering::SeqCst);
                    let chat = me.current_chat.lock().await.clone().unwrap_or_default();
                    tracing::warn!("pi main child is gone; respawning");
                    if let Err(e) = me.respawn_pi(&chat).await {
                        tracing::error!(error = %e, "pi respawn failed");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
        Ok(())
    }

    async fn respawn_pi(self: &Arc<Self>, chat_id: &str) -> Result<()> {
        if let Some(old) = self.pi.lock().await.take() {
            old.shutdown().await;
        }
        let session = self.session_file_for(chat_id)?;
        let mut args = PiAdapter::common_args(
            &self.cfg,
            &self.cfg.pi_sessions_dir(),
            &self.cfg.talking_model,
            &self.system_prompt_path(),
        );
        args.extend(["--session".into(), session.display().to_string()]);
        let log = self.cfg.state_dir.join("logs").join("pi-main.log");
        let child = RpcChild::spawn(
            "pi-main",
            &self.cfg.pi_bin,
            &args,
            &PiAdapter::envs(&self.cfg, chat_id, "main"),
            &self.cfg.work_dir,
            &log,
        )
        .await?;
        child
            .call_timeout(
                RpcCommand::GetState {
                    id: "boot-state".into(),
                },
                Duration::from_secs(30),
            )
            .await?;
        *self.pi.lock().await = Some(child);
        *self.current_chat.lock().await = Some(chat_id.to_owned());
        self.pi_ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn pi(&self) -> Result<Arc<RpcChild>> {
        self.pi
            .lock()
            .await
            .clone()
            .ok_or_else(|| anyhow!("pi main agent not running"))
    }

    async fn ensure_chat_session(&self, chat_id: &str) -> Result<()> {
        if self.current_chat.lock().await.as_deref() == Some(chat_id) {
            return Ok(());
        }
        let path = self.session_file_for(chat_id)?;
        let pi = self.pi().await?;
        let t0 = Instant::now();
        pi.call_ok(RpcCommand::SwitchSession {
            id: pi.next_id("sw"),
            session_path: path.display().to_string(),
        })
        .await?;
        tracing::info!(
            chat_id,
            ms = t0.elapsed().as_millis() as i64,
            "switched pi session"
        );
        *self.current_chat.lock().await = Some(chat_id.to_owned());
        self.store.upsert_session(
            chat_id,
            None,
            Some(&path.display().to_string()),
            &self.cfg.work_dir.display().to_string(),
            &self.cfg.talking_model,
            "pi",
            "main",
        )?;
        Ok(())
    }

    // ---- commands -----------------------------------------------------------------------------

    pub async fn handle_command(self: &Arc<Self>, chat_id: &str, cmd: CommandView) {
        let Some(host) = self.sync.host(chat_id).await else {
            return;
        };
        {
            host.lock()
                .await
                .doc
                .set_command_status(&cmd.id, CommandStatus::Taken, None);
        }
        let res = match cmd.kind.as_str() {
            "send" => self.handle_send(chat_id, &host, &cmd).await,
            "stop" => self.handle_stop().await,
            "answer" => self.handle_send(chat_id, &host, &cmd).await,
            "retitle" => Ok(()),
            "retry" => Ok(()),
            other => Err(anyhow!("unknown command kind {other}")),
        };
        let mut h = host.lock().await;
        match res {
            Ok(()) => h.doc.set_command_status(&cmd.id, CommandStatus::Done, None),
            Err(e) => {
                tracing::warn!(chat_id, cmd = %cmd.kind, error = %e, "command failed");
                h.doc.set_command_status(
                    &cmd.id,
                    CommandStatus::Failed,
                    Some(json!({ "error": e.to_string() })),
                );
            }
        }
    }

    async fn handle_stop(&self) -> Result<()> {
        if let Ok(pi) = self.pi().await {
            let _ = pi
                .call(RpcCommand::Abort {
                    id: pi.next_id("ab"),
                })
                .await;
        }
        Ok(())
    }

    async fn handle_send(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        cmd: &CommandView,
    ) -> Result<()> {
        let _guard = self.turn_lock.lock().await;
        let user_msg_id = cmd
            .payload
            .get("messageId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let user_text = {
            let h = host.lock().await;
            h.doc
                .message(&user_msg_id)
                .map(|m| m.text())
                .unwrap_or_default()
        };
        if user_text.trim().is_empty() {
            return Err(anyhow!("send: message {user_msg_id} not found or empty"));
        }
        let _ = self.store.touch_chat(chat_id);
        let _ = self.store.t3_append(
            chat_id,
            None,
            Some(chat_id),
            "user",
            Some("user"),
            &user_text,
            Some(&json!({ "message_id": user_msg_id })),
        );
        self.ensure_chat_session(chat_id).await?;

        let assistant_msg_id = ulid();
        let text_part_id = {
            let mut h = host.lock().await;
            let pid =
                h.doc
                    .start_assistant_message(&assistant_msg_id, &user_msg_id, &self.cfg.device_id);
            h.doc
                .add_status_part(&assistant_msg_id, "deciding", "Arbos is deciding…");
            pid
        };
        // Answering an open question (S8): mark it answered; pi receives the text as the next prompt.
        let mut prompt_text = user_text.clone();
        if let Ok(Some((ask_id, _, question))) = self.store.open_ask(chat_id) {
            let _ = self.store.answer_ask(&ask_id);
            prompt_text = format!("Answer to your question \"{question}\": {user_text}");
        }

        *self.turn.lock().await = Some(TurnState {
            chat_id: chat_id.to_owned(),
            user_msg_id: user_msg_id.clone(),
            assistant_msg_id: assistant_msg_id.clone(),
            text_part_id: text_part_id.clone(),
            started: Instant::now(),
            route: None,
            route_attempts: 0,
            route_failed: false,
            escalate: false,
            asked: None,
            metrics: TurnMetrics::default(),
            streamed_text: String::new(),
            act_transcript: Vec::new(),
            reply_extractor: ReplyExtractor::default(),
            route_call_id: None,
            text_part_dirty: false,
        });
        self.turn_running.store(true, Ordering::SeqCst);
        let keepalive = self.spawn_keepalive();
        let result = self.run_main_turn(chat_id, host, &prompt_text).await;
        self.turn_running.store(false, Ordering::SeqCst);
        keepalive.abort();

        let turn = self.turn.lock().await.take();
        let Some(turn) = turn else { return result };
        let total_ms = turn.started.elapsed().as_millis() as i64;
        let route_name = turn
            .route
            .as_ref()
            .map(route_name)
            .unwrap_or(if turn.route_failed { "reply" } else { "none" });
        let (final_status, err_text) = match &result {
            Ok(()) => (MessageStatus::Done, None),
            Err(e) => (MessageStatus::Error, Some(e.to_string())),
        };
        {
            let mut h = host.lock().await;
            if let Some(e) = &err_text {
                h.doc.add_error_part(&assistant_msg_id, "turn_failed", e);
            }
            h.doc.set_message_fields(
                &assistant_msg_id,
                &[
                    ("decision", json!(route_name)),
                    ("turn", json!({ "kind": route_name })),
                    ("model", json!(self.cfg.talking_model)),
                    ("usage", turn.metrics.usage.clone().unwrap_or(Value::Null)),
                ],
            );
            h.doc.finish_message(&assistant_msg_id, final_status);
        }
        if !turn.streamed_text.trim().is_empty() {
            let _ = self.store.t3_append(
                chat_id,
                None,
                Some(chat_id),
                "assistant",
                Some("assistant"),
                &turn.streamed_text,
                Some(&json!({ "message_id": assistant_msg_id, "route": route_name })),
            );
        }
        let _ = self.store.record_turn(
            chat_id,
            &user_msg_id,
            Some(route_name),
            turn.metrics.ttft_ms,
            turn.metrics.route_ms,
            total_ms,
            turn.metrics.tool_calls,
            turn.escalate,
            turn.metrics.usage.as_ref(),
        );
        if let Some(ttft) = turn.metrics.ttft_ms {
            let _ = self
                .post_timing("main_turn_ttft_ms", ttft, Some(route_name))
                .await;
        }
        let _ = self
            .post_timing("main_turn_total_ms", total_ms, Some(route_name))
            .await;
        tracing::info!(chat_id, route = route_name, ttft_ms = ?turn.metrics.ttft_ms, route_ms = ?turn.metrics.route_ms, total_ms, tool_calls = turn.metrics.tool_calls, "turn done");

        // Off the reply path: memory write pass + compaction (design §6.9).
        {
            let me = self.clone();
            let chat = chat_id.to_owned();
            let user_text2 = user_text.clone();
            let reply = turn.streamed_text.clone();
            let msg = assistant_msg_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                let text = format!("User: {user_text2}\nAgent: {reply}");
                if let Err(e) = me
                    .memory
                    .write_pass(&msg, &chat, "assistant", &text, &[])
                    .await
                {
                    tracing::debug!(error = %e, "memory write pass");
                }
                if let Err(e) = me.memory.maybe_compact(&chat).await {
                    tracing::debug!(error = %e, "compaction");
                }
                let _ = me.write_system_prompt();
            });
        }

        // Hand-offs after the main turn: new / wake / escalation.
        let followup = match (&turn.route, turn.escalate) {
            (
                Some(ValidRoute::New {
                    name,
                    purpose,
                    brief,
                    harness,
                    isolate: _,
                    data_heavy,
                }),
                _,
            ) => Some((
                None,
                name.clone(),
                purpose.clone(),
                brief.clone(),
                *harness,
                *data_heavy,
                false,
            )),
            (Some(ValidRoute::Wake { subagent_id, brief }), _) => Some((
                Some(subagent_id.clone()),
                String::new(),
                String::new(),
                brief.clone(),
                None,
                false,
                false,
            )),
            (Some(ValidRoute::Act { plan }), true) => {
                let brief = format!(
                    "Continue this task that the project agent started and could not finish within its limits.\n\nUser request: {user_text}\n\nPlan: {plan}\n\nAct so far:\n{}\n\nFinish the task.",
                    turn.act_transcript.join("\n").chars().take(2400).collect::<String>()
                );
                Some((
                    None,
                    "continue-act".into(),
                    "Finish a task the project agent started".into(),
                    brief,
                    None,
                    false,
                    true,
                ))
            }
            _ => None,
        };
        if let Some((existing, name, purpose, brief, harness, data_heavy, escalated)) = followup {
            let me = self.clone();
            let chat = chat_id.to_owned();
            let host2 = host.clone();
            let user_msg = user_msg_id.clone();
            tokio::spawn(async move {
                if let Err(e) = me
                    .run_subagent(
                        &chat, &host2, &user_msg, existing, &name, &purpose, &brief, harness,
                        data_heavy, escalated,
                    )
                    .await
                {
                    tracing::warn!(error = %e, "subagent run failed");
                    let mut h = host2.lock().await;
                    let id = ulid();
                    h.doc
                        .start_assistant_message(&id, &user_msg, &me.cfg.device_id);
                    h.doc.add_error_part(&id, "subagent_failed", &e.to_string());
                    h.doc.finish_message(&id, MessageStatus::Error);
                }
            });
        }
        result
    }

    fn spawn_keepalive(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let _ = me.keepalive(true).await;
            let mut t = tokio::time::interval(Duration::from_secs(5 * 60));
            t.tick().await;
            loop {
                t.tick().await;
                let _ = me.keepalive(true).await;
            }
        })
    }

    pub async fn keepalive(&self, turn_running: bool) -> Result<()> {
        self.http
            .post(format!(
                "{}/projects/{}/keepalive",
                self.cfg.relay_url, self.cfg.project_id
            ))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "turn_running": turn_running }))
            .send()
            .await?;
        Ok(())
    }

    pub async fn post_timing(&self, kind: &str, ms: i64, detail: Option<&str>) -> Result<()> {
        self.http
            .post(format!(
                "{}/projects/{}/timings",
                self.cfg.relay_url, self.cfg.project_id
            ))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "kind": kind, "ms": ms, "detail": detail }))
            .send()
            .await?;
        Ok(())
    }

    /// Upload a blob to the relay mirror, fire-and-forget. Returns the key.
    pub fn upload_blob(self: &Arc<Self>, bytes: Vec<u8>, mime: &str) -> String {
        use sha2::Digest;
        let key = hex::encode(sha2::Sha256::digest(&bytes));
        let me = self.clone();
        let mime = mime.to_owned();
        let k = key.clone();
        tokio::spawn(async move {
            let _ = me
                .http
                .put(format!(
                    "{}/blob/{}/{}",
                    me.cfg.relay_url, me.cfg.project_id, k
                ))
                .bearer_auth(&me.cfg.box_token)
                .header("content-type", mime)
                .body(bytes)
                .send()
                .await;
        });
        key
    }

    // ---- the main turn ------------------------------------------------------------------------

    async fn run_main_turn(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        prompt_text: &str,
    ) -> Result<()> {
        let pi = self.pi().await?;
        let mut events = pi.subscribe();
        let r = pi
            .call(RpcCommand::Prompt {
                id: pi.next_id("p"),
                message: prompt_text.to_owned(),
                streaming_behavior: None,
            })
            .await?;
        if !r.success {
            return Err(anyhow!("prompt rejected: {}", r.error.unwrap_or_default()));
        }
        let started = Instant::now();
        let mut act_steered = false;
        let mut act_aborted = false;
        let deadline = Duration::from_secs(15 * 60);
        loop {
            let ev = tokio::select! {
                e = events.recv() => e,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    // Act wall-clock limit (D66): steer at 60 s, abort 20 s later.
                    let is_act = matches!(self.turn.lock().await.as_ref().and_then(|t| t.route.as_ref()), Some(ValidRoute::Act { .. }));
                    if is_act && started.elapsed() > Duration::from_secs(ACT_MAX_WALL_SECS) && !act_steered {
                        act_steered = true;
                        let _ = pi.call(RpcCommand::Steer { id: pi.next_id("st"), message: "Time limit reached. Stop now and summarize what you did and what is left.".into() }).await;
                        if let Some(t) = self.turn.lock().await.as_mut() { t.escalate = true; }
                    }
                    if is_act && started.elapsed() > Duration::from_secs(ACT_MAX_WALL_SECS + 20) && !act_aborted {
                        act_aborted = true;
                        let _ = pi.call(RpcCommand::Abort { id: pi.next_id("ab") }).await;
                    }
                    if started.elapsed() > deadline {
                        let _ = pi.call(RpcCommand::Abort { id: pi.next_id("ab") }).await;
                        return Err(anyhow!("turn exceeded 15 minutes"));
                    }
                    continue;
                }
            };
            let ev = match ev {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "lagged behind pi events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(anyhow!("pi event stream closed"))
                }
            };
            if self.fold_main_event(chat_id, host, &ev, started).await? {
                break;
            }
            if matches!(ev, Event::Other) && !pi.is_alive() {
                return Err(anyhow!("pi main child exited mid-turn"));
            }
        }
        Ok(())
    }

    /// Fold one pi event into the chat doc and the turn state. Returns true when the turn settled.
    async fn fold_main_event(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        ev: &Event,
        started: Instant,
    ) -> Result<bool> {
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return Ok(true);
        };
        let elapsed = started.elapsed().as_millis() as i64;
        match ev {
            Event::MessageUpdate {
                assistant_message_event,
                usage,
            } => {
                if let Some(u) = usage {
                    if u.total_tokens > 0 {
                        t.metrics.usage = Some(
                            json!({ "input": u.input, "output": u.output, "cacheRead": u.cache_read, "cacheWrite": u.cache_write, "cost": u.cost }),
                        );
                    }
                }
                match assistant_message_event {
                    AssistantEvent::TextDelta { delta, .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                        self.append_text(host, t, delta).await;
                    }
                    AssistantEvent::ThinkingDelta { .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                    }
                    AssistantEvent::ToolcallStart { id, tool_name, .. } => {
                        if t.metrics.ttft_ms.is_none() {
                            t.metrics.ttft_ms = Some(elapsed);
                        }
                        if tool_name == "route" {
                            t.route_call_id = Some(id.clone());
                            t.reply_extractor = ReplyExtractor::default();
                        }
                    }
                    AssistantEvent::ToolcallDelta {
                        delta,
                        content_index: _,
                    } => {
                        if t.route_call_id.is_some() && t.route.is_none() {
                            let text = t.reply_extractor.feed(delta);
                            if !text.is_empty() {
                                self.append_text(host, t, &text).await;
                            }
                        }
                    }
                    AssistantEvent::ToolcallEnd { .. }
                    | AssistantEvent::Start
                    | AssistantEvent::TextStart { .. }
                    | AssistantEvent::TextEnd { .. }
                    | AssistantEvent::ThinkingStart { .. }
                    | AssistantEvent::ThinkingEnd { .. }
                    | AssistantEvent::Done { .. }
                    | AssistantEvent::Unknown => {}
                    AssistantEvent::Error { error, .. } => {
                        let mut h = host.lock().await;
                        h.doc.add_error_part(
                            &t.assistant_msg_id,
                            "model_error",
                            &error
                                .as_ref()
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "provider error".into()),
                        );
                    }
                }
            }
            Event::ToolExecutionStart {
                tool_call_id: _,
                tool_name,
                args,
            } => {
                if tool_name == "route" {
                    return Ok(false);
                }
                t.metrics.tool_calls += 1;
                let one_line = args.to_string().chars().take(120).collect::<String>();
                t.act_transcript.push(format!("→ {tool_name} {one_line}"));
                let mut h = host.lock().await;
                h.doc.add_status_part(
                    &t.assistant_msg_id,
                    "tool_start",
                    &format!("{tool_name} {one_line}"),
                );
                t.text_part_dirty = true;
            }
            Event::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                ..
            } => {
                if tool_name == "route" {
                    if t.metrics.route_ms.is_none() {
                        t.metrics.route_ms = Some(elapsed);
                    }
                    if t.route.is_none() && !t.route_failed {
                        // A route the hook never saw (no agentd reachable): validate from the result args is impossible; treat as reply.
                    }
                    return Ok(false);
                }
                let output = result
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| c.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                t.act_transcript.push(format!(
                    "← {tool_name}: {}",
                    output.chars().take(300).collect::<String>()
                ));
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "tool_output",
                    Some("tool"),
                    &format!("{tool_name}: {output}"),
                    Some(&json!({ "call_id": tool_call_id })),
                );
                let summary: String = output.chars().take(TOOL_OUTPUT_SUMMARY_MAX).collect();
                let output_ref = if output.len() > TOOL_OUTPUT_SUMMARY_MAX {
                    Some(self.upload_blob(output.clone().into_bytes(), "text/plain"))
                } else {
                    None
                };
                let mut h = host.lock().await;
                h.doc.add_tool_part(
                    &t.assistant_msg_id,
                    tool_call_id,
                    tool_name,
                    &json!({}),
                    &summary,
                    output_ref.as_deref(),
                    Some(output.len() as i64),
                    *is_error,
                );
                t.text_part_dirty = true;
            }
            Event::AgentSettled { .. } => return Ok(true),
            Event::CompactionEnd { .. } => {
                let _ = self.store.bump_session_compactions(chat_id);
                let _ = self.store.t3_append(
                    chat_id,
                    None,
                    Some(chat_id),
                    "compaction",
                    Some("system"),
                    "pi session compacted",
                    None,
                );
            }
            Event::Other
            | Event::AgentStart
            | Event::AgentEnd { .. }
            | Event::TurnStart
            | Event::TurnEnd { .. }
            | Event::MessageStart { .. }
            | Event::MessageEnd { .. }
            | Event::ToolExecutionUpdate { .. }
            | Event::QueueUpdate { .. }
            | Event::EntryAppended { .. }
            | Event::CompactionStart { .. }
            | Event::ExtensionUiRequest { .. } => {}
        }
        Ok(false)
    }

    async fn append_text(&self, host: &SharedHost, t: &mut TurnState, delta: &str) {
        let mut h = host.lock().await;
        if t.text_part_dirty {
            // A tool part landed since the last text: open a new text part so order is kept.
            if let Some(p) = h.doc.add_text_part(&t.assistant_msg_id, "text") {
                t.text_part_id = p;
            }
            t.text_part_dirty = false;
        }
        h.doc
            .append_text(&t.assistant_msg_id, &t.text_part_id, delta);
        t.streamed_text.push_str(delta);
    }

    // ---- hooks called by arbos-pi-ext ---------------------------------------------------------

    /// `/hook/route`: validate; one retry; a second failure is treated as `reply` (design §6.2).
    pub async fn hook_route(&self, args: &Value) -> Result<(), String> {
        let mut tl = self.turn.lock().await;
        let Some(t) = tl.as_mut() else {
            return Err("no turn is running".into());
        };
        t.route_attempts += 1;
        match validate(args) {
            Ok(r) => {
                tracing::info!(
                    route = route_name(&r),
                    attempt = t.route_attempts,
                    "route accepted"
                );
                let _ = self.store.t3_append(
                    &t.chat_id,
                    None,
                    Some(&t.chat_id),
                    "route",
                    Some("system"),
                    &format!("route {}: {}", route_name(&r), args),
                    None,
                );
                t.route = Some(r);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, attempt = t.route_attempts, "route invalid");
                if t.route_attempts >= 2 {
                    t.route_failed = true;
                    t.route = Some(ValidRoute::Reply { reply: None });
                }
                Err(e)
            }
        }
    }

    pub async fn hook_ask_user(&self, question: &str, session_id: &str) -> Result<()> {
        let mut tl = self.turn.lock().await;
        let chat_id = tl
            .as_ref()
            .map(|t| t.chat_id.clone())
            .unwrap_or_else(|| session_id.to_owned());
        let id = ulid();
        self.store
            .insert_ask(&id, Some(&chat_id), Some(session_id), question)?;
        if let Some(t) = tl.as_mut() {
            t.asked = Some(question.to_owned());
            if let Some(host) = self.sync.host(&t.chat_id).await {
                let mut h = host.lock().await;
                h.doc
                    .add_status_part(&t.assistant_msg_id, "waiting_user", question);
                h.doc
                    .set_message_fields(&t.assistant_msg_id, &[("waitingUser", json!(true))]);
            }
        }
        let _ = self.store.t3_append(
            &chat_id,
            None,
            Some(session_id),
            "ask_user",
            Some("assistant"),
            question,
            None,
        );
        Ok(())
    }

    pub async fn hook_act_limit(&self) {
        if let Some(t) = self.turn.lock().await.as_mut() {
            t.escalate = true;
        }
    }

    /// Working-set section (design §6.6 item 2b): compaction summary + handles + notes, ≤ 40 lines.
    pub fn working_set_text(&self, chat_id: &str) -> String {
        let mut lines = Vec::new();
        if let Ok(Some(c)) = self.store.chat(chat_id) {
            if let Some(s) = c.compaction_summary {
                lines.push(format!(
                    "Summary of earlier turns in this chat: {}",
                    s.chars().take(6000).collect::<String>()
                ));
            }
        }
        for h in self.store.handles_for_chat(chat_id, 20).unwrap_or_default() {
            lines.push(format!(
                "- {} ({}, {} chars, {} lines): {}",
                h.id, h.kind, h.chars, h.lines, h.note
            ));
        }
        for (k, v) in self.store.scratch_all(chat_id).unwrap_or_default() {
            lines.push(format!(
                "- remember:{k} = {}",
                v.chars().take(120).collect::<String>()
            ));
        }
        lines.into_iter().take(40).collect::<Vec<_>>().join("\n")
    }

    // ---- subagents ----------------------------------------------------------------------------

    /// Context pack v0 (design §6.7 sections 1–4, 9, 10).
    fn build_pack(&self, name: &str, purpose: &str, brief: &str, data_heavy: bool) -> String {
        let mut s = String::new();
        s.push_str(&format!("# Task: {name}\n\n{purpose}\n\n{brief}\n\n"));
        if !self.cfg.instructions.trim().is_empty() {
            s.push_str(&format!(
                "## Project instructions\n\n{}\n\n",
                self.cfg.instructions.trim()
            ));
        }
        let core = self.memory.core_memory_text();
        if !core.trim().is_empty() {
            s.push_str("## Project memory (T0 + T1)\n\n");
            s.push_str(&core);
            s.push('\n');
        }
        if data_heavy {
            s.push_str("## Data handling\n\nInputs are large. Keep data in variables or files and print slices; never paste whole files into the conversation.\n\n");
        }
        s.push_str("## Tools\n\nMCP server `arbos`: history_search, history_read, read_thread, memory_search, memory_read (you may search and read the entire history of this Project), peek, sub_llm, remember/recall, projects_list/projects_send, ask_user.\n\n");
        s.push_str("## Return contract\n\nEnd your final reply with one fenced ```json block:\n{ \"status\": \"done\" | \"blocked\" | \"partial\" | \"needs_input\", \"summary\": \"<= 5 lines\", \"artifacts\": [ { \"kind\": \"file\" | \"commit\" | \"url\" | \"command\", \"ref\": \"…\", \"note\": \"…\" } ], \"acceptance\": [ { \"criterion\": \"…\", \"met\": true, \"evidence\": \"…\" } ], \"open_items\": [], \"proposed_memory\": [ { \"kind\": \"decision\", \"scope\": \"project\", \"text\": \"…\" } ], \"questions\": [] }\n");
        s
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_subagent(
        self: &Arc<Self>,
        chat_id: &str,
        host: &SharedHost,
        in_reply_to: &str,
        existing: Option<String>,
        name: &str,
        purpose: &str,
        brief: &str,
        route_harness: Option<Harness>,
        data_heavy: bool,
        escalated: bool,
    ) -> Result<()> {
        let default_harness = std::env::var("ARBOS_DEFAULT_HARNESS")
            .ok()
            .and_then(|s| match s.as_str() {
                "pi" => Some(Harness::Pi),
                "claude_code" => Some(Harness::ClaudeCode),
                "prime" => Some(Harness::Prime),
                _ => None,
            })
            .unwrap_or(Harness::Prime);
        let (subagent_id, harness, is_new) = match existing {
            Some(id) => {
                let row = self
                    .store
                    .subagent(&id)?
                    .ok_or_else(|| anyhow!("unknown subagent {id}"))?;
                let h = match row.harness.as_str() {
                    "pi" => Harness::Pi,
                    "claude_code" => Harness::ClaudeCode,
                    _ => Harness::Prime,
                };
                (id, h, false)
            }
            None => {
                let h = pick_harness(
                    route_harness,
                    data_heavy,
                    brief,
                    &self.cfg.subagent_model,
                    default_harness,
                );
                let id = ulid();
                self.store
                    .insert_subagent(&id, name, purpose, h.as_str(), chat_id)?;
                (id, h, true)
            }
        };
        let adapter = self.adapter(harness);
        let sub_name = self
            .store
            .subagent(&subagent_id)?
            .map(|s| s.name)
            .unwrap_or_else(|| name.to_owned());

        // Templated acknowledgement first (design §6.9), then the subagent's own message.
        let msg_id = ulid();
        let text_part = {
            let mut h = host.lock().await;
            let pid = h
                .doc
                .start_assistant_message(&msg_id, in_reply_to, &self.cfg.device_id);
            let ack = if is_new {
                format!("Starting *{sub_name}* on this ({})…", harness.as_str())
            } else {
                format!("Picking up *{sub_name}* where it left off…")
            };
            h.doc.append_text(&msg_id, &pid, &ack);
            h.doc.add_status_part(
                &msg_id,
                "subagent_working",
                &format!("Working in sandbox · {sub_name}"),
            );
            h.doc.set_message_fields(
                &msg_id,
                &[
                    ("subagentId", json!(subagent_id)),
                    (
                        "turn",
                        json!({ "kind": if is_new { "spawn" } else { "wake" } }),
                    ),
                    ("escalated_from_act", json!(escalated)),
                    ("model", json!(self.cfg.subagent_model)),
                ],
            );
            pid
        };
        self.write_threads_index().await;
        let t0 = Instant::now();
        let pack = self.build_pack(&sub_name, purpose, brief, data_heavy);
        std::fs::create_dir_all(self.cfg.state_dir.join("packs"))?;
        std::fs::write(
            self.cfg
                .state_dir
                .join("packs")
                .join(format!("{subagent_id}.md")),
            &pack,
        )?;
        self.store.set_subagent_status(&subagent_id, "running")?;
        let _ = self.store.t3_append(
            &subagent_id,
            Some(&subagent_id),
            Some(&subagent_id),
            "user",
            Some("user"),
            brief,
            Some(&json!({ "chat_id": chat_id, "pack": "pack-v0" })),
        );

        if is_new {
            adapter
                .start(StartRequest {
                    session_id: subagent_id.clone(),
                    cwd: self.cfg.work_dir.clone(),
                    system_prompt: pack.clone(),
                    model: self.cfg.subagent_model.clone(),
                    thinking: "medium".into(),
                })
                .await?;
            let _ = self
                .post_timing(
                    "subagent_start_ms",
                    t0.elapsed().as_millis() as i64,
                    Some(harness.as_str()),
                )
                .await;
        } else if !adapter.is_open(&subagent_id) {
            adapter.resume(&subagent_id).await?;
            let _ = self
                .post_timing(
                    "subagent_resume_ms",
                    t0.elapsed().as_millis() as i64,
                    Some(harness.as_str()),
                )
                .await;
        }
        let mut stream = adapter
            .stream(&subagent_id)
            .ok_or_else(|| anyhow!("no stream for {subagent_id}"))?;
        let first_prompt = if is_new {
            brief.to_owned()
        } else {
            format!("Context update: continue your thread.\n\n{brief}")
        };
        adapter.prompt(&subagent_id, &first_prompt).await?;

        let keepalive = self.spawn_keepalive();
        let mut text = String::new();
        let mut part_id = text_part;
        let mut part_dirty = true;
        let mut usage_total = json!({});
        let mut first_delta: Option<i64> = None;
        let mut done_err: Option<String> = None;
        let deadline = Instant::now() + Duration::from_secs(60 * 60);
        loop {
            let ev = tokio::select! {
                e = stream.recv() => e,
                _ = tokio::time::sleep_until(deadline.into()) => { done_err = Some("subagent exceeded 60 minutes".into()); break; }
            };
            let ev = match ev {
                Ok(e) => e,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            };
            match ev {
                AgentEvent::TextDelta(d) => {
                    if first_delta.is_none() {
                        first_delta = Some(t0.elapsed().as_millis() as i64);
                    }
                    let mut h = host.lock().await;
                    if part_dirty {
                        if let Some(p) = h.doc.add_text_part(&msg_id, "text") {
                            part_id = p;
                        }
                        part_dirty = false;
                    }
                    h.doc.append_text(&msg_id, &part_id, &d);
                    text.push_str(&d);
                }
                AgentEvent::ReasoningDelta(_) => {}
                AgentEvent::ToolCall { id: _, name, input } => {
                    let one_line = input.to_string().chars().take(120).collect::<String>();
                    let mut h = host.lock().await;
                    h.doc
                        .add_status_part(&msg_id, "tool_start", &format!("{name} {one_line}"));
                    part_dirty = true;
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "tool_call",
                        Some("assistant"),
                        &format!("{name} {input}"),
                        None,
                    );
                }
                AgentEvent::ToolResult {
                    id,
                    name,
                    output,
                    is_error,
                } => {
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "tool_output",
                        Some("tool"),
                        &format!("{name}: {output}"),
                        None,
                    );
                    let summary: String = output.chars().take(TOOL_OUTPUT_SUMMARY_MAX).collect();
                    let output_ref = if output.len() > TOOL_OUTPUT_SUMMARY_MAX {
                        Some(self.upload_blob(output.clone().into_bytes(), "text/plain"))
                    } else {
                        None
                    };
                    let mut h = host.lock().await;
                    h.doc.add_tool_part(
                        &msg_id,
                        &id,
                        &name,
                        &json!({}),
                        &summary,
                        output_ref.as_deref(),
                        Some(output.len() as i64),
                        is_error,
                    );
                    part_dirty = true;
                }
                AgentEvent::Usage {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    cost_usd,
                } => {
                    usage_total = json!({ "input": input, "output": output, "cacheRead": cache_read, "cacheWrite": cache_write, "cost": cost_usd });
                }
                AgentEvent::InputRequested { question } => {
                    let _ = self.store.insert_ask(
                        &ulid(),
                        Some(chat_id),
                        Some(&subagent_id),
                        &question,
                    );
                    let mut h = host.lock().await;
                    h.doc.add_status_part(&msg_id, "waiting_user", &question);
                }
                AgentEvent::CompactionStarted => {}
                AgentEvent::CompactionEnded { summary } => {
                    let n = self
                        .store
                        .bump_session_compactions(&subagent_id)
                        .unwrap_or(0);
                    let _ = self.store.t3_append(
                        &subagent_id,
                        Some(&subagent_id),
                        Some(&subagent_id),
                        "compaction",
                        Some("system"),
                        &format!(
                            "session compacted ({n}); summary {} chars",
                            summary.as_deref().map(str::len).unwrap_or(0)
                        ),
                        None,
                    );
                }
                AgentEvent::Done { aborted, error } => {
                    if aborted || error.is_some() {
                        done_err = error.or(Some("aborted".into()));
                    }
                    break;
                }
            }
        }
        keepalive.abort();
        let total_ms = t0.elapsed().as_millis() as i64;
        let contract = extract_return_contract(&text);
        let status = contract
            .as_ref()
            .and_then(|c| c.get("status"))
            .and_then(Value::as_str)
            .unwrap_or(if done_err.is_some() {
                "blocked"
            } else {
                "done"
            })
            .to_owned();
        {
            let mut h = host.lock().await;
            if let Some(e) = &done_err {
                h.doc.add_error_part(&msg_id, "subagent_error", e);
            }
            h.doc.add_status_part(
                &msg_id,
                "subagent_done",
                &format!("{sub_name} · {status} · {:.1}s", total_ms as f64 / 1000.0),
            );
            h.doc.set_message_fields(
                &msg_id,
                &[
                    ("usage", usage_total.clone()),
                    ("returnContract", contract.clone().unwrap_or(Value::Null)),
                ],
            );
            h.doc.finish_message(
                &msg_id,
                if done_err.is_some() {
                    MessageStatus::Error
                } else {
                    MessageStatus::Done
                },
            );
        }
        let _ = self.store.t3_append(
            &subagent_id,
            Some(&subagent_id),
            Some(&subagent_id),
            "assistant",
            Some("assistant"),
            &text,
            Some(&json!({ "chat_id": chat_id, "message_id": msg_id })),
        );
        let new_status = match status.as_str() {
            "needs_input" => "waiting_user",
            _ => "idle",
        };
        self.store.set_subagent_status(&subagent_id, new_status)?;
        if let Some(c) = &contract {
            if let Some(s) = c.get("summary").and_then(Value::as_str) {
                let _ = self.store.set_subagent_summary(&subagent_id, s, &json!([]));
            }
            if let Some(qs) = c.get("questions").and_then(Value::as_array) {
                for q in qs.iter().filter_map(Value::as_str) {
                    let _ = self
                        .store
                        .insert_ask(&ulid(), Some(chat_id), Some(&subagent_id), q);
                }
            }
        }
        let _ = self
            .post_timing("subagent_turn_total_ms", total_ms, Some(harness.as_str()))
            .await;
        if let Some(fd) = first_delta {
            let _ = self
                .post_timing("subagent_ttft_ms", fd, Some(harness.as_str()))
                .await;
        }
        self.write_threads_index().await;
        // T3 index of the subagent's own session file (history_search source) + write pass, off path.
        {
            let me = self.clone();
            let sid = subagent_id.clone();
            let proposals = contract
                .as_ref()
                .and_then(|c| c.get("proposed_memory"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let summary_text = contract
                .as_ref()
                .and_then(|c| c.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let text2 = text.clone();
            let chat = chat_id.to_owned();
            tokio::spawn(async move {
                me.index_session_history(&sid, harness).await;
                let body = if summary_text.is_empty() {
                    text2
                } else {
                    summary_text
                };
                let _ = me
                    .memory
                    .write_pass(
                        &format!("subagent:{sid}"),
                        &sid,
                        "subagent",
                        &body,
                        &proposals,
                    )
                    .await;
                let _ = me.memory.maybe_compact(&chat).await;
                let _ = me.write_system_prompt();
            });
        }
        Ok(())
    }

    /// Tail the subagent's session JSONL into T3 (pre-compaction entries included).
    pub async fn index_session_history(&self, session_id: &str, harness: Harness) {
        let adapter = self.adapter(harness);
        let cursor = self.store.session_cursor(session_id).ok().flatten();
        match adapter.read_history(session_id, cursor).await {
            Ok((entries, next)) => {
                for e in &entries {
                    if let Some((role, text)) = crate::harness::entry_text(e) {
                        let _ = self.store.t3_append(
                            session_id,
                            Some(session_id),
                            Some(session_id),
                            "session_entry",
                            Some(&role),
                            &text,
                            Some(&json!({ "entry_id": e.get("id") })),
                        );
                    }
                }
                if let Some(c) = next {
                    let _ = self.store.set_session_cursor(session_id, &c);
                }
            }
            Err(e) => tracing::debug!(error = %e, session_id, "history index"),
        }
    }

    /// `threads` (T1 index) + `status` in the project doc.
    pub async fn write_threads_index(&self) {
        let Some(h) = self.sync.host(arbos_core::wire::PROJECT_DOC_CHAT_ID).await else {
            return;
        };
        let h = h.lock().await;
        let threads = h.doc.doc.get_map("threads");
        for s in self.store.subagents().unwrap_or_default() {
            let v = json!({ "name": s.name, "purpose": s.purpose, "summary": s.summary, "tags": s.tags, "status": s.status, "lastTurnAt": s.last_turn_at, "harness": s.harness });
            let lv: loro::LoroValue = serde_json::from_value(v).unwrap_or(loro::LoroValue::Null);
            threads.insert(&s.id, lv).ok();
        }
        h.doc.doc.commit();
    }

    /// MCP `spawn_subagent` / `wake_subagent` from inside an act turn (D66 escalation).
    pub async fn spawn_from_tool(
        self: &Arc<Self>,
        existing: Option<String>,
        name: &str,
        purpose: &str,
        brief: &str,
        isolate: bool,
    ) -> Result<String> {
        let _ = isolate;
        let (chat_id, user_msg) = {
            let t = self.turn.lock().await;
            match t.as_ref() {
                Some(t) => (t.chat_id.clone(), t.user_msg_id.clone()),
                None => (
                    self.store
                        .chats()?
                        .first()
                        .map(|c| c.id.clone())
                        .unwrap_or_default(),
                    String::new(),
                ),
            }
        };
        let host = self
            .sync
            .host(&chat_id)
            .await
            .ok_or_else(|| anyhow!("chat room not open"))?;
        if let Some(t) = self.turn.lock().await.as_mut() {
            t.escalate = false;
            let mut h = host.lock().await;
            h.doc.add_status_part(
                &t.assistant_msg_id,
                "handed_off",
                &format!("Handed to subagent {name}"),
            );
        }
        let me = self.clone();
        let (n, p, b) = (name.to_owned(), purpose.to_owned(), brief.to_owned());
        let id_holder = Arc::new(Mutex::new(String::new()));
        let idh = id_holder.clone();
        tokio::spawn(async move {
            let _ = idh;
            if let Err(e) = me
                .run_subagent(
                    &chat_id, &host, &user_msg, existing, &n, &p, &b, None, false, true,
                )
                .await
            {
                tracing::warn!(error = %e, "spawn_from_tool");
            }
        });
        Ok("subagent started; its result will be posted in this chat".to_owned())
    }

    /// Stop every child in order (subagents, then pi) so session files and kernel snapshots land.
    pub async fn shutdown(&self) {
        for a in self.adapters.values() {
            a.shutdown_all().await;
        }
        if let Some(pi) = self.pi.lock().await.take() {
            pi.shutdown().await;
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.pi_ready.load(Ordering::SeqCst)
    }
}

pub fn route_name(r: &ValidRoute) -> &'static str {
    match r {
        ValidRoute::Reply { .. } => "reply",
        ValidRoute::Act { .. } => "act",
        ValidRoute::Wake { .. } => "wake",
        ValidRoute::New { .. } => "new",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Routing rule: the inline `reply` streams out of partial JSON exactly as written.
    #[test]
    fn reply_extractor_streams_partial_json() {
        let mut ex = ReplyExtractor::default();
        let mut out = String::new();
        for d in [
            "{\"action\": \"reply",
            "\", \"reply\": \"Hi ",
            "there\\n\\\"you\\\"",
            " — ok\"}",
        ] {
            out.push_str(&ex.feed(d));
        }
        assert_eq!(out, "Hi there\n\"you\" — ok");
        let mut ex2 = ReplyExtractor::default();
        assert_eq!(
            ex2.feed("{\"action\":\"act\",\"plan\":\"x\",\"reply\":null}"),
            ""
        );
    }
}
