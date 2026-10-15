//! System notifications (D49).
//!
//! The [`Coalescer`] is the policy: at most one notification per Project per
//! [`WINDOW_MS`]; events that land inside the window fold into one "N updates
//! in <Project>"; nothing fires while that Project is streaming (the turn end
//! will) or while the window is focused and the chat is on screen. The
//! [`Notifier`] wraps it with the platform backend: `osascript` on macOS,
//! `notify-send` on Linux. Both are plain subprocesses with no entitlements.

use std::collections::HashMap;
use std::process::{Command, Stdio};

pub const WINDOW_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NotifyKind {
    TurnFinished,
    NeedsInput,
    /// Raised by F2 approval gates (`AppState::notify_approval_requested`).
    #[allow(dead_code)]
    ApprovalRequested,
    WakeFailed,
    SpendCapReached,
    CrossProject,
    RoutineRan,
    /// A person wrote in a people chat (D72).
    DirectMessage,
}

impl NotifyKind {
    fn title(self, project: &str) -> String {
        match self {
            NotifyKind::TurnFinished => format!("{project} · done"),
            NotifyKind::NeedsInput => format!("{project} · needs your input"),
            NotifyKind::ApprovalRequested => format!("{project} · approval requested"),
            NotifyKind::WakeFailed => format!("{project} · wake failed"),
            NotifyKind::SpendCapReached => format!("{project} · spend cap reached"),
            NotifyKind::CrossProject => format!("{project} · message from another project"),
            NotifyKind::RoutineRan => format!("{project} · routine ran"),
            NotifyKind::DirectMessage => format!("@{project}"),
        }
    }
}

/// One thing the app wants to tell the user about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotifyEvent {
    pub project_id: String,
    pub project_name: String,
    pub kind: NotifyKind,
    /// Chat title, question text, error… ≤ one line.
    pub detail: String,
}

/// What actually reaches the OS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub project_id: String,
    pub title: String,
    pub body: String,
    /// How many events this one stands for.
    pub count: usize,
}

/// Why an event may not fire right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gate {
    /// The Project has a streaming turn; hold until it finishes.
    pub streaming: bool,
    /// Window focused and this Project's chat on screen; the user sees it.
    pub visible: bool,
}

struct Pending {
    latest: NotifyEvent,
    count: usize,
}

#[derive(Default)]
pub struct Coalescer {
    last_sent: HashMap<String, u64>,
    pending: HashMap<String, Pending>,
}

impl Coalescer {
    /// Offer an event at `now_ms`. Returns a notification when it may fire
    /// immediately; otherwise it is folded into the Project's pending batch
    /// (or dropped when the user is already looking).
    pub fn offer(&mut self, event: NotifyEvent, now_ms: u64, gate: Gate) -> Option<Notification> {
        if gate.visible {
            self.pending.remove(&event.project_id);
            return None;
        }
        let project = event.project_id.clone();
        let in_window = self
            .last_sent
            .get(&project)
            .is_some_and(|last| now_ms.saturating_sub(*last) < WINDOW_MS);
        let held = self.pending.contains_key(&project);
        if gate.streaming || in_window || held {
            let slot = self.pending.entry(project).or_insert(Pending {
                latest: event.clone(),
                count: 0,
            });
            slot.latest = event;
            slot.count += 1;
            return None;
        }
        self.last_sent.insert(project.clone(), now_ms);
        Some(render(&event, 1))
    }

    /// Flush batches whose window has elapsed. `gate_of` answers for each
    /// Project; held batches stay, visible ones are dropped.
    pub fn tick(
        &mut self,
        now_ms: u64,
        mut gate_of: impl FnMut(&str) -> Gate,
    ) -> Vec<Notification> {
        let mut out = Vec::new();
        let projects: Vec<String> = self.pending.keys().cloned().collect();
        for project in projects {
            let gate = gate_of(&project);
            if gate.visible {
                self.pending.remove(&project);
                continue;
            }
            if gate.streaming {
                continue;
            }
            let elapsed = self
                .last_sent
                .get(&project)
                .map(|last| now_ms.saturating_sub(*last) >= WINDOW_MS)
                .unwrap_or(true);
            if !elapsed {
                continue;
            }
            if let Some(p) = self.pending.remove(&project) {
                self.last_sent.insert(project, now_ms);
                out.push(render(&p.latest, p.count));
            }
        }
        out
    }

    #[cfg(test)]
    pub fn pending_count(&self, project_id: &str) -> usize {
        self.pending.get(project_id).map(|p| p.count).unwrap_or(0)
    }
}

fn render(event: &NotifyEvent, count: usize) -> Notification {
    if count > 1 {
        return Notification {
            project_id: event.project_id.clone(),
            title: format!("{count} updates in {}", event.project_name),
            body: event.detail.clone(),
            count,
        };
    }
    Notification {
        project_id: event.project_id.clone(),
        title: event.kind.title(&event.project_name),
        body: event.detail.clone(),
        count: 1,
    }
}

/// Deliver through the platform's notification center.
pub fn deliver(n: &Notification) {
    if std::env::var_os("ARBOS_NOTIFY_DRY_RUN").is_some() {
        eprintln!("notify: {} — {}", n.title, n.body);
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            applescript_escape(&n.body),
            applescript_escape(&n.title)
        );
        spawn_quiet(Command::new("osascript").arg("-e").arg(script));
    }
    #[cfg(target_os = "linux")]
    {
        spawn_quiet(
            Command::new("notify-send")
                .arg("--app-name=Arbos")
                .arg("--expire-time=8000")
                .arg(&n.title)
                .arg(&n.body),
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        eprintln!("notify: {} — {}", n.title, n.body);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn spawn_quiet(cmd: &mut Command) {
    let spawned = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            // Reap in the background; the OS notifier returns at once.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => eprintln!("notify: backend unavailable: {err}"),
    }
}

#[cfg(target_os = "macos")]
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Coalescer + backend + the user's on/off switch.
pub struct Notifier {
    coalescer: Coalescer,
    pub enabled: bool,
    /// `ARBOS_NOTIFY_ALWAYS=1`: ignore the focus gate (demos, screen clips).
    always: bool,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier {
    pub fn new() -> Self {
        Self {
            coalescer: Coalescer::default(),
            enabled: true,
            always: std::env::var_os("ARBOS_NOTIFY_ALWAYS").is_some(),
        }
    }

    fn gate(&self, gate: Gate) -> Gate {
        if self.always {
            Gate {
                streaming: gate.streaming,
                visible: false,
            }
        } else {
            gate
        }
    }

    pub fn offer(&mut self, event: NotifyEvent, now_ms: u64, gate: Gate) {
        if !self.enabled {
            return;
        }
        let gate = self.gate(gate);
        if let Some(n) = self.coalescer.offer(event, now_ms, gate) {
            deliver(&n);
        }
    }

    pub fn tick(&mut self, now_ms: u64, mut gate_of: impl FnMut(&str) -> Gate) {
        if !self.enabled {
            return;
        }
        let always = self.always;
        for n in self.coalescer.tick(now_ms, |p| {
            let g = gate_of(p);
            if always {
                Gate {
                    streaming: g.streaming,
                    visible: false,
                }
            } else {
                g
            }
        }) {
            deliver(&n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: NotifyKind, detail: &str) -> NotifyEvent {
        NotifyEvent {
            project_id: "comms".into(),
            project_name: "Comms".into(),
            kind,
            detail: detail.into(),
        }
    }

    const OFF: Gate = Gate {
        streaming: false,
        visible: false,
    };

    #[test]
    fn at_most_one_per_project_per_window_and_coalesces() {
        let mut c = Coalescer::default();
        let mut fired: Vec<(u64, Notification)> = Vec::new();

        // One event per second for two minutes, plus ticks.
        for s in 0..=120u64 {
            let now = s * 1_000;
            if let Some(n) = c.offer(ev(NotifyKind::TurnFinished, &format!("turn {s}")), now, OFF) {
                fired.push((now, n));
            }
            for n in c.tick(now, |_| OFF) {
                fired.push((now, n));
            }
        }

        for pair in fired.windows(2) {
            assert!(
                pair[1].0 - pair[0].0 >= WINDOW_MS,
                "two notifications inside one window: {:?}",
                pair.iter().map(|(t, _)| t).collect::<Vec<_>>()
            );
        }
        assert_eq!(fired.len(), 3, "0 s, 60 s and 120 s");
        assert_eq!(fired[0].1.count, 1);
        assert_eq!(fired[0].1.title, "Comms · done");
        assert_eq!(fired[1].1.count, 60, "events 1..=60 fold into one");
        assert_eq!(fired[1].1.title, "60 updates in Comms");
        assert_eq!(fired[1].1.body, "turn 60");
    }

    #[test]
    fn three_updates_wording() {
        let mut c = Coalescer::default();
        assert!(c.offer(ev(NotifyKind::TurnFinished, "a"), 0, OFF).is_some());
        for (i, d) in ["b", "c", "d"].iter().enumerate() {
            assert!(c
                .offer(ev(NotifyKind::NeedsInput, d), 1_000 * (i as u64 + 1), OFF)
                .is_none());
        }
        assert_eq!(c.pending_count("comms"), 3);
        let out = c.tick(WINDOW_MS, |_| OFF);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "3 updates in Comms");
        assert_eq!(out[0].body, "d");
    }

    #[test]
    fn nothing_while_streaming_then_fires_when_done() {
        let mut c = Coalescer::default();
        let streaming = Gate {
            streaming: true,
            visible: false,
        };
        assert!(c
            .offer(ev(NotifyKind::NeedsInput, "q"), 0, streaming)
            .is_none());
        assert!(c.tick(5_000, |_| streaming).is_empty());
        let out = c.tick(6_000, |_| OFF);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Comms · needs your input");
    }

    #[test]
    fn visible_drops_instead_of_queueing() {
        let mut c = Coalescer::default();
        let visible = Gate {
            streaming: false,
            visible: true,
        };
        assert!(c
            .offer(ev(NotifyKind::TurnFinished, "x"), 0, visible)
            .is_none());
        assert_eq!(c.pending_count("comms"), 0);
        assert!(c.tick(WINDOW_MS, |_| OFF).is_empty());
    }

    #[test]
    fn projects_are_independent() {
        let mut c = Coalescer::default();
        let other = NotifyEvent {
            project_id: "arbos".into(),
            project_name: "Arbos".into(),
            kind: NotifyKind::RoutineRan,
            detail: "Morning digest".into(),
        };
        assert!(c.offer(ev(NotifyKind::TurnFinished, "a"), 0, OFF).is_some());
        assert!(c.offer(other, 1, OFF).is_some());
    }
}
