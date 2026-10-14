//! Local, offline source. Every chat is a real Loro doc (`doc.rs`); the
//! "agent" is a scripted tokio task that writes into the doc the way the box
//! would: a `deciding` status, tool cards flipping from running to done, text
//! appended in 120 ms frames, a final status. The UI cannot tell it apart from
//! the relay path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures::future::BoxFuture;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::sleep;

use super::doc::ChatDoc;
use super::{ClientCommand, SyncEvent, SyncSource};
use crate::model::{
    Attachment, BoxState, Chat, Effort, Id, Link, Message, MessageStatus, Part, Project, Role,
    StatusKind, ToolCall, ToolStatus, TurnKind, new_id, now_ms,
};

pub const STREAM_FRAME_MS: u64 = arbos_core::STREAM_COMMIT_MS;
const DEVICE: &str = "mac:fixture";

pub const PROJECT_ARBOS: &str = "01J9ARBOS00000000000000000";
pub const PROJECT_AFFINE: &str = "01J9AFFINE0000000000000000";
pub const PROJECT_COMMS: &str = "01J9COMMS00000000000000000";

pub const CHAT_ARBOS_MAIN: &str = "01J9CHAT0ARBOS000000000000";
pub const CHAT_ARBOS_RESEARCH: &str = "01J9CHAT0RESEARCH000000000";
pub const CHAT_ARBOS_LAUNCH: &str = "01J9CHAT0LAUNCH00000000000";
pub const CHAT_AFFINE_MAIN: &str = "01J9CHAT0AFFINE00000000000";
pub const CHAT_COMMS_MAIN: &str = "01J9CHAT0COMMS000000000000";

pub const MODELS: [&str; 4] = ["GPT-6.1-Sol", "Claude Fable 5.1", "Gemini 4 Argon", "Grok 4.7"];

type Docs = Arc<Mutex<HashMap<Id, ChatDoc>>>;

struct DocChanged {
    chat_id: Id,
    touched: super::doc::Touched,
}

struct Shared {
    docs: Docs,
    projects: Mutex<Vec<Project>>,
    chats: Mutex<Vec<Chat>>,
    open_chat: Mutex<Option<Id>>,
    running: Mutex<HashMap<Id, Arc<AtomicBool>>>,
    events: mpsc::UnboundedSender<SyncEvent>,
    doc_tx: mpsc::UnboundedSender<DocChanged>,
    subscriptions: Mutex<Vec<loro::Subscription>>,
}

pub struct FixtureSource;

impl FixtureSource {
    pub fn new() -> Self {
        Self
    }
}

impl SyncSource for FixtureSource {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn run(
        self: Box<Self>,
        events: mpsc::UnboundedSender<SyncEvent>,
        mut commands: mpsc::UnboundedReceiver<ClientCommand>,
    ) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move {
            let (doc_tx, mut doc_rx) = mpsc::unbounded_channel();
            let shared = Arc::new(Shared {
                docs: Arc::new(Mutex::new(HashMap::new())),
                projects: Mutex::new(seed_projects()),
                chats: Mutex::new(seed_chats()),
                open_chat: Mutex::new(None),
                running: Mutex::new(HashMap::new()),
                events: events.clone(),
                doc_tx,
                subscriptions: Mutex::new(Vec::new()),
            });
            seed_docs(&shared);

            let _ = events.send(SyncEvent::Link(Link::Connecting));
            emit_projects(&shared);
            emit_chats(&shared);
            {
                let docs = shared.docs.lock().unwrap();
                for (chat_id, doc) in docs.iter() {
                    let _ = events.send(SyncEvent::ChatSnapshot {
                        chat_id: chat_id.clone(),
                        messages: doc.read_all(),
                    });
                }
            }
            let _ = events.send(SyncEvent::Link(Link::Live));

            loop {
                tokio::select! {
                    changed = doc_rx.recv() => {
                        let Some(changed) = changed else { break };
                        forward_doc_change(&shared, changed);
                    }
                    cmd = commands.recv() => {
                        let Some(cmd) = cmd else { break };
                        handle_command(&shared, cmd);
                    }
                }
            }
            Ok(())
        })
    }
}

fn forward_doc_change(shared: &Arc<Shared>, changed: DocChanged) {
    let docs = shared.docs.lock().unwrap();
    let Some(doc) = docs.get(&changed.chat_id) else { return };
    if changed.touched.list_changed {
        let _ = shared.events.send(SyncEvent::ChatSnapshot {
            chat_id: changed.chat_id.clone(),
            messages: doc.read_all(),
        });
        return;
    }
    for ix in changed.touched.messages {
        if let Some(message) = doc.read_message(ix) {
            let _ = shared.events.send(SyncEvent::MessageUpsert {
                chat_id: changed.chat_id.clone(),
                message,
            });
        }
    }
}

fn handle_command(shared: &Arc<Shared>, cmd: ClientCommand) {
    match cmd {
        ClientCommand::OpenChat { chat_id } => {
            *shared.open_chat.lock().unwrap() = Some(chat_id.clone());
            mark_read(shared, &chat_id);
        }
        ClientCommand::Send {
            chat_id,
            message_id,
            text,
            attachments,
        } => {
            let project_id = project_of(shared, &chat_id);
            {
                let docs = shared.docs.lock().unwrap();
                if let Some(doc) = docs.get(&chat_id) {
                    let asleep = project_state(shared, &project_id).is_asleep();
                    let mut parts: Vec<Part> = attachments
                        .iter()
                        .map(|name| Part::Attachment {
                            id: new_id(),
                            attachment: Attachment {
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
                    let msg = Message {
                        id: message_id.clone(),
                        chat_id: chat_id.clone(),
                        role: Role::User,
                        sender: "user".into(),
                        status: if asleep {
                            MessageStatus::Queued
                        } else {
                            MessageStatus::Done
                        },
                        turn: None,
                        model: None,
                        effort: None,
                        created_at: now_ms(),
                        completed_at: None,
                        pending: true,
                        parts,
                    };
                    let _ = doc.insert_message(&msg);
                    let _ = doc.push_command(
                        "send",
                        json!({ "messageId": message_id, "text": text }),
                        DEVICE,
                    );
                    doc.commit();
                }
            }
            maybe_retitle(shared, &chat_id, &text);
            spawn_turn(shared, project_id, chat_id, message_id, text, false);
        }
        ClientCommand::Answer {
            chat_id,
            message_id,
            text,
        } => {
            let project_id = project_of(shared, &chat_id);
            {
                let docs = shared.docs.lock().unwrap();
                if let Some(doc) = docs.get(&chat_id) {
                    let msg = Message {
                        id: message_id.clone(),
                        chat_id: chat_id.clone(),
                        role: Role::User,
                        sender: "user".into(),
                        status: MessageStatus::Done,
                        turn: None,
                        model: None,
                        effort: None,
                        created_at: now_ms(),
                        completed_at: None,
                        pending: true,
                        parts: vec![Part::Text {
                            id: new_id(),
                            text: text.clone(),
                        }],
                    };
                    let _ = doc.insert_message(&msg);
                    let _ = doc.push_command(
                        "answer",
                        json!({ "messageId": message_id, "text": text }),
                        DEVICE,
                    );
                    doc.commit();
                }
            }
            spawn_turn(shared, project_id, chat_id, message_id, text, true);
        }
        ClientCommand::Stop { chat_id } => {
            if let Some(flag) = shared.running.lock().unwrap().get(&chat_id) {
                flag.store(true, Ordering::SeqCst);
            }
            let docs = shared.docs.lock().unwrap();
            if let Some(doc) = docs.get(&chat_id) {
                let _ = doc.push_command("stop", json!({}), DEVICE);
                doc.commit();
            }
        }
        ClientCommand::Retry {
            chat_id,
            message_id,
        } => {
            let project_id = project_of(shared, &chat_id);
            let prompt = {
                let docs = shared.docs.lock().unwrap();
                docs.get(&chat_id).and_then(|doc| {
                    let ix = doc.index_of(&message_id)?;
                    let text = (0..ix)
                        .rev()
                        .filter_map(|i| doc.read_message(i))
                        .find(|m| m.role == Role::User)
                        .map(|m| m.plain_text())?;
                    let _ = doc.set_status(ix, MessageStatus::Cancelled);
                    let _ = doc.push_command("retry", json!({ "messageId": message_id }), DEVICE);
                    doc.commit();
                    Some(text)
                })
            };
            if let Some(text) = prompt {
                spawn_turn(shared, project_id, chat_id, new_id(), text, true);
            }
        }
        ClientCommand::NewChat {
            project_id,
            chat_id,
        } => {
            add_doc(shared, ChatDoc::new(&chat_id, &project_id));
            shared.chats.lock().unwrap().push(Chat {
                id: chat_id.clone(),
                project_id,
                title: "New chat".into(),
                is_pinned: false,
                is_system: false,
                unread: false,
                last_message_at: now_ms(),
            });
            emit_chats(shared);
            let _ = shared.events.send(SyncEvent::ChatSnapshot {
                chat_id,
                messages: Vec::new(),
            });
        }
        ClientCommand::CloseChat { chat_id } => {
            shared
                .chats
                .lock()
                .unwrap()
                .retain(|c| c.id != chat_id || c.is_pinned);
            emit_chats(shared);
        }
        ClientCommand::MarkRead { chat_id } => mark_read(shared, &chat_id),
        ClientCommand::SetModel {
            project_id,
            model,
            effort,
        } => {
            {
                let mut projects = shared.projects.lock().unwrap();
                if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
                    p.model = model;
                    p.effort = effort;
                }
            }
            emit_projects(shared);
        }
        ClientCommand::Wake { project_id } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                wake_project(&shared, &project_id).await;
            });
        }
    }
}

fn add_doc(shared: &Arc<Shared>, doc: ChatDoc) {
    let chat_id = doc.chat_id.clone();
    let tx = shared.doc_tx.clone();
    let id_for_cb = chat_id.clone();
    let sub = doc.doc.subscribe_root(Arc::new(move |event| {
        let touched = ChatDoc::touched(&event);
        if touched.list_changed || !touched.messages.is_empty() {
            let _ = tx.send(DocChanged {
                chat_id: id_for_cb.clone(),
                touched,
            });
        }
    }));
    shared.subscriptions.lock().unwrap().push(sub);
    shared.docs.lock().unwrap().insert(chat_id, doc);
}

fn project_of(shared: &Shared, chat_id: &str) -> Id {
    shared
        .chats
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.id == chat_id)
        .map(|c| c.project_id.clone())
        .unwrap_or_else(|| PROJECT_ARBOS.to_string())
}

fn project_state(shared: &Shared, project_id: &str) -> BoxState {
    shared
        .projects
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.id == project_id)
        .map(|p| p.state)
        .unwrap_or(BoxState::Awake)
}

fn project_model(shared: &Shared, project_id: &str) -> (String, Effort) {
    shared
        .projects
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.id == project_id)
        .map(|p| (p.model.clone(), p.effort))
        .unwrap_or((MODELS[0].to_string(), Effort::High))
}

fn set_project_state(
    shared: &Shared,
    project_id: &str,
    state: BoxState,
    detail: Option<&str>,
    progress: Option<f32>,
) {
    {
        let mut projects = shared.projects.lock().unwrap();
        if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
            p.state = state;
            p.state_detail = detail.map(str::to_string);
            p.wake_progress = progress;
        }
    }
    let _ = shared.events.send(SyncEvent::ProjectStatus {
        project_id: project_id.to_string(),
        state,
        detail: detail.map(str::to_string),
        progress,
    });
}

fn emit_projects(shared: &Shared) {
    let projects = shared.projects.lock().unwrap().clone();
    let _ = shared.events.send(SyncEvent::Projects(projects));
}

fn emit_chats(shared: &Shared) {
    let chats = shared.chats.lock().unwrap().clone();
    let mut by_project: HashMap<Id, Vec<Chat>> = HashMap::new();
    for chat in chats {
        by_project
            .entry(chat.project_id.clone())
            .or_default()
            .push(chat);
    }
    for (project_id, chats) in by_project {
        let _ = shared.events.send(SyncEvent::Chats { project_id, chats });
    }
}

fn mark_read(shared: &Shared, chat_id: &str) {
    let changed = {
        let mut chats = shared.chats.lock().unwrap();
        match chats.iter_mut().find(|c| c.id == chat_id && c.unread) {
            Some(c) => {
                c.unread = false;
                true
            }
            None => false,
        }
    };
    if changed {
        emit_chats(shared);
    }
}

fn maybe_retitle(shared: &Shared, chat_id: &str, text: &str) {
    let changed = {
        let mut chats = shared.chats.lock().unwrap();
        match chats
            .iter_mut()
            .find(|c| c.id == chat_id && !c.is_pinned && c.title == "New chat")
        {
            Some(c) => {
                c.title = title_from(text);
                c.last_message_at = now_ms();
                true
            }
            None => false,
        }
    };
    if changed {
        emit_chats(shared);
    }
}

fn title_from(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(34).collect();
    if line.chars().count() > 34 {
        out.push('…');
    }
    if out.is_empty() {
        "New chat".into()
    } else {
        out
    }
}

async fn wake_project(shared: &Arc<Shared>, project_id: &str) {
    let state = project_state(shared, project_id);
    if !state.is_asleep() && state != BoxState::Error {
        return;
    }
    let deep = state == BoxState::HibernatedDeep;
    let steps = if deep { 24 } else { 14 };
    let detail = if deep {
        "Restoring from archive…"
    } else {
        "Waking sandbox…"
    };
    for i in 0..=steps {
        set_project_state(
            shared,
            project_id,
            BoxState::Waking,
            Some(detail),
            Some(i as f32 / steps as f32),
        );
        sleep(Duration::from_millis(180)).await;
    }
    set_project_state(shared, project_id, BoxState::Awake, None, None);
    let chat_ids: Vec<Id> = shared
        .chats
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.project_id == project_id)
        .map(|c| c.id.clone())
        .collect();
    let docs = shared.docs.lock().unwrap();
    for chat_id in chat_ids {
        if let Some(doc) = docs.get(&chat_id) {
            for ix in 0..doc.len() {
                if doc
                    .read_message(ix)
                    .map(|m| m.status == MessageStatus::Queued)
                    .unwrap_or(false)
                {
                    let _ = doc.set_status(ix, MessageStatus::Done);
                }
            }
            doc.commit();
        }
    }
}

fn spawn_turn(
    shared: &Arc<Shared>,
    project_id: Id,
    chat_id: Id,
    user_message_id: Id,
    text: String,
    force_reply: bool,
) {
    let stop = Arc::new(AtomicBool::new(false));
    shared
        .running
        .lock()
        .unwrap()
        .insert(chat_id.clone(), stop.clone());
    let shared = shared.clone();
    tokio::spawn(async move {
        run_turn(&shared, &project_id, &chat_id, &user_message_id, &text, force_reply, stop).await;
        shared.running.lock().unwrap().remove(&chat_id);
    });
}

enum Script {
    Reply(&'static [&'static str]),
    Act {
        tools: &'static [(&'static str, &'static str, &'static str, u64)],
        diff: Option<(&'static str, u32, u32)>,
        summary: &'static [&'static str],
    },
    Browse {
        title: &'static str,
        url: &'static str,
        summary: &'static [&'static str],
    },
    Ask(&'static str, &'static str),
    Fail(&'static str, &'static str),
}

const REPLY_DEFAULT: &[&str] = &[
    "Here is how I would approach it.",
    "- Keep the rail narrow and the tabs flat, so the chat column gets the width.\n- Put the composer at the bottom once a chat has a message; before that, center it so a new tab feels like a blank page.\n- Treat every tool call as a card that folds, so long runs stay readable.",
    "If you want, I can turn this into a short checklist and start on the first item.",
];

const REPLY_SIMPLE: &[&str] = &[
    "Agreed. Two sections only: the rail on the left and the chat in the center.",
    "Nothing else at rest. Details open only when you ask for them.",
];

const REPLY_CODE: &[&str] = &[
    "The transcript is a bottom-aligned list with one row per markdown block:",
    "```rust\nlet list = ListState::new(0, ListAlignment::Bottom, px(320.0));\nlist(list.clone(), cx.processor(Self::render_row))\n```",
    "Rows are keyed by `{msg}#{part}.{block}` and re-spliced only when their bytes change, so a streaming reply never re-renders the settled prefix.",
];

const ACT_TOOLS: &[(&str, &str, &str, u64)] = &[
    ("read_file", "crates/arbos-mac/src/ui/rail.rs", "90 px rail, 72 px tiles, 11 px labels", 420),
    ("grep", "TILE_W crates/", "3 matches in rail.rs, theme.rs", 180),
    ("edit_file", "crates/arbos-mac/src/ui/rail.rs", "Set tile gap to 10 px", 640),
    ("bash", "cargo check -p arbos-mac", "Finished `dev` profile in 4.1 s", 1900),
];

const ACT_SUMMARY: &[&str] = &[
    "Done. The tiles were 68 px wide with an 8 px gap; the mock uses 72 px and 10 px.",
    "I changed the two constants in `rail.rs`, re-ran `cargo check`, and it is clean. The diff is above if you want to look.",
];

const BROWSE_SUMMARY: &[&str] = &[
    "Yes. `ListAlignment::Bottom` scrolls from the bottom up, like a chat log, and `ListSizingBehavior::Auto` lets the list fill the column.",
    "I saved the relevant page as `docs/research/gpui.md` in the sandbox.",
];

const ASK_LEAD: &str = "I can do that. One thing first:";
const ASK_QUESTION: &str =
    "Which date should I plan toward: the 1 December private beta, or the January public launch?";

fn pick_script(text: &str, force_reply: bool) -> Script {
    let t = text.to_lowercase();
    if force_reply {
        return Script::Reply(REPLY_DEFAULT);
    }
    if t.contains("fail") || t.contains("error") || t.contains("timeout") {
        return Script::Fail("sandbox_timeout", "Sandbox did not answer in 60 s");
    }
    if t.contains("ask") || t.contains("launch") || t.contains("deadline") {
        return Script::Ask(ASK_LEAD, ASK_QUESTION);
    }
    if t.contains("browse") || t.contains("docs") || t.contains("open ") || t.contains("website") {
        return Script::Browse {
            title: "GPUI docs — ListAlignment",
            url: "https://docs.rs/gpui/latest/gpui/enum.ListAlignment.html",
            summary: BROWSE_SUMMARY,
        };
    }
    if t.contains("fix")
        || t.contains("check")
        || t.contains("run ")
        || t.contains("build")
        || t.contains("test")
        || t.contains("padding")
    {
        return Script::Act {
            tools: ACT_TOOLS,
            diff: Some(("crates/arbos-mac/src/ui/rail.rs", 6, 2)),
            summary: ACT_SUMMARY,
        };
    }
    if t.contains("simple") {
        return Script::Reply(REPLY_SIMPLE);
    }
    if t.contains("list") || t.contains("code") || t.contains("transcript") {
        return Script::Reply(REPLY_CODE);
    }
    Script::Reply(REPLY_DEFAULT)
}

async fn run_turn(
    shared: &Arc<Shared>,
    project_id: &str,
    chat_id: &str,
    user_message_id: &str,
    text: &str,
    force_reply: bool,
    stop: Arc<AtomicBool>,
) {
    sleep(Duration::from_millis(220)).await;
    with_doc(shared, chat_id, |doc| {
        if let Some(ix) = doc.index_of(user_message_id) {
            let _ = doc.set_field(ix, "pending", json!(false));
            doc.commit();
        }
    });

    if project_state(shared, project_id).is_asleep() {
        wake_project(shared, project_id).await;
    }

    let (model, effort) = project_model(shared, project_id);
    let assistant_id = new_id();
    let ix = with_doc(shared, chat_id, |doc| {
        let msg = Message {
            id: assistant_id.clone(),
            chat_id: chat_id.to_string(),
            role: Role::Assistant,
            sender: "agent".into(),
            status: MessageStatus::Streaming,
            turn: None,
            model: Some(model.clone()),
            effort: Some(effort),
            created_at: now_ms(),
            completed_at: None,
            pending: false,
            parts: vec![Part::Status {
                id: new_id(),
                code: StatusKind::Deciding,
                message: "Arbos is deciding…".into(),
            }],
        };
        let ix = doc.insert_message(&msg).unwrap_or(0);
        doc.commit();
        ix
    })
    .unwrap_or(0);

    sleep(Duration::from_millis(700)).await;
    if stop.load(Ordering::SeqCst) {
        finish_cancelled(shared, chat_id, ix);
        return;
    }

    let script = pick_script(text, force_reply);
    let turn = match &script {
        Script::Reply(_) | Script::Ask(_, _) | Script::Fail(_, _) => TurnKind::Reply,
        Script::Act { .. } => TurnKind::Act,
        Script::Browse { .. } => TurnKind::Spawn,
    };
    with_doc(shared, chat_id, |doc| {
        let _ = doc.set_field(ix, "turn", serde_json::to_value(turn).unwrap_or_default());
        doc.commit();
    });

    let finished = match script {
        Script::Reply(blocks) => {
            with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(ix, 0, &empty_text());
                doc.commit();
            });
            stream_blocks(shared, chat_id, ix, 0, blocks, &stop).await
        }
        Script::Act {
            tools,
            diff,
            summary,
        } => {
            let mut first = true;
            let mut ok = true;
            for (name, input, out, ms) in tools {
                if stop.load(Ordering::SeqCst) {
                    ok = false;
                    break;
                }
                let call = ToolCall {
                    id: new_id(),
                    name: name.to_string(),
                    input: json!(input),
                };
                let pix = with_doc(shared, chat_id, |doc| {
                    let part = Part::Tool {
                        id: new_id(),
                        call: call.clone(),
                        output_summary: String::new(),
                        output_ref: None,
                        is_error: false,
                        status: Some(ToolStatus::Running),
                        duration_ms: None,
                        subagent_name: None,
                    };
                    let pix = if first {
                        let _ = doc.replace_part(ix, 0, &part);
                        0
                    } else {
                        doc.push_part(ix, &part).unwrap_or(0)
                    };
                    doc.commit();
                    pix
                })
                .unwrap_or(0);
                first = false;
                sleep(Duration::from_millis(*ms)).await;
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.set_part_field(ix, pix, "outputSummary", json!(out));
                    let _ = doc.set_part_field(
                        ix,
                        pix,
                        "outputRef",
                        json!(format!("blob/{}/{}", project_id, call.id)),
                    );
                    let _ = doc.set_part_field(ix, pix, "status", json!("ok"));
                    let _ = doc.set_part_field(ix, pix, "durationMs", json!(ms));
                    doc.commit();
                });
                if *name == "edit_file" {
                    if let Some((path, added, removed)) = diff {
                        with_doc(shared, chat_id, |doc| {
                            let _ = doc.push_part(
                                ix,
                                &Part::Diff {
                                    id: new_id(),
                                    path: path.to_string(),
                                    added,
                                    removed,
                                    blob_ref: Some(format!("blob/{project_id}/diff-rail")),
                                },
                            );
                            doc.commit();
                        });
                    }
                }
            }
            if ok {
                let pix = with_doc(shared, chat_id, |doc| {
                    let pix = doc.push_part(ix, &empty_text()).unwrap_or(0);
                    doc.commit();
                    pix
                })
                .unwrap_or(0);
                stream_blocks(shared, chat_id, ix, pix, summary, &stop).await
            } else {
                false
            }
        }
        Script::Browse {
            title,
            url,
            summary,
        } => {
            let status_pix = with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(
                    ix,
                    0,
                    &Part::Status {
                        id: new_id(),
                        code: StatusKind::SubagentWorking,
                        message: "Working in sandbox · starting browser · 00:00".into(),
                    },
                );
                let _ = doc.push_part(
                    ix,
                    &Part::Tool {
                        id: new_id(),
                        call: ToolCall {
                            id: new_id(),
                            name: "spawn_subagent".into(),
                            input: json!("docs-lookup · prime"),
                        },
                        output_summary: "Subagent started".into(),
                        output_ref: None,
                        is_error: false,
                        status: Some(ToolStatus::Ok),
                        duration_ms: Some(300),
                        subagent_name: Some("docs-lookup".into()),
                    },
                );
                doc.commit();
                0
            })
            .unwrap_or(0);
            sleep(Duration::from_millis(900)).await;
            let browser_pix = with_doc(shared, chat_id, |doc| {
                let pix = doc
                    .push_part(
                        ix,
                        &Part::Browser {
                            id: new_id(),
                            title: title.to_string(),
                            url: url.to_string(),
                            last_action: "navigate".into(),
                            live: true,
                        },
                    )
                    .unwrap_or(0);
                doc.commit();
                pix
            })
            .unwrap_or(0);
            let actions = ["scroll", "click \"Variants\"", "read page", "save notes"];
            let mut cancelled = false;
            for (i, action) in actions.iter().enumerate() {
                if stop.load(Ordering::SeqCst) {
                    cancelled = true;
                    break;
                }
                sleep(Duration::from_millis(1000)).await;
                let secs = i + 2;
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.set_part_field(
                        ix,
                        status_pix,
                        "message",
                        json!(format!(
                            "Working in sandbox · {} · 00:{:02}",
                            action, secs
                        )),
                    );
                    let _ = doc.set_part_field(ix, browser_pix, "lastAction", json!(action));
                    doc.commit();
                });
            }
            if cancelled {
                false
            } else {
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.set_part_field(ix, browser_pix, "live", json!(false));
                    let _ = doc.set_part_field(
                        ix,
                        status_pix,
                        "message",
                        json!("Worked in sandbox · 4 actions · 00:06"),
                    );
                    let _ = doc.push_part(
                        ix,
                        &Part::File {
                            id: new_id(),
                            path: "docs/research/gpui.md".into(),
                            lines: 212,
                            blob_ref: Some(format!("blob/{project_id}/gpui-notes")),
                        },
                    );
                    doc.commit();
                });
                let pix = with_doc(shared, chat_id, |doc| {
                    let pix = doc.push_part(ix, &empty_text()).unwrap_or(0);
                    doc.commit();
                    pix
                })
                .unwrap_or(0);
                stream_blocks(shared, chat_id, ix, pix, summary, &stop).await
            }
        }
        Script::Ask(lead, question) => {
            with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(ix, 0, &empty_text());
                doc.commit();
            });
            let done = stream_blocks(shared, chat_id, ix, 0, &[lead], &stop).await;
            if done {
                with_doc(shared, chat_id, |doc| {
                    let _ = doc.push_part(
                        ix,
                        &Part::Status {
                            id: new_id(),
                            code: StatusKind::AskUser,
                            message: question.to_string(),
                        },
                    );
                    doc.commit();
                });
            }
            done
        }
        Script::Fail(code, message) => {
            sleep(Duration::from_millis(900)).await;
            with_doc(shared, chat_id, |doc| {
                let _ = doc.replace_part(
                    ix,
                    0,
                    &Part::Error {
                        id: new_id(),
                        code: code.to_string(),
                        message: message.to_string(),
                    },
                );
                let _ = doc.set_status(ix, MessageStatus::Error);
                doc.commit();
            });
            mark_unread_if_hidden(shared, chat_id);
            return;
        }
    };

    if finished {
        with_doc(shared, chat_id, |doc| {
            let _ = doc.set_status(ix, MessageStatus::Done);
            doc.commit();
        });
        mark_unread_if_hidden(shared, chat_id);
    } else {
        finish_cancelled(shared, chat_id, ix);
    }
}

fn empty_text() -> Part {
    Part::Text {
        id: new_id(),
        text: String::new(),
    }
}

fn finish_cancelled(shared: &Arc<Shared>, chat_id: &str, ix: usize) {
    with_doc(shared, chat_id, |doc| {
        let _ = doc.push_part(
            ix,
            &Part::Status {
                id: new_id(),
                code: StatusKind::Note,
                message: "Stopped by you".into(),
            },
        );
        let _ = doc.set_status(ix, MessageStatus::Cancelled);
        doc.commit();
    });
}

fn mark_unread_if_hidden(shared: &Arc<Shared>, chat_id: &str) {
    let open = shared.open_chat.lock().unwrap().clone();
    if open.as_deref() == Some(chat_id) {
        return;
    }
    {
        let mut chats = shared.chats.lock().unwrap();
        if let Some(c) = chats.iter_mut().find(|c| c.id == chat_id) {
            c.unread = true;
            c.last_message_at = now_ms();
        }
    }
    emit_chats(shared);
}

/// Stream markdown blocks into one text part in 120 ms frames of a few words.
/// Returns false when stopped.
async fn stream_blocks(
    shared: &Arc<Shared>,
    chat_id: &str,
    ix: usize,
    pix: usize,
    blocks: &[&str],
    stop: &AtomicBool,
) -> bool {
    let mut full = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            full.push_str("\n\n");
        }
        full.push_str(block);
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut words = 0;
    for piece in full.split_inclusive(' ') {
        current.push_str(piece);
        words += 1;
        if words >= 3 {
            chunks.push(std::mem::take(&mut current));
            words = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    for chunk in chunks {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        with_doc(shared, chat_id, |doc| {
            let _ = doc.append_text(ix, pix, &chunk);
            doc.commit();
        });
        sleep(Duration::from_millis(STREAM_FRAME_MS)).await;
    }
    true
}

fn with_doc<R>(shared: &Arc<Shared>, chat_id: &str, f: impl FnOnce(&ChatDoc) -> R) -> Option<R> {
    let docs = shared.docs.lock().unwrap();
    docs.get(chat_id).map(f)
}

// ---------------------------------------------------------------------------
// Seed data: the mock conversation plus one chat per rendering state.
// ---------------------------------------------------------------------------

fn seed_projects() -> Vec<Project> {
    vec![
        Project {
            id: PROJECT_AFFINE.into(),
            name: "Affine".into(),
            icon: "△".into(),
            color: "#8b7cf6".into(),
            state: BoxState::HibernatedLight,
            state_detail: None,
            wake_progress: None,
            model: MODELS[1].into(),
            effort: Effort::Medium,
            spend_today_usd: 0.42,
            spend_cap_usd: 25.0,
        },
        Project {
            id: PROJECT_COMMS.into(),
            name: "Comms".into(),
            icon: "Ψ".into(),
            color: "#8b7cf6".into(),
            state: BoxState::StoppedByUser,
            state_detail: None,
            wake_progress: None,
            model: MODELS[0].into(),
            effort: Effort::Low,
            spend_today_usd: 1.10,
            spend_cap_usd: 25.0,
        },
        Project {
            id: PROJECT_ARBOS.into(),
            name: "Arbos".into(),
            icon: "Ω".into(),
            color: "#8b7cf6".into(),
            state: BoxState::Awake,
            state_detail: None,
            wake_progress: None,
            model: MODELS[0].into(),
            effort: Effort::High,
            spend_today_usd: 3.87,
            spend_cap_usd: 25.0,
        },
    ]
}

fn seed_chats() -> Vec<Chat> {
    let t = now_ms();
    let chat = |id: &str, project: &str, title: &str, pinned: bool, ago_min: u64| Chat {
        id: id.into(),
        project_id: project.into(),
        title: title.into(),
        is_pinned: pinned,
        is_system: false,
        unread: false,
        last_message_at: t.saturating_sub(ago_min * 60_000),
    };
    vec![
        chat(CHAT_ARBOS_MAIN, PROJECT_ARBOS, "Arbos", true, 3),
        chat(CHAT_ARBOS_RESEARCH, PROJECT_ARBOS, "Interface research", false, 40),
        chat(CHAT_ARBOS_LAUNCH, PROJECT_ARBOS, "Launch planning", false, 95),
        chat(CHAT_AFFINE_MAIN, PROJECT_AFFINE, "Affine", true, 60 * 26),
        chat(CHAT_COMMS_MAIN, PROJECT_COMMS, "Comms", true, 60 * 5),
    ]
}

struct Seeder<'a> {
    doc: &'a ChatDoc,
    t: u64,
}

impl Seeder<'_> {
    fn user(&mut self, text: &str) {
        self.t += 45_000;
        let _ = self.doc.insert_message(&Message {
            id: new_id(),
            chat_id: self.doc.chat_id.clone(),
            role: Role::User,
            sender: "user".into(),
            status: MessageStatus::Done,
            turn: None,
            model: None,
            effort: None,
            created_at: self.t,
            completed_at: Some(self.t),
            pending: false,
            parts: vec![Part::Text {
                id: new_id(),
                text: text.into(),
            }],
        });
    }

    fn assistant(&mut self, turn: TurnKind, status: MessageStatus, parts: Vec<Part>) {
        self.t += 20_000;
        let _ = self.doc.insert_message(&Message {
            id: new_id(),
            chat_id: self.doc.chat_id.clone(),
            role: Role::Assistant,
            sender: "agent".into(),
            status,
            turn: Some(turn),
            model: Some(MODELS[0].into()),
            effort: Some(Effort::High),
            created_at: self.t,
            completed_at: Some(self.t + 9_000),
            pending: false,
            parts,
        });
    }
}

fn text(s: &str) -> Part {
    Part::Text {
        id: new_id(),
        text: s.into(),
    }
}

fn tool(name: &str, input: &str, out: &str, ms: u64) -> Part {
    Part::Tool {
        id: new_id(),
        call: ToolCall {
            id: new_id(),
            name: name.into(),
            input: json!(input),
        },
        output_summary: out.into(),
        output_ref: Some(format!("blob/seed/{}", new_id())),
        is_error: false,
        status: Some(ToolStatus::Ok),
        duration_ms: Some(ms),
        subagent_name: None,
    }
}

fn seed_docs(shared: &Arc<Shared>) {
    let base = now_ms().saturating_sub(3 * 60 * 60_000);

    let main = ChatDoc::new(CHAT_ARBOS_MAIN, PROJECT_ARBOS);
    {
        let mut s = Seeder { doc: &main, t: base };
        s.user("Let's refine the agent workspace.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![text(
                "Each agent has its own space, with separate tabs for the conversations happening in parallel.",
            )],
        );
        s.user("Keep it simple.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![text(
                "A compact agent rail, clear chat tabs, and everything else within reach.",
            )],
        );
        main.commit();
    }
    add_doc(shared, main);

    let research = ChatDoc::new(CHAT_ARBOS_RESEARCH, PROJECT_ARBOS);
    {
        let mut s = Seeder {
            doc: &research,
            t: base + 600_000,
        };
        s.user("Check the rail tile sizes against the mock and fix the padding.");
        s.assistant(
            TurnKind::Act,
            MessageStatus::Done,
            vec![
                tool("read_file", "crates/arbos-mac/src/ui/rail.rs", "90 px rail, 68 px tiles, 8 px gap", 410),
                tool("grep", "TILE_W crates/", "3 matches in rail.rs, theme.rs", 160),
                tool("edit_file", "crates/arbos-mac/src/ui/rail.rs", "Tiles 72 px, gap 10 px", 620),
                Part::Diff {
                    id: new_id(),
                    path: "crates/arbos-mac/src/ui/rail.rs".into(),
                    added: 6,
                    removed: 2,
                    blob_ref: Some("blob/seed/diff-rail".into()),
                },
                tool("bash", "cargo check -p arbos-mac", "Finished `dev` profile in 4.1 s", 4100),
                text("Tiles were 68 px with an 8 px gap. I set them to 72 px and 10 px to match the mock, and `cargo check` is clean."),
            ],
        );
        s.user("Open the GPUI docs and see if list() supports bottom alignment.");
        s.assistant(
            TurnKind::Spawn,
            MessageStatus::Done,
            vec![
                Part::Status {
                    id: new_id(),
                    code: StatusKind::SubagentWorking,
                    message: "Worked in sandbox · 4 actions · 00:41".into(),
                },
                Part::Tool {
                    id: new_id(),
                    call: ToolCall {
                        id: new_id(),
                        name: "spawn_subagent".into(),
                        input: json!("docs-lookup · prime"),
                    },
                    output_summary: "Subagent finished".into(),
                    output_ref: None,
                    is_error: false,
                    status: Some(ToolStatus::Ok),
                    duration_ms: Some(41_000),
                    subagent_name: Some("docs-lookup".into()),
                },
                Part::Browser {
                    id: new_id(),
                    title: "GPUI docs — ListAlignment".into(),
                    url: "https://docs.rs/gpui/latest/gpui/enum.ListAlignment.html".into(),
                    last_action: "read page".into(),
                    live: false,
                },
                Part::File {
                    id: new_id(),
                    path: "docs/research/gpui.md".into(),
                    lines: 212,
                    blob_ref: Some("blob/seed/gpui-notes".into()),
                },
                text("Yes. `ListAlignment::Bottom` scrolls from the bottom up, like a chat log. Notes are in `docs/research/gpui.md`."),
            ],
        );
        research.commit();
    }
    add_doc(shared, research);

    let launch = ChatDoc::new(CHAT_ARBOS_LAUNCH, PROJECT_ARBOS);
    {
        let mut s = Seeder {
            doc: &launch,
            t: base + 1_200_000,
        };
        s.user("Draft a launch plan for the Mac beta.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![
                text(ASK_LEAD),
                Part::Status {
                    id: new_id(),
                    code: StatusKind::AskUser,
                    message: ASK_QUESTION.into(),
                },
            ],
        );
        launch.commit();
    }
    add_doc(shared, launch);

    let affine = ChatDoc::new(CHAT_AFFINE_MAIN, PROJECT_AFFINE);
    {
        let mut s = Seeder {
            doc: &affine,
            t: base.saturating_sub(23 * 60 * 60_000),
        };
        s.user("Summarise yesterday's design review.");
        s.assistant(
            TurnKind::Reply,
            MessageStatus::Done,
            vec![text(
                "Three decisions: keep the canvas infinite, move export to the share sheet, and drop the legacy sidebar. Two open questions are in the thread.",
            )],
        );
        affine.commit();
    }
    add_doc(shared, affine);

    let comms = ChatDoc::new(CHAT_COMMS_MAIN, PROJECT_COMMS);
    {
        let mut s = Seeder {
            doc: &comms,
            t: base.saturating_sub(2 * 60 * 60_000),
        };
        s.user("Send the weekly update to the team.");
        s.assistant(
            TurnKind::Act,
            MessageStatus::Error,
            vec![
                tool("read_file", "updates/2026-10-13.md", "1.2 KB", 300),
                Part::Error {
                    id: new_id(),
                    code: "sandbox_timeout".into(),
                    message: "Sandbox did not answer in 60 s".into(),
                },
            ],
        );
        comms.commit();
    }
    add_doc(shared, comms);
}
