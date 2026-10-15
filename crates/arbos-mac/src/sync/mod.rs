//! The data layer. A [`SyncSource`] feeds the UI with [`SyncEvent`]s and takes
//! [`ClientCommand`]s back. Two sources exist: [`fixture::FixtureSource`]
//! (local Loro docs, scripted agent) and [`relay::RelaySource`] (HTTP + one
//! WebSocket per room on `arbos-relay`, `crdt-sync.md` §7–§9).
//!
//! Everything in this module is GPUI-free. Wire frames and the doc schema
//! constants come from `arbos-core`; the client-side mirror (`doc.rs`) and
//! the sources are candidates to move there later (`arbos-design.md` §7.2).

pub mod credentials;
pub mod doc;
pub mod fixture;
pub mod relay;

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::mpsc;

use crate::model::{BoxState, Chat, Effort, Id, Link, Message, Project, ProjectDocView};

/// Something the source learned; the UI applies it to its mirror.
#[derive(Clone, Debug)]
pub enum SyncEvent {
    /// No device token for this relay yet: show the pairing screen.
    NeedsPairing {
        relay_url: String,
    },
    /// Pairing finished (or failed with a message).
    Paired {
        device_id: String,
    },
    PairingFailed(String),
    /// Registry: the full project list, rail order.
    Projects(Vec<Project>),
    /// Box lifecycle for one project (relay `STATUS` frame or doc `status`).
    ProjectStatus {
        project_id: Id,
        state: BoxState,
        detail: Option<String>,
        progress: Option<f32>,
    },
    /// Project doc `chats` index for one project, in tab order.
    Chats {
        project_id: Id,
        chats: Vec<Chat>,
    },
    /// The relay created a chat for a locally minted tab; swap the id.
    ChatCreated {
        project_id: Id,
        local_id: Id,
        chat: Chat,
    },
    /// The project doc: threads (T1 index), status, settings, memory.
    ProjectDoc {
        project_id: Id,
        view: ProjectDocView,
    },
    /// Full message list of one chat doc (after a checkpoint or a list-level edit).
    ChatSnapshot {
        chat_id: Id,
        messages: Vec<Message>,
    },
    /// One message changed (streaming text, status, a new part).
    MessageUpsert {
        chat_id: Id,
        message: Message,
    },
    /// A blob the user asked for (file card, tool output).
    Blob {
        project_id: Id,
        key: String,
        mime: String,
        bytes: Arc<Vec<u8>>,
        saved_to: Option<PathBuf>,
    },
    /// One JPEG frame from the box's browser stream (S16).
    BrowserFrame {
        project_id: Id,
        jpeg: Arc<Vec<u8>>,
    },
    /// Relay link state (S14).
    Link(Link),
    /// Short user-facing notice (errors from HTTP calls, etc.).
    Notice(String),
}

/// Something the user did; the source writes it into the doc / outbox.
#[derive(Clone, Debug)]
pub enum ClientCommand {
    Pair {
        code: String,
    },
    OpenChat {
        chat_id: Id,
    },
    Send {
        chat_id: Id,
        message_id: Id,
        text: String,
        attachments: Vec<PathBuf>,
    },
    /// Reply to an `ask_user` question (S8). Same shape as `Send`, different outbox kind.
    Answer {
        chat_id: Id,
        message_id: Id,
        text: String,
    },
    Stop {
        chat_id: Id,
    },
    Retry {
        chat_id: Id,
        message_id: Id,
    },
    NewChat {
        project_id: Id,
        chat_id: Id,
    },
    CloseChat {
        chat_id: Id,
    },
    MarkRead {
        chat_id: Id,
    },
    SetModel {
        project_id: Id,
        model: String,
        effort: Effort,
    },
    CreateProject {
        name: String,
    },
    /// S11 "tap to resume" / S13 "Retry".
    Wake {
        project_id: Id,
    },
    FetchBlob {
        project_id: Id,
        key: String,
        save: bool,
    },
    WatchBrowser {
        project_id: Id,
        on: bool,
    },
}

pub trait SyncSource: Send + 'static {
    /// Short name shown in the settings popover ("fixture", "relay").
    fn name(&self) -> &'static str;

    /// Run until the command channel closes. The future runs on tokio.
    fn run(
        self: Box<Self>,
        events: mpsc::UnboundedSender<SyncEvent>,
        commands: mpsc::UnboundedReceiver<ClientCommand>,
    ) -> BoxFuture<'static, anyhow::Result<()>>;
}

/// Pick the source from the environment. `ARBOS_RELAY_URL=http://host:port`
/// (or `ws://`) selects the relay; anything else runs the fixture.
pub fn source_from_env() -> Box<dyn SyncSource> {
    match std::env::var("ARBOS_RELAY_URL") {
        Ok(url) if !url.trim().is_empty() => Box::new(relay::RelaySource::from_env(url)),
        Ok(_) | Err(_) => Box::new(fixture::FixtureSource::new()),
    }
}
