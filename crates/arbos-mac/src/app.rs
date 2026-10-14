//! `AppState`: the one shared entity. Views observe it (`cx.observe`) and
//! subscribe to the narrow [`AppEvent::TranscriptText`] for streaming appends
//! (zeron-study §4.4 step 4). It owns the sync channel; nothing here touches
//! GPUI elements.

use std::collections::HashMap;

use gpui::{App, Context, EventEmitter, Task};
use gpui_tokio::Tokio;
use tokio::sync::mpsc;

use crate::model::{
    BoxState, Chat, Effort, Id, Link, Message, MessageStatus, Part, Project, Role, StatusKind,
    new_id, now_ms,
};
use crate::sync::{ClientCommand, SyncEvent, SyncSource};

#[derive(Clone, Debug)]
pub enum AppEvent {
    /// Pure text append on a streaming message; only the transcript re-syncs.
    TranscriptText { chat_id: Id },
    /// Anything else in a chat changed.
    TranscriptChanged { chat_id: Id },
}

/// What the composer should look like for the active chat (§3.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerMode {
    /// Send arrow.
    Send,
    /// A turn is running: stop square.
    Stop,
    /// S8: the agent asked a question.
    Answer,
}

/// Per-chat UI state kept across tab switches.
#[derive(Clone, Debug, Default)]
pub struct ChatUi {
    /// S3 → S9: the composer dropped to the bottom; it never returns.
    pub composer_dropped: bool,
    /// When the drop animation started (ms since epoch).
    pub drop_started_at: Option<u64>,
}

pub struct AppState {
    pub source_name: &'static str,
    pub projects: Vec<Project>,
    pub chats: HashMap<Id, Vec<Chat>>,
    pub messages: HashMap<Id, Vec<Message>>,
    pub chat_ui: HashMap<Id, ChatUi>,
    pub selected_project: Option<Id>,
    pub active_tab: HashMap<Id, Id>,
    pub link: Link,
    pub rail_collapsed: bool,
    pub right_panel_open: bool,
    pub details_open: Option<DetailsTarget>,
    commands: mpsc::UnboundedSender<ClientCommand>,
    _pump: Task<()>,
}

/// What the Details drawer (stub) describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailsTarget {
    Project(Id),
    Message { chat_id: Id, message_id: Id },
    Blob(String),
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    pub fn new(source: Box<dyn SyncSource>, cx: &mut Context<Self>) -> Self {
        let source_name = source.name();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<SyncEvent>();
        let (command_tx, command_rx) = mpsc::unbounded_channel::<ClientCommand>();

        let run = Tokio::spawn(cx, async move {
            if let Err(err) = source.run(event_tx, command_rx).await {
                eprintln!("sync source stopped: {err:#}");
            }
        });

        let pump = cx.spawn(async move |this, cx| {
            let _run = run;
            while let Some(event) = event_rx.recv().await {
                let applied = this.update(cx, |state, cx| state.apply(event, cx));
                if applied.is_err() {
                    break;
                }
            }
        });

        Self {
            source_name,
            projects: Vec::new(),
            chats: HashMap::new(),
            messages: HashMap::new(),
            chat_ui: HashMap::new(),
            selected_project: None,
            active_tab: HashMap::new(),
            link: Link::Connecting,
            rail_collapsed: false,
            right_panel_open: false,
            details_open: None,
            commands: command_tx,
            _pump: pump,
        }
    }

    fn apply(&mut self, event: SyncEvent, cx: &mut Context<Self>) {
        match event {
            SyncEvent::Projects(projects) => {
                self.projects = projects;
                if self.selected_project.is_none() {
                    let pick = self
                        .projects
                        .iter()
                        .find(|p| p.name == "Arbos")
                        .or(self.projects.last())
                        .map(|p| p.id.clone());
                    if let Some(id) = pick {
                        self.select_project(id, cx);
                    }
                }
                cx.notify();
            }
            SyncEvent::ProjectStatus {
                project_id,
                state,
                detail,
                progress,
            } => {
                if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
                    p.state = state;
                    p.state_detail = detail;
                    p.wake_progress = progress;
                }
                cx.notify();
            }
            SyncEvent::Chats { project_id, chats } => {
                let active = self.active_tab.get(&project_id).cloned();
                let still_there = active
                    .as_ref()
                    .map(|a| chats.iter().any(|c| &c.id == a))
                    .unwrap_or(false);
                if !still_there {
                    if let Some(first) = chats.first() {
                        self.active_tab
                            .insert(project_id.clone(), first.id.clone());
                    }
                }
                self.chats.insert(project_id, chats);
                cx.notify();
            }
            SyncEvent::ChatSnapshot { chat_id, messages } => {
                let dropped = !messages.is_empty();
                let ui = self.chat_ui.entry(chat_id.clone()).or_default();
                if dropped {
                    ui.composer_dropped = true;
                }
                self.messages.insert(chat_id.clone(), messages);
                cx.emit(AppEvent::TranscriptChanged { chat_id });
                cx.notify();
            }
            SyncEvent::MessageUpsert { chat_id, message } => {
                let list = self.messages.entry(chat_id.clone()).or_default();
                let mut text_only = false;
                let mut appended = 0;
                match list.iter_mut().find(|m| m.id == message.id) {
                    Some(existing) => {
                        let old_len = existing.text_len();
                        let new_len = message.text_len();
                        text_only = existing.status == message.status
                            && existing.parts.len() == message.parts.len()
                            && new_len > old_len
                            && same_shape(existing, &message);
                        appended = new_len.saturating_sub(old_len);
                        *existing = message;
                    }
                    None => list.push(message.clone()),
                }
                if text_only && appended > 0 {
                    cx.emit(AppEvent::TranscriptText { chat_id });
                } else {
                    cx.emit(AppEvent::TranscriptChanged { chat_id });
                    cx.notify();
                }
            }
            SyncEvent::Link(link) => {
                self.link = link;
                cx.notify();
            }
        }
    }

    // ----- selectors -------------------------------------------------------

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn selected(&self) -> Option<&Project> {
        self.selected_project
            .as_deref()
            .and_then(|id| self.project(id))
    }

    pub fn tabs(&self, project_id: &str) -> &[Chat] {
        self.chats
            .get(project_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn active_chat(&self) -> Option<&Chat> {
        let project_id = self.selected_project.as_deref()?;
        let chat_id = self.active_tab.get(project_id)?;
        self.tabs(project_id).iter().find(|c| &c.id == chat_id)
    }

    pub fn active_chat_id(&self) -> Option<Id> {
        self.active_chat().map(|c| c.id.clone())
    }

    pub fn messages_of(&self, chat_id: &str) -> &[Message] {
        self.messages
            .get(chat_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn chat_is_streaming(&self, chat_id: &str) -> bool {
        self.messages_of(chat_id).iter().any(|m| m.is_streaming())
    }

    pub fn project_is_streaming(&self, project_id: &str) -> bool {
        self.tabs(project_id)
            .iter()
            .any(|c| self.chat_is_streaming(&c.id))
    }

    pub fn project_unread(&self, project_id: &str) -> usize {
        self.tabs(project_id).iter().filter(|c| c.unread).count()
    }

    /// S8: the last assistant message ended with an `ask_user` question and
    /// nothing has been sent since.
    pub fn chat_needs_answer(&self, chat_id: &str) -> bool {
        match self.messages_of(chat_id).last() {
            Some(m) if m.role == Role::Assistant && !m.is_streaming() => m.parts.iter().any(|p| {
                matches!(
                    p,
                    Part::Status {
                        code: StatusKind::AskUser,
                        ..
                    }
                )
            }),
            Some(_) | None => false,
        }
    }

    pub fn composer_mode(&self, chat_id: &str) -> ComposerMode {
        if self.chat_is_streaming(chat_id) {
            ComposerMode::Stop
        } else if self.chat_needs_answer(chat_id) {
            ComposerMode::Answer
        } else {
            ComposerMode::Send
        }
    }

    pub fn chat_ui(&self, chat_id: &str) -> ChatUi {
        self.chat_ui.get(chat_id).cloned().unwrap_or_default()
    }

    pub fn window_is_narrow(&self, width: f32) -> bool {
        width < crate::ui::theme::NARROW_WINDOW
    }

    // ----- commands --------------------------------------------------------

    fn command(&self, cmd: ClientCommand) {
        let _ = self.commands.send(cmd);
    }

    pub fn select_project(&mut self, project_id: Id, cx: &mut Context<Self>) {
        self.selected_project = Some(project_id.clone());
        if let Some(chat_id) = self.active_tab.get(&project_id).cloned() {
            self.command(ClientCommand::OpenChat { chat_id });
        } else if let Some(first) = self.tabs(&project_id).first() {
            let id = first.id.clone();
            self.active_tab.insert(project_id, id.clone());
            self.command(ClientCommand::OpenChat { chat_id: id });
        }
        cx.notify();
    }

    pub fn select_tab(&mut self, chat_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else { return };
        self.active_tab.insert(project_id, chat_id.clone());
        if let Some(chat) = self
            .chats
            .values_mut()
            .flat_map(|v| v.iter_mut())
            .find(|c| c.id == chat_id)
        {
            chat.unread = false;
        }
        self.command(ClientCommand::MarkRead {
            chat_id: chat_id.clone(),
        });
        self.command(ClientCommand::OpenChat { chat_id });
        cx.notify();
    }

    pub fn new_tab(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else { return };
        let chat_id = new_id();
        self.chats
            .entry(project_id.clone())
            .or_default()
            .push(Chat {
                id: chat_id.clone(),
                project_id: project_id.clone(),
                title: "New chat".into(),
                is_pinned: false,
                is_system: false,
                unread: false,
                last_message_at: now_ms(),
            });
        self.messages.insert(chat_id.clone(), Vec::new());
        self.chat_ui.insert(chat_id.clone(), ChatUi::default());
        self.active_tab.insert(project_id.clone(), chat_id.clone());
        self.command(ClientCommand::NewChat {
            project_id,
            chat_id: chat_id.clone(),
        });
        self.command(ClientCommand::OpenChat { chat_id });
        cx.notify();
    }

    /// Returns false for the pinned tab (S3.8: it never closes).
    pub fn close_tab(&mut self, chat_id: Id, cx: &mut Context<Self>) -> bool {
        let Some(project_id) = self.selected_project.clone() else { return false };
        let tabs = self.chats.entry(project_id.clone()).or_default();
        let Some(ix) = tabs.iter().position(|c| c.id == chat_id) else { return false };
        if tabs[ix].is_pinned {
            return false;
        }
        tabs.remove(ix);
        if self.active_tab.get(&project_id) == Some(&chat_id) {
            let next = tabs
                .get(ix.saturating_sub(1))
                .or(tabs.first())
                .map(|c| c.id.clone());
            if let Some(next) = next {
                self.active_tab.insert(project_id.clone(), next.clone());
                self.command(ClientCommand::OpenChat { chat_id: next });
            }
        }
        self.command(ClientCommand::CloseChat { chat_id });
        cx.notify();
        true
    }

    pub fn send(&mut self, text: String, attachments: Vec<String>, cx: &mut Context<Self>) {
        let Some(chat_id) = self.active_chat_id() else { return };
        let message_id = new_id();
        let answer = self.chat_needs_answer(&chat_id);
        {
            let ui = self.chat_ui.entry(chat_id.clone()).or_default();
            if !ui.composer_dropped {
                ui.composer_dropped = true;
                ui.drop_started_at = Some(now_ms());
            }
        }
        // Optimistic echo: the row shows at once; the source's frame with the
        // same id replaces it (§3.6).
        let mut parts: Vec<Part> = attachments
            .iter()
            .map(|name| Part::Attachment {
                id: new_id(),
                attachment: crate::model::Attachment {
                    name: name.clone(),
                    mime: "application/octet-stream".into(),
                    blob_ref: None,
                },
            })
            .collect();
        parts.push(Part::Text {
            id: new_id(),
            text: text.clone(),
        });
        self.messages
            .entry(chat_id.clone())
            .or_default()
            .push(Message {
                id: message_id.clone(),
                chat_id: chat_id.clone(),
                role: Role::User,
                sender: "user".into(),
                status: MessageStatus::Queued,
                turn: None,
                model: None,
                effort: None,
                created_at: now_ms(),
                completed_at: None,
                pending: true,
                parts,
            });
        if answer {
            self.command(ClientCommand::Answer {
                chat_id: chat_id.clone(),
                message_id,
                text,
            });
        } else {
            self.command(ClientCommand::Send {
                chat_id: chat_id.clone(),
                message_id,
                text,
                attachments,
            });
        }
        cx.emit(AppEvent::TranscriptChanged { chat_id });
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(chat_id) = self.active_chat_id() {
            self.command(ClientCommand::Stop { chat_id });
        }
        cx.notify();
    }

    pub fn retry(&mut self, chat_id: Id, message_id: Id, cx: &mut Context<Self>) {
        self.command(ClientCommand::Retry {
            chat_id,
            message_id,
        });
        cx.notify();
    }

    pub fn wake(&mut self, project_id: Id, cx: &mut Context<Self>) {
        self.command(ClientCommand::Wake { project_id });
        cx.notify();
    }

    pub fn set_model(&mut self, model: String, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else { return };
        let effort = self.selected().map(|p| p.effort).unwrap_or(Effort::High);
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.model = model.clone();
        }
        self.command(ClientCommand::SetModel {
            project_id,
            model,
            effort,
        });
        cx.notify();
    }

    pub fn set_effort(&mut self, effort: Effort, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else { return };
        let model = self
            .selected()
            .map(|p| p.model.clone())
            .unwrap_or_default();
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.effort = effort;
        }
        self.command(ClientCommand::SetModel {
            project_id,
            model,
            effort,
        });
        cx.notify();
    }

    pub fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        self.rail_collapsed = !self.rail_collapsed;
        cx.notify();
    }

    pub fn toggle_right_panel(&mut self, cx: &mut Context<Self>) {
        self.right_panel_open = !self.right_panel_open;
        cx.notify();
    }

    pub fn open_details(&mut self, target: DetailsTarget, cx: &mut Context<Self>) {
        self.details_open = Some(target);
        cx.notify();
    }

    pub fn close_details(&mut self, cx: &mut Context<Self>) {
        self.details_open = None;
        cx.notify();
    }

    /// The live browser card of the selected project, if any (S16).
    pub fn live_browser(&self) -> Option<(String, String, String)> {
        let project_id = self.selected_project.as_deref()?;
        for chat in self.tabs(project_id) {
            for m in self.messages_of(&chat.id).iter().rev() {
                for p in &m.parts {
                    if let Part::Browser {
                        title,
                        url,
                        last_action,
                        live: true,
                        ..
                    } = p
                    {
                        return Some((title.clone(), url.clone(), last_action.clone()));
                    }
                }
            }
        }
        None
    }

    pub fn box_state(&self) -> Option<BoxState> {
        self.selected().map(|p| p.state)
    }
}

fn same_shape(a: &Message, b: &Message) -> bool {
    a.parts
        .iter()
        .zip(b.parts.iter())
        .all(|(x, y)| match (x, y) {
            (Part::Text { id: xa, .. }, Part::Text { id: ya, .. }) => xa == ya,
            (Part::Reasoning { id: xa, .. }, Part::Reasoning { id: ya, .. }) => xa == ya,
            (Part::Text { .. }, _) | (Part::Reasoning { .. }, _) => false,
            (other_a, other_b) => other_a == other_b,
        })
}

pub fn init_source(cx: &mut App) -> Box<dyn SyncSource> {
    let _ = cx;
    crate::sync::source_from_env()
}
