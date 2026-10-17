//! D82: the first sign-in on a fresh account. Instead of "Loading your agents…"
//! the user watches a terminal type while the relay creates their computer
//! (`GET /machine` waking → awake), the first agent ("Home": project +
//! workspace) and its first chat (room sync + the agent's greeting). The
//! terminal then morphs into that greeting bubble and the live chat takes
//! over. Every line is driven by relay facts, never by a timer. Rendering:
//! `ui/onboarding.rs`.
//!
//! Stages (internal/onboarding-lottie-implementation.md):
//!   Computer    — no project yet, or the machine is waking / asleep
//!   Agent       — machine awake, project exists, its workspace not running yet
//!   AlmostReady — workspace running; chat syncing, greeting being written
//!   Finishing   — the 1.5 s morph into the greeting bubble (loop-boundary aligned)
//!   Failed      — machine error, or more than 3 minutes elapsed

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::Context;

use crate::app::AppState;
use crate::model::{Id, MachineState};
use crate::sync::ClientCommand;

pub const LOOP_MS: u64 = 4_000;
pub const FINISH_MS: u64 = 1_500;
const MIN_STAGE_HOLD: Duration = Duration::from_millis(1_200);
const TIMEOUT: Duration = Duration::from_secs(180);
const SLOW_AFTER: Duration = Duration::from_secs(45);
/// Boxes on an image older than the workspace report never say "running":
/// after this long awake with a synced chat, the chat is the signal.
const WORKSPACE_FALLBACK: Duration = Duration::from_secs(25);
pub const AGENT_NAME: &str = "Home";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Computer,
    Agent,
    AlmostReady,
    Finishing,
    Failed,
}

impl Stage {
    fn rank(self) -> u8 {
        match self {
            Stage::Computer => 0,
            Stage::Agent => 1,
            Stage::AlmostReady => 2,
            Stage::Finishing => 3,
            Stage::Failed => 4,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Stage::Computer => "Creating your computer",
            Stage::Agent => "Setting up your first agent",
            Stage::AlmostReady | Stage::Finishing => "Almost ready",
            Stage::Failed => "Can’t reach your computer",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Onboarding {
    pub stage: Stage,
    pub started_at: Instant,
    pub stage_since: Instant,
    /// Second line under the hero: a fact from the relay.
    pub detail: Option<String>,
    /// S14 quiet block text while `Failed`.
    pub error: Option<String>,
    pub project_id: Option<Id>,
    pub chat_id: Option<Id>,
    pub agent_name: String,
    pub finish_started_at: Option<Instant>,
    /// The first registry answer arrived (an empty list really means "new account").
    pub registry_seen: bool,
    pub greeting_written: bool,
    pub greeting_requested: bool,
    pub awake_since: Option<Instant>,
    pub synced_chats: HashSet<Id>,
    pub workspaces_running: HashSet<Id>,
    /// macOS "Reduce motion": static panel, plain cross-fade.
    pub reduce_motion: bool,
    /// Fixture / rig: frozen on one stage.
    pub demo: bool,
}

impl Onboarding {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            stage: Stage::Computer,
            started_at: now,
            stage_since: now,
            detail: None,
            error: None,
            project_id: None,
            chat_id: None,
            agent_name: AGENT_NAME.to_string(),
            finish_started_at: None,
            registry_seen: false,
            greeting_written: false,
            greeting_requested: false,
            awake_since: None,
            synced_chats: HashSet::new(),
            workspaces_running: HashSet::new(),
            reduce_motion: reduce_motion_enabled(),
            demo: false,
        }
    }

    /// Fixture hook: `ARBOS_SHOW=onboarding` + `ARBOS_ONBOARDING_STAGE=1|2|3|fail`.
    pub fn demo(stage: &str) -> Self {
        let mut o = Self::new();
        o.demo = true;
        match stage {
            "2" => {
                o.stage = Stage::Agent;
                o.detail = Some("Starting Home’s workspace · arbos-agent".into());
            }
            "3" => {
                o.stage = Stage::AlmostReady;
                o.detail = Some("Syncing your first chat".into());
            }
            "fail" => {
                o.stage = Stage::Failed;
                o.error = Some("The relay could not start your computer.".into());
            }
            _ => {
                o.detail = Some("Starting a 4-core machine with 8 GB of memory".into());
            }
        }
        o
    }

    /// Seconds into the terminal loop.
    pub fn loop_t(&self, now: Instant) -> f32 {
        (now.duration_since(self.started_at).as_millis() % LOOP_MS as u128) as f32 / 1000.0
    }

    /// 0…1 through the morph, None before it starts.
    pub fn finish_progress(&self, now: Instant) -> Option<f32> {
        let t0 = self.finish_started_at?;
        Some((now.duration_since(t0).as_millis() as f32 / FINISH_MS as f32).clamp(0.0, 1.0))
    }

    pub fn finished(&self, now: Instant) -> bool {
        self.finish_progress(now).is_some_and(|p| p >= 1.0)
    }
}

impl Default for Onboarding {
    fn default() -> Self {
        Self::new()
    }
}

/// macOS Accessibility → Display → Reduce motion, read once per onboarding.
fn reduce_motion_enabled() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "com.apple.universalaccess", "reduceMotion"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(false)
}

impl AppState {
    /// A fresh sign-in on this Mac: show the first-login screen until the
    /// registry proves the account already has agents.
    pub fn begin_onboarding(&mut self, cx: &mut Context<Self>) {
        if self.onboarding.is_some() || std::env::var_os("ARBOS_FIXTURE").is_some() {
            return;
        }
        self.onboarding = Some(Onboarding::new());
        // Always-on (D73): the relay starts the machine at sign-in; asking again is
        // a no-op there and covers relays (and test accounts) that did not.
        self.command(ClientCommand::MachineStart);
        self.spawn_onboarding_timer(cx);
    }

    fn spawn_onboarding_timer(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut n: u32 = 0;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let alive = this
                    .update(cx, |s, cx| {
                        if s.onboarding.is_none() {
                            return false;
                        }
                        if n % 3 == 0
                            && s.onboarding
                                .as_ref()
                                .is_some_and(|o| !o.demo && o.stage.rank() < 3)
                        {
                            s.command(ClientCommand::PollNow);
                        }
                        s.onboarding_tick(cx);
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
                n = n.wrapping_add(1);
            }
        })
        .detach();
    }

    pub fn end_onboarding(&mut self, reason: &str, cx: &mut Context<Self>) {
        eprintln!("first login: end ({reason})");
        self.onboarding = None;
        cx.notify();
    }

    /// Retry from the S14 block: ask the relay to (re)start the machine and
    /// run the stages again from the top.
    pub fn retry_onboarding(&mut self, cx: &mut Context<Self>) {
        if let Some(o) = self.onboarding.as_mut() {
            let now = Instant::now();
            o.stage = Stage::Computer;
            o.stage_since = now;
            o.started_at = now;
            o.error = None;
            o.detail = None;
            o.awake_since = None;
        }
        self.command(ClientCommand::MachineStart);
        self.command(ClientCommand::PollNow);
        cx.notify();
    }

    pub fn onboarding_registry_seen(&mut self) {
        if let Some(o) = self.onboarding.as_mut() {
            o.registry_seen = true;
        }
    }

    pub fn onboarding_chat_synced(&mut self, chat_id: &str) {
        if let Some(o) = self.onboarding.as_mut() {
            o.synced_chats.insert(chat_id.to_string());
        }
    }

    pub fn onboarding_workspace(&mut self, project_id: &str, running: bool) {
        if let Some(o) = self.onboarding.as_mut() {
            if running {
                o.workspaces_running.insert(project_id.to_string());
            } else {
                o.workspaces_running.remove(project_id);
            }
        }
    }

    pub fn onboarding_greeting_written(&mut self, chat_id: &str) {
        if let Some(o) = self.onboarding.as_mut() {
            if o.chat_id.as_deref() == Some(chat_id) {
                o.greeting_written = true;
            }
        }
    }

    /// One evaluation of the stage machine against the current model.
    pub fn onboarding_tick(&mut self, cx: &mut Context<Self>) {
        let Some(mut o) = self.onboarding.clone() else {
            return;
        };
        if o.demo {
            return;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(o.started_at);

        // The account already has agents other than the one we made: not a first login.
        if o.registry_seen
            && o.project_id.is_none()
            && self
                .projects
                .iter()
                .any(|p| !p.shared && !p.is_people() && p.name != AGENT_NAME)
        {
            let names: Vec<&str> = self.projects.iter().map(|p| p.name.as_str()).collect();
            self.end_onboarding(&format!("account already has agents: {names:?}"), cx);
            return;
        }

        if o.stage == Stage::Finishing {
            if o.finished(now) {
                self.end_onboarding("finished", cx);
            } else {
                cx.notify();
            }
            return;
        }
        if o.stage == Stage::Failed {
            return;
        }

        if self.machine.state == MachineState::Error {
            o.stage = Stage::Failed;
            o.error = Some(
                self.machine
                    .detail
                    .clone()
                    .unwrap_or_else(|| "The relay could not start your computer.".into()),
            );
            o.stage_since = now;
            self.onboarding = Some(o);
            cx.notify();
            return;
        }
        if elapsed > TIMEOUT {
            o.stage = Stage::Failed;
            o.error = Some("Your computer is taking longer than expected.".into());
            o.stage_since = now;
            self.onboarding = Some(o);
            cx.notify();
            return;
        }

        // The first project: `SyncEvent::Paired` already asked for "Home" when
        // the list came back empty; adopt whichever own project shows up.
        if o.project_id.is_none() {
            if let Some(p) = self.projects.iter().find(|p| !p.shared && !p.is_people()) {
                o.project_id = Some(p.id.clone());
                o.agent_name = p.name.clone();
            }
        }
        if let Some(pid) = o.project_id.clone() {
            if o.chat_id.is_none() {
                o.chat_id = self
                    .primary_chat(&pid)
                    .map(|c| c.id.clone())
                    .or_else(|| self.active_tab.get(&pid).cloned());
            }
        }

        let machine_awake = self.machine.state == MachineState::Awake;
        if machine_awake {
            if o.awake_since.is_none() {
                o.awake_since = Some(now);
            }
        } else {
            o.awake_since = None;
        }
        let have_project = o.project_id.is_some();
        let workspace_running = o
            .project_id
            .as_ref()
            .is_some_and(|p| o.workspaces_running.contains(p));
        let workspace_fallback = o
            .awake_since
            .is_some_and(|t| now.duration_since(t) > WORKSPACE_FALLBACK);
        let chat_synced = o
            .chat_id
            .as_ref()
            .is_some_and(|c| o.synced_chats.contains(c));

        if chat_synced && !o.greeting_written {
            if let Some(chat_id) = o.chat_id.clone() {
                if self.messages_of(&chat_id).iter().any(|m| m.greeting) {
                    o.greeting_written = true;
                } else if !o.greeting_requested {
                    o.greeting_requested = true;
                    self.command(ClientCommand::Greeting {
                        chat_id,
                        project_id: o.project_id.clone().unwrap_or_default(),
                        agent_name: o.agent_name.clone(),
                    });
                }
            }
        }

        let target = if !machine_awake || !have_project {
            Stage::Computer
        } else if !(workspace_running || (workspace_fallback && chat_synced)) {
            Stage::Agent
        } else {
            Stage::AlmostReady
        };
        let ready = target == Stage::AlmostReady && chat_synced && o.greeting_written;

        o.detail = match target {
            Stage::Computer => {
                let reported = self.machine.spec.is_some() || self.machine.since.is_some();
                let base = match (reported, self.machine.cpu, self.machine.mem_gib) {
                    (false, _, _) => "Connecting to the relay".to_string(),
                    (true, Some(cpu), Some(mem)) => {
                        format!("Starting a {cpu}-core machine with {mem} GB of memory")
                    }
                    (true, _, _) => "Starting your machine".to_string(),
                };
                Some(if elapsed > SLOW_AFTER {
                    format!("{base} · this usually takes under a minute")
                } else {
                    base
                })
            }
            Stage::Agent => Some(if workspace_running {
                format!("{} is online", o.agent_name)
            } else if let Some(img) = self.machine.image.as_deref().filter(|s| !s.is_empty()) {
                format!("Starting {}’s workspace · {img}", o.agent_name)
            } else {
                format!("Starting {}’s workspace on your computer", o.agent_name)
            }),
            Stage::AlmostReady => Some(if !chat_synced {
                "Syncing your first chat".to_string()
            } else if !o.greeting_written {
                format!("Writing {}’s first message", o.agent_name)
            } else {
                format!("Opening your chat with {}", o.agent_name)
            }),
            Stage::Finishing | Stage::Failed => o.detail.clone(),
        };

        // Stages only move forward, one at a time, each held at least 1.2 s.
        let held = now.duration_since(o.stage_since) >= MIN_STAGE_HOLD;
        if target.rank() > o.stage.rank() && held {
            o.stage = match o.stage {
                Stage::Computer => Stage::Agent,
                Stage::Agent | Stage::AlmostReady => Stage::AlmostReady,
                Stage::Finishing | Stage::Failed => o.stage,
            };
            o.stage_since = now;
            eprintln!(
                "first login: stage → {:?} ({})",
                o.stage,
                o.detail.as_deref().unwrap_or("")
            );
        }

        if ready && o.stage == Stage::AlmostReady && held {
            // Start the morph at the loop seam it was authored for (unless motion is reduced).
            if o.reduce_motion || o.loop_t(now) < 0.6 {
                o.stage = Stage::Finishing;
                o.stage_since = now;
                o.finish_started_at = Some(now);
                eprintln!("first login: finishing → chat {:?}", o.chat_id);
                if let Some(pid) = o.project_id.clone() {
                    if self.selected_project.as_ref() != Some(&pid) {
                        self.select_project(pid, cx);
                    }
                }
            }
        }
        self.onboarding = Some(o);
        cx.notify();
    }
}
