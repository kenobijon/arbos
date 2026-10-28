//! The waker: turns wake candidates into at most one small nudge per debounce window and delivers
//! it through the configured plugin (spec §4, brakes from §12).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::config::{expand_home, read_secret_line, WakerConfig, WakerKind};
use crate::mirror::{now_ms, Mirror, WakeCandidate};
use crate::redact;

/// How the nudge leaves the sidecar.
#[derive(Clone)]
pub enum Plugin {
    Webhook {
        url_file: Option<PathBuf>,
        auth_file: Option<PathBuf>,
        url: Option<String>,
    },
    Command {
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        timeout: Duration,
        env_allow: Vec<String>,
    },
    File {
        path: PathBuf,
    },
    /// Tests and `--waker none`: nudges go nowhere (or into a channel).
    Channel(Option<mpsc::UnboundedSender<Value>>),
}

impl Plugin {
    pub fn from_config(cfg: &WakerConfig, default_file: PathBuf) -> Result<Self> {
        Ok(match cfg.kind {
            WakerKind::Webhook => {
                if cfg.url_file.is_none() && cfg.url.is_none() {
                    bail!("webhook waker needs url_file (or url)");
                }
                Self::Webhook {
                    url_file: cfg.url_file.as_deref().map(expand_home),
                    auth_file: cfg.auth_file.as_deref().map(expand_home),
                    url: cfg.url.clone(),
                }
            }
            WakerKind::Command => {
                let argv = cfg.command.clone().unwrap_or_default();
                if argv.is_empty() {
                    bail!("command waker needs a command");
                }
                Self::Command {
                    argv,
                    cwd: cfg.cwd.as_deref().map(expand_home),
                    timeout: Duration::from_secs(cfg.timeout_s.max(1)),
                    env_allow: cfg
                        .env_allow
                        .iter()
                        .filter(|k| !k.starts_with("ARBOS"))
                        .cloned()
                        .collect(),
                }
            }
            WakerKind::File => Self::File {
                path: cfg.path.as_deref().map(expand_home).unwrap_or(default_file),
            },
            WakerKind::None => Self::Channel(None),
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Webhook { .. } => "webhook",
            Self::Command { .. } => "command",
            Self::File { .. } => "file",
            Self::Channel(_) => "none",
        }
    }

    /// Deliver one nudge. `Ok(status)` = delivered; `Err` = not delivered.
    pub async fn deliver(&self, nudge: &Value) -> Result<String> {
        match self {
            Self::Webhook {
                url_file,
                auth_file,
                url,
            } => {
                let url = match (url_file, url) {
                    (Some(f), _) => read_secret_line(f)?,
                    (None, Some(u)) => u.clone(),
                    (None, None) => bail!("no webhook url"),
                };
                redact::protect(&url);
                let auth = match auth_file {
                    Some(f) => Some(read_secret_line(f)?),
                    None => None,
                };
                if !(url.starts_with("https://")
                    || url.starts_with("http://127.0.0.1")
                    || url.starts_with("http://localhost"))
                {
                    bail!("webhook url must be https:// (http only for localhost)");
                }
                let body = serde_json::to_vec(nudge)?;
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()?;
                let mut req = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("X-Arbos-Agent", nudge["agent"]["id"].as_str().unwrap_or(""));
                if let Some(a) = &auth {
                    redact::protect(a);
                    req = req
                        .header("Authorization", a)
                        .header("X-Arbos-Signature", format!("sha256={}", sign(&body, a)));
                }
                let resp = req.body(body).send().await.context("webhook")?;
                let status = resp.status();
                if status.is_success() {
                    Ok(status.as_u16().to_string())
                } else {
                    Err(anyhow!("webhook answered {status}"))
                }
            }
            Self::Command {
                argv,
                cwd,
                timeout,
                env_allow,
            } => {
                let json = serde_json::to_string(nudge)?;
                let prompt = prompt_text(nudge);
                let chat = nudge["chats"][0]["id"].as_str().unwrap_or("").to_owned();
                let fill = |s: &str| {
                    s.replace("{json}", &json)
                        .replace("{prompt}", &prompt)
                        .replace("{chat}", &chat)
                };
                let mut cmd = tokio::process::Command::new(fill(&argv[0]));
                for a in &argv[1..] {
                    cmd.arg(fill(a));
                }
                cmd.env_clear();
                for k in env_allow {
                    if k.starts_with("ARBOS") {
                        continue;
                    }
                    if let Ok(v) = std::env::var(k) {
                        cmd.env(k, v);
                    }
                }
                cmd.env("ARBOS_SIDECAR_WAKE", "1");
                if let Some(d) = cwd {
                    cmd.current_dir(d);
                }
                cmd.stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .process_group(0)
                    .kill_on_drop(true);
                let mut child = cmd.spawn().context("starting the waker command")?;
                let pid = child.id().map(|p| p as i32);
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(json.as_bytes()).await;
                    let _ = stdin.shutdown().await;
                }
                match tokio::time::timeout(*timeout, child.wait_with_output()).await {
                    Ok(Ok(out)) => {
                        let tail = |b: &[u8]| {
                            let s = String::from_utf8_lossy(b);
                            let s = s.trim();
                            s.chars()
                                .rev()
                                .take(4096)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect::<String>()
                        };
                        if !out.stdout.is_empty() {
                            tracing::info!(waker = "command", stdout = %tail(&out.stdout));
                        }
                        if !out.stderr.is_empty() {
                            tracing::info!(waker = "command", stderr = %tail(&out.stderr));
                        }
                        if out.status.success() {
                            Ok("exit 0".into())
                        } else {
                            Err(anyhow!("command exited with {}", out.status))
                        }
                    }
                    Ok(Err(e)) => Err(anyhow!("command failed: {e}")),
                    Err(_) => {
                        if let Some(pid) = pid {
                            let _ = nix::sys::signal::killpg(
                                nix::unistd::Pid::from_raw(pid),
                                nix::sys::signal::Signal::SIGKILL,
                            );
                        }
                        Err(anyhow!("command timed out after {} s", timeout.as_secs()))
                    }
                }
            }
            Self::File { path } => {
                if let Some(d) = path.parent() {
                    std::fs::create_dir_all(d)?;
                }
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                use std::io::Write;
                f.write_all(serde_json::to_string(nudge)?.as_bytes())?;
                f.write_all(b"\n")?;
                Ok("appended".into())
            }
            Self::Channel(tx) => {
                if let Some(tx) = tx {
                    tx.send(nudge.clone())
                        .map_err(|_| anyhow!("channel closed"))?;
                }
                Ok("noop".into())
            }
        }
    }
}

/// `X-Arbos-Signature` value: hex HMAC-SHA256 of the body keyed with the Authorization value —
/// the same contract as the relay's Wake URL (#2185).
pub fn sign(body: &[u8], key: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("hmac key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// One-line text form for `{prompt}` wakers.
pub fn prompt_text(nudge: &Value) -> String {
    let mut parts = Vec::new();
    for c in nudge["chats"].as_array().into_iter().flatten() {
        let n = c["pending"].as_i64().unwrap_or(1);
        let title = c["title"].as_str().unwrap_or("");
        let kind = c["kind"].as_str().unwrap_or("chat");
        let from = c["last_from"]["name"].as_str().unwrap_or("");
        let what = if c["reason"] == "member.added" {
            format!("you were added to \"{title}\" ({kind})")
        } else {
            let label = if title.is_empty() {
                format!("a {kind}")
            } else {
                format!("\"{title}\" ({kind})")
            };
            format!(
                "{n} new message{} for you in {label}{}",
                if n == 1 { "" } else { "s" },
                if from.is_empty() {
                    String::new()
                } else {
                    format!(" — last from {from}")
                }
            )
        };
        parts.push(format!("{what} [chat {}]", c["id"].as_str().unwrap_or("")));
    }
    format!(
        "Arbos: {}. Read with the arbos_read_chat MCP tool or 'arbos-sidecar read <chat>'; reply with arbos_reply. Reply only where you are addressed; post nothing if you decide not to answer.",
        parts.join("; ")
    )
}

#[derive(Debug, Clone, Default)]
struct PendingChat {
    title: String,
    kind: String,
    count: i64,
    last_from: Value,
    last_at: i64,
    events: Vec<i64>,
    reason: &'static str,
}

/// Why a candidate was not queued for a nudge.
#[derive(Debug, PartialEq)]
pub enum Skip {
    Stale,
    AgentLoop,
    AgentCooldown,
}

const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(5)];
const RETRY_LAST: Duration = Duration::from_secs(30);
const BREAKER_WINDOW: Duration = Duration::from_secs(60);
const BREAKER_OPEN: Duration = Duration::from_secs(300);
const BREAKER_THRESHOLD: usize = 3;
const AGENT_COOLDOWN: Duration = Duration::from_secs(600);
const SLOW_INTERVAL: Duration = Duration::from_secs(300);

pub struct Engine {
    cfg: WakerConfig,
    plugin: Plugin,
    mirror: Mirror,
    agent: Value,
    pending: BTreeMap<String, PendingChat>,
    window_deadline: Option<Instant>,
    sent: VecDeque<Instant>,
    failures: VecDeque<Instant>,
    open_until: Option<Instant>,
    half_open: bool,
    agent_sender_last: HashMap<String, Instant>,
}

impl Engine {
    pub fn new(cfg: WakerConfig, plugin: Plugin, mirror: Mirror, agent: Value) -> Self {
        Self {
            cfg,
            plugin,
            mirror,
            agent,
            pending: BTreeMap::new(),
            window_deadline: None,
            sent: VecDeque::new(),
            failures: VecDeque::new(),
            open_until: None,
            half_open: false,
            agent_sender_last: HashMap::new(),
        }
    }

    /// Queue a candidate for the next nudge, or say why not.
    pub fn offer(&mut self, c: WakeCandidate) -> std::result::Result<(), Skip> {
        let now = Instant::now();
        let age_ms = now_ms() - c.ts;
        if age_ms > (self.cfg.stale_wake_s as i64) * 1000 {
            return Err(Skip::Stale);
        }
        if c.from_agent {
            // Belt for agent↔agent ping-pong (Buzz L10): the relay already stops re-forwarding
            // (one-hop rule); here an agent-sent wake nudges at most once per chat per 10 min
            // and never when the last rows are agents talking to agents with no human between.
            let rows = self.mirror.rows(&c.chat.id);
            let tail: Vec<&Value> = rows.iter().rev().take(3).collect();
            if tail.len() == 3 && tail.iter().all(|r| r["from"]["kind"] == "agent") {
                return Err(Skip::AgentLoop);
            }
            if let Some(last) = self.agent_sender_last.get(&c.chat.id) {
                if now.duration_since(*last) < AGENT_COOLDOWN {
                    return Err(Skip::AgentCooldown);
                }
            }
            self.agent_sender_last.insert(c.chat.id.clone(), now);
        }
        let p = self.pending.entry(c.chat.id.clone()).or_default();
        if !c.chat.title.is_empty() {
            p.title = c.chat.title.clone();
        }
        if !c.chat.kind.is_empty() {
            p.kind = c.chat.kind.clone();
        }
        p.count += 1;
        p.last_from = c.from.clone();
        p.last_at = c.ts;
        p.events.push(c.event_id);
        p.reason = c.reason;
        if self.window_deadline.is_none() {
            self.window_deadline = Some(now + Duration::from_secs(self.cfg.debounce_s));
        }
        Ok(())
    }

    fn build_nudge(&self) -> Value {
        let mut chats: Vec<(&String, &PendingChat)> = self.pending.iter().collect();
        chats.sort_by_key(|a| std::cmp::Reverse(a.1.last_at));
        let events: Vec<i64> = chats.iter().flat_map(|(_, p)| p.events.clone()).collect();
        let first = chats.first().map(|(id, _)| id.as_str()).unwrap_or("");
        json!({
            "type": "wake",
            "v": 1,
            "at": now_ms(),
            "agent": self.agent,
            "chats": chats.iter().map(|(id, p)| json!({
                "id": id, "kind": p.kind, "title": p.title, "pending": p.count,
                "last_from": p.last_from, "last_at": p.last_at, "reason": p.reason,
            })).collect::<Vec<_>>(),
            "events": events,
            "read": format!("arbos-sidecar read {first}   |   MCP: arbos_read_chat {{\"chat\":\"{first}\"}}"),
        })
    }

    fn sent_last_hour(&mut self, now: Instant) -> usize {
        while let Some(t) = self.sent.front() {
            if now.duration_since(*t) > Duration::from_secs(3600) {
                self.sent.pop_front();
            } else {
                break;
            }
        }
        self.sent.len()
    }

    fn breaker_allows(&mut self, now: Instant) -> bool {
        if let Some(until) = self.open_until {
            if now < until {
                return false;
            }
            // Half-open: one probe.
            self.open_until = None;
            self.half_open = true;
        }
        true
    }

    fn record_failure(&mut self, now: Instant) {
        if self.half_open {
            self.half_open = false;
            self.open_until = Some(now + BREAKER_OPEN);
            self.failures.clear();
            return;
        }
        self.failures.push_back(now);
        while let Some(t) = self.failures.front() {
            if now.duration_since(*t) > BREAKER_WINDOW {
                self.failures.pop_front();
            } else {
                break;
            }
        }
        if self.failures.len() >= BREAKER_THRESHOLD {
            self.open_until = Some(now + BREAKER_OPEN);
            self.failures.clear();
        }
    }

    fn record(&self, nudge: &Value, ok: bool, status: &str, error: Option<&str>, attempt: u32) {
        let rec = json!({
            "at": now_ms(),
            "chats": nudge["chats"].as_array().map(|a| a.iter().map(|c| c["id"].clone()).collect::<Vec<_>>()),
            "events": nudge["events"],
            "waker": self.plugin.name(),
            "ok": ok,
            "status": status,
            "error": error.map(redact::redact),
            "attempt": attempt,
        });
        if let Err(e) = self.mirror.append_wake(&rec) {
            tracing::warn!(error = %e, "wakes.jsonl not written");
        }
    }

    /// The window closed: send one nudge for everything pending, with retries.
    async fn fire(&mut self) {
        let now = Instant::now();
        self.window_deadline = None;
        if self.pending.is_empty() {
            return;
        }
        // Rate cap (Buzz L16): past the hourly cap, one nudge per 5 min.
        let n = self.sent_last_hour(now);
        if n as u32 >= self.cfg.max_nudges_per_hour {
            if let Some(last) = self.sent.back() {
                let since = now.duration_since(*last);
                if since < SLOW_INTERVAL {
                    tracing::warn!(sent_last_hour = n, "nudge rate cap reached — slowing down");
                    self.window_deadline = Some(*last + SLOW_INTERVAL);
                    return;
                }
            }
        }
        let nudge = self.build_nudge();
        if !self.breaker_allows(now) {
            self.record(&nudge, false, "skipped", Some("circuit_open"), 0);
            // Keep pending; try again when the circuit half-opens.
            self.window_deadline = self.open_until;
            return;
        }
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.plugin.deliver(&nudge).await {
                Ok(status) => {
                    self.record(&nudge, true, &status, None, attempt);
                    self.sent.push_back(Instant::now());
                    self.half_open = false;
                    self.failures.clear();
                    self.pending.clear();
                    tracing::info!(attempt, status = %status, "nudge delivered");
                    return;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    tracing::warn!(attempt, error = %redact::redact(&msg), "nudge not delivered");
                    if attempt <= RETRY_DELAYS.len() as u32 {
                        tokio::time::sleep(RETRY_DELAYS[attempt as usize - 1]).await;
                        continue;
                    }
                    if attempt == RETRY_DELAYS.len() as u32 + 1 {
                        tokio::time::sleep(RETRY_LAST).await;
                        continue;
                    }
                    self.record(&nudge, false, "failed", Some(&msg), attempt);
                    self.record_failure(Instant::now());
                    // Pending stays; the next event re-arms the window.
                    return;
                }
            }
        }
    }

    /// Drive the engine until the candidate channel closes.
    pub async fn run(mut self, mut rx: mpsc::UnboundedReceiver<WakeCandidate>) {
        loop {
            let deadline = self.window_deadline;
            tokio::select! {
                c = rx.recv() => match c {
                    Some(c) => {
                        if let Err(skip) = self.offer(c) {
                            tracing::debug!(?skip, "wake candidate skipped");
                        }
                    }
                    None => {
                        if !self.pending.is_empty() { self.fire().await; }
                        return;
                    }
                },
                _ = async { match deadline { Some(d) => tokio::time::sleep_until(d).await, None => std::future::pending().await } } => {
                    self.fire().await;
                }
            }
        }
    }

    /// `arbos-sidecar test-wake`: one nudge now, no debounce, no retry.
    pub async fn test_wake(&self) -> Result<String> {
        let nudge = json!({
            "type": "wake.test", "v": 1, "at": now_ms(), "agent": self.agent,
            "chats": [], "events": [],
            "read": "this is a test nudge from arbos-sidecar test-wake",
        });
        let r = self.plugin.deliver(&nudge).await;
        match &r {
            Ok(s) => self.record(&nudge, true, s, None, 1),
            Err(e) => self.record(&nudge, false, "failed", Some(&format!("{e:#}")), 1),
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::tests::{received, temp_mirror};
    use crate::mirror::ChatMeta;

    fn cand(chat: &str, event_id: i64, from_agent: bool) -> WakeCandidate {
        WakeCandidate {
            chat: ChatMeta {
                id: chat.into(),
                kind: "group".into(),
                title: "Launch".into(),
                ..Default::default()
            },
            event_id,
            ts: now_ms(),
            from: json!({ "kind": if from_agent { "agent" } else { "user" }, "name": "Jacob" }),
            from_agent,
            reason: "message",
        }
    }

    fn engine(cfg: WakerConfig) -> (tempfile::TempDir, Engine, mpsc::UnboundedReceiver<Value>) {
        let (dir, m) = temp_mirror();
        let (tx, rx) = mpsc::unbounded_channel();
        let e = Engine::new(
            cfg,
            Plugin::Channel(Some(tx)),
            m,
            json!({ "id": "a1", "name": "Bot", "handle": "bot" }),
        );
        (dir, e, rx)
    }

    /// Buzz L4/L5: many events in one window → one nudge listing every chat and count.
    #[tokio::test(start_paused = true)]
    async fn catch_up_after_offline_is_one_nudge() {
        let (_d, e, mut out) = engine(WakerConfig::default());
        let (tx, rx) = mpsc::unbounded_channel();
        let h = tokio::spawn(e.run(rx));
        for i in 0..1000 {
            tx.send(cand(if i % 2 == 0 { "c1" } else { "c2" }, i, false))
                .unwrap();
        }
        tokio::time::sleep(Duration::from_secs(6)).await;
        let n = out.recv().await.unwrap();
        assert_eq!(n["chats"].as_array().unwrap().len(), 2);
        assert_eq!(n["chats"][0]["pending"], 500);
        assert_eq!(n["events"].as_array().unwrap().len(), 1000);
        assert!(n["read"].as_str().unwrap().contains("arbos_read_chat"));
        assert!(out.try_recv().is_err());
        drop(tx);
        h.await.unwrap();
    }

    /// Buzz L4: a stale row never nudges.
    #[test]
    fn stale_candidates_are_skipped() {
        let (_d, mut e, _rx) = engine(WakerConfig::default());
        let mut c = cand("c1", 1, false);
        c.ts = now_ms() - 7 * 3600 * 1000;
        assert_eq!(e.offer(c), Err(Skip::Stale));
        assert!(e.pending.is_empty());
    }

    /// Buzz L8: nothing arrives → nothing is sent, however long we wait.
    #[tokio::test(start_paused = true)]
    async fn idle_sidecar_sends_no_nudge() {
        let (_d, e, mut out) = engine(WakerConfig::default());
        let (tx, rx) = mpsc::unbounded_channel::<WakeCandidate>();
        let h = tokio::spawn(e.run(rx));
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert!(out.try_recv().is_err());
        drop(tx);
        h.await.unwrap();
    }

    /// Buzz L10: agent-sent wakes nudge once per chat per 10 min and never for an all-agent tail.
    #[tokio::test(start_paused = true)]
    async fn agent_to_agent_pingpong_stops_after_one_hop() {
        let (_d, mut e, _rx) = engine(WakerConfig::default());
        assert!(e.offer(cand("c1", 1, true)).is_ok());
        assert_eq!(e.offer(cand("c1", 2, true)), Err(Skip::AgentCooldown));
        tokio::time::sleep(Duration::from_secs(601)).await;
        assert!(e.offer(cand("c1", 3, true)).is_ok());
        // Three agent rows in a row in the mirror → no nudge even after the cooldown.
        for i in 0..3 {
            let mut ev = received(10 + i, "c2", &format!("m{i}"), "ping", true);
            ev["message"]["from"] = json!({ "kind": "agent", "id": "other", "name": "Other bot" });
            e.mirror.apply_event(&ev).unwrap();
        }
        assert_eq!(e.offer(cand("c2", 20, true)), Err(Skip::AgentLoop));
        // A human breaks the run.
        e.mirror
            .apply_event(&received(30, "c2", "m30", "hey bot", true))
            .unwrap();
        assert!(e.offer(cand("c2", 30, true)).is_ok());
    }

    /// Buzz L5: events arriving while a nudge is being delivered are not lost; they ride the
    /// next window.
    #[tokio::test(start_paused = true)]
    async fn events_during_in_flight_nudge_join_next_window() {
        let (_d, e, mut out) = engine(WakerConfig {
            debounce_s: 5,
            ..Default::default()
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let h = tokio::spawn(e.run(rx));
        tx.send(cand("c1", 1, false)).unwrap();
        tokio::time::sleep(Duration::from_secs(6)).await;
        let first = out.recv().await.unwrap();
        assert_eq!(first["chats"][0]["pending"], 1);
        tx.send(cand("c1", 2, false)).unwrap();
        tx.send(cand("c3", 3, false)).unwrap();
        tokio::time::sleep(Duration::from_secs(6)).await;
        let second = out.recv().await.unwrap();
        assert_eq!(second["chats"].as_array().unwrap().len(), 2);
        assert_eq!(second["events"].as_array().unwrap().len(), 2);
        drop(tx);
        h.await.unwrap();
    }

    /// Buzz L16: past the hourly cap, nudges coalesce to one per 5 min; nothing is lost.
    #[tokio::test(start_paused = true)]
    async fn nudge_rate_cap_coalesces() {
        let (_d, e, mut out) = engine(WakerConfig {
            max_nudges_per_hour: 2,
            debounce_s: 1,
            ..Default::default()
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let h = tokio::spawn(e.run(rx));
        for i in 0..2 {
            tx.send(cand("c1", i, false)).unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
            out.recv().await.unwrap();
        }
        tx.send(cand("c1", 10, false)).unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(out.try_recv().is_err(), "capped");
        tx.send(cand("c1", 11, false)).unwrap();
        tokio::time::sleep(Duration::from_secs(300)).await;
        let n = out.recv().await.unwrap();
        assert_eq!(n["events"].as_array().unwrap().len(), 2);
        drop(tx);
        h.await.unwrap();
    }

    /// Buzz L7: three attempts (1 s, 5 s, 30 s) then the failure is recorded and the next event
    /// re-arms the window.
    #[tokio::test(start_paused = true)]
    async fn waker_retries_three_times_then_rearms() {
        let (dir, m) = temp_mirror();
        let bad = Plugin::Webhook {
            url_file: None,
            auth_file: None,
            url: Some("http://127.0.0.1:9/never".into()),
        };
        let e = Engine::new(
            WakerConfig {
                debounce_s: 1,
                ..Default::default()
            },
            bad,
            m.clone(),
            json!({ "id": "a1" }),
        );
        let (tx, rx) = mpsc::unbounded_channel();
        let h = tokio::spawn(e.run(rx));
        tx.send(cand("c1", 1, false)).unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        let last = m.last_wake().unwrap();
        assert_eq!(last["ok"], false);
        assert_eq!(last["attempt"], 4);
        assert_eq!(last["status"], "failed");
        drop(tx);
        h.await.unwrap();
        drop(dir);
    }

    /// Buzz L17: three failed nudges in a minute open the circuit for five minutes; one probe
    /// after that.
    #[tokio::test(start_paused = true)]
    async fn command_waker_circuit_breaker() {
        let (_d, m) = temp_mirror();
        let mut e = Engine::new(
            WakerConfig::default(),
            Plugin::Channel(None),
            m,
            json!({ "id": "a1" }),
        );
        let now = Instant::now();
        assert!(e.breaker_allows(now));
        for _ in 0..3 {
            e.record_failure(now);
        }
        assert!(!e.breaker_allows(now + Duration::from_secs(10)));
        assert!(e.breaker_allows(now + BREAKER_OPEN + Duration::from_secs(1)));
        assert!(e.half_open);
        e.record_failure(now + BREAKER_OPEN + Duration::from_secs(2));
        assert!(!e.breaker_allows(now + BREAKER_OPEN + Duration::from_secs(3)));
    }

    /// Buzz L6: a hung command waker is killed after `timeout_s` with its process group and the
    /// next event nudges again.
    #[tokio::test]
    async fn stuck_command_waker_is_killed_and_rearmed() {
        let p = Plugin::Command {
            argv: vec!["sh".into(), "-c".into(), "sleep 30; exit 0".into()],
            cwd: None,
            timeout: Duration::from_secs(1),
            env_allow: vec!["PATH".into()],
        };
        let t = std::time::Instant::now();
        let r = p.deliver(&json!({ "chats": [], "agent": {} })).await;
        assert!(r.is_err());
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(format!("{:#}", r.unwrap_err()).contains("timed out"));
    }

    /// Buzz L13: the key is never on argv and never in the child's environment.
    #[tokio::test]
    async fn key_never_on_argv_or_in_child_env() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("env.txt");
        std::env::set_var("ARBOS_AGENT_KEY", format!("arbk_{}", "d".repeat(40)));
        std::env::set_var("ARBOS_SIDECAR_HOME", dir.path());
        let p = Plugin::Command {
            argv: vec![
                "sh".into(),
                "-c".into(),
                format!("env > {}; echo \"$@\" >> {}", out.display(), out.display()),
                "_".into(),
                "{prompt}".into(),
            ],
            cwd: None,
            timeout: Duration::from_secs(10),
            env_allow: vec!["PATH".into(), "ARBOS_AGENT_KEY".into()],
        };
        p.deliver(&json!({ "chats": [{ "id": "c1", "title": "T", "kind": "group", "pending": 1 }], "agent": { "id": "a1" } }))
            .await
            .unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(!text.contains("arbk_"), "{text}");
        assert!(!text.contains("ARBOS_AGENT_KEY"), "{text}");
        assert!(!text.contains("ARBOS_SIDECAR_HOME"), "{text}");
        assert!(text.contains("ARBOS_SIDECAR_WAKE=1"));
        assert!(text.contains("Arbos: 1 new message"));
        std::env::remove_var("ARBOS_AGENT_KEY");
        std::env::remove_var("ARBOS_SIDECAR_HOME");
    }

    /// The webhook signature matches the relay's Wake URL contract (#2185).
    #[test]
    fn signature_is_hmac_sha256_hex() {
        let s = sign(b"{}", "secret");
        assert_eq!(s.len(), 64);
        assert_eq!(s, sign(b"{}", "secret"));
        assert_ne!(s, sign(b"{}", "other"));
    }

    /// Buzz L11: chat text is data; "!shutdown" is mirrored, never obeyed.
    #[test]
    fn chat_text_is_never_a_command() {
        let (_d, m) = temp_mirror();
        let w = m
            .apply_event(&received(1, "c1", "m1", "!shutdown", true))
            .unwrap();
        assert!(w.is_some());
        assert_eq!(m.rows("c1")[0]["text"], "!shutdown");
    }
}
