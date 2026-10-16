//! `AppState`: the one shared entity. Views observe it (`cx.observe`) and
//! subscribe to the narrow [`AppEvent::TranscriptText`] for streaming appends
//! (zeron-study §4.4 step 4). It owns the sync channel; nothing here touches
//! GPUI elements.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{App, Context, EventEmitter, RenderImage, Task};
use gpui_tokio::Tokio;
use tokio::sync::mpsc;

use crate::auth;
use crate::calls::{self, CallCmd, CallEvent};
use crate::model::{
    call_ended_line, reachable_line, AccountView, Call, CallEnded, CallKind, CallMissed, Person,
    Reachable, Sender,
};
use crate::model::{
    new_id, now_ms, AttachmentKind, BoxState, ChannelsView, Chat, ChatKind, ConnectorsView, Effort,
    Id, Link, LocalView, Machine, Message, MessageStatus, Part, Project, ProjectDocView,
    QuestionState, Role, Routine, RoutineRun, RoutineTrigger, Skill, StatusKind, VaultNewItem,
    VaultRequestStatus, VaultView,
};
use crate::sync::{ClientCommand, SyncEvent, SyncSource};
use crate::ui::notify::{self, Gate, Notifier, NotifyEvent, NotifyKind};
use crate::ui::tones::{Tone, TonePlayer};

/// How often held notification batches are re-checked.
const NOTIFY_TICK_MS: u64 = 5_000;
/// The ring card and its sound give up after this long.
const RING_TIMEOUT_MS: u64 = 45_000;
/// An outgoing call nobody answers is hung up after this long.
const RINGBACK_TIMEOUT_MS: u64 = 60_000;
/// How long a `call_missed` push waits for its chat to load before it is dropped.
const MISSED_PENDING_MS: u64 = 120_000;
/// Presence frames come every 15 s; three missed = offline (relay §7c uses 45 s too).
const PRESENCE_ONLINE_MS: u64 = 45_000;

/// Which profile sheet is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfilePage {
    /// A person, by handle; `space_id` = the chat it was opened from.
    User {
        handle: String,
        space_id: Option<Id>,
    },
    /// A people group chat or a shared project.
    Group { space_id: Id },
    /// An agent (one of my projects or a shared one).
    Agent { project_id: Id },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallTarget {
    User(String),
    Agent(Id),
}

fn muted_path() -> PathBuf {
    crate::sync::credentials::config_dir().join("muted.json")
}

/// Multi-chat (several chats per agent: chips, ⌘T, agent groups) is off by
/// default. `ARBOS_MULTI_CHAT=1` or `{"multi_chat": true}` in
/// `<config>/settings.json` turns it back on.
fn multi_chat_enabled() -> bool {
    if let Ok(v) = std::env::var("ARBOS_MULTI_CHAT") {
        return matches!(v.trim(), "1" | "true" | "yes" | "on");
    }
    std::fs::read_to_string(crate::sync::credentials::config_dir().join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("multi_chat").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

fn load_muted() -> HashSet<Id> {
    std::fs::read_to_string(muted_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_muted(muted: &HashSet<Id>) {
    let dir = crate::sync::credentials::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string(&muted.iter().collect::<Vec<_>>()) {
        let _ = std::fs::write(muted_path(), json);
    }
}

/// Telegram wording, shared with the iPhone: "last seen 3 min ago" (< 1 h),
/// "last seen today at 14:05" (same day), "last seen N days ago", and
/// "last seen recently" when the stamp is older than a month.
pub fn last_seen_text(at_ms: u64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let now = now_ms();
    let ago = now.saturating_sub(at_ms);
    if ago < 60_000 {
        return "last seen just now".into();
    }
    if ago < 3_600_000 {
        return format!("last seen {} min ago", ago / 60_000);
    }
    let (Some(t), Some(n)) = (
        Local.timestamp_millis_opt(at_ms as i64).single(),
        Local.timestamp_millis_opt(now as i64).single(),
    ) else {
        return "last seen recently".into();
    };
    if t.date_naive() == n.date_naive() {
        return format!("last seen today at {}", t.format("%H:%M"));
    }
    let days = (n.date_naive() - t.date_naive()).num_days().max(1);
    if days == 1 {
        return format!("last seen yesterday at {}", t.format("%H:%M"));
    }
    if days <= 30 {
        return format!("last seen {days} days ago");
    }
    let _ = t.year();
    "last seen recently".into()
}

/// "3 min ago", "2 h ago", "yesterday", "5 days ago".
pub fn ago_text(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        "just now".into()
    } else if s < 3_600 {
        format!("{} min ago", s / 60)
    } else if s < 86_400 {
        format!("{} h ago", s / 3_600)
    } else if s < 2 * 86_400 {
        "yesterday".into()
    } else {
        format!("{} days ago", s / 86_400)
    }
}
/// A chat's first snapshot only announces messages younger than this.
const SNAPSHOT_FRESH_MS: u64 = 30_000;
use crate::update::{self, UpdateCommand, UpdateEvent, UpdatePhase, UpdateState};

/// How the sign-in screen signs this device in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInMode {
    /// "Sign in with GitHub" through the relay's OAuth flow.
    GitHub,
    /// The relay runs `ARBOS_AUTH_MODE=paircode`: one-time code from its log.
    PairCode,
}

/// Sign-in screen state (first launch, after sign-out, after a revoked token).
#[derive(Clone, Debug)]
pub struct Pairing {
    pub relay_url: String,
    pub mode: SignInMode,
    pub busy: bool,
    pub error: Option<String>,
}

/// A downloaded blob shown in the Details drawer.
#[derive(Clone, Debug)]
pub struct BlobView {
    pub mime: String,
    pub bytes: Arc<Vec<u8>>,
    pub saved_to: Option<PathBuf>,
}

/// Transient bottom-right message.
#[derive(Clone, Debug)]
pub struct Notice {
    pub text: String,
    pub at: u64,
}

pub const NOTICE_TTL_MS: u64 = 6_000;

#[derive(Clone, Debug)]
pub enum AppEvent {
    /// Pure text append on a streaming message; only the transcript re-syncs.
    TranscriptText { chat_id: Id },
    /// Anything else in a chat changed.
    TranscriptChanged { chat_id: Id },
    /// F11: speech-to-text finished for a `transcribe` request.
    VoiceTranscript {
        request_id: u64,
        text: String,
        error: Option<String>,
        mock: bool,
    },
    /// F11: text-to-speech finished for a `speak` request.
    VoiceSpeech {
        request_id: u64,
        bytes: Arc<Vec<u8>>,
        mime: String,
        error: Option<String>,
        mock: bool,
    },
    /// F11: the speaker icon on a reply was clicked.
    ReplaySpeech { message_id: Id },
}

/// What the composer should look like for the active chat (§3.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerMode {
    /// Send arrow. Sending is always allowed: the agent multitasks, and a
    /// running turn shows as a small indicator with its own Stop.
    Send,
    /// S8: the agent asked a question.
    Answer,
    /// S18: an approval card is pending; the composer waits for Approve/Reject.
    Approve,
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
    pub pairing: Option<Pairing>,
    pub device_id: Option<String>,
    /// Signed-in account (`/auth/token` user, then `/me`).
    pub account: Option<AccountView>,
    /// The tree mark (the macOS app icon), drawn on the sign-in screen.
    pub mark: Option<Arc<RenderImage>>,
    /// People chats (D72): the new-chat sheet, directory results, typing, avatars.
    pub new_chat_open: bool,
    pub people_query: String,
    pub people_results: Vec<Person>,
    pub people_busy: bool,
    pub people_error: Option<String>,
    /// handle → until (ms) per chat; shown as "@sam is typing…".
    pub typing: HashMap<Id, (String, u64)>,
    /// Decoded avatars by `avatar_url`; `None` while a fetch is in flight.
    pub avatars: HashMap<String, Option<Arc<RenderImage>>>,
    /// A DM just started from the sheet: select it when it arrives.
    awaiting_dm: Option<String>,
    /// The sheet invites into this project instead of starting a DM (step 3).
    pub invite_target: Option<Id>,
    /// Avatar reply reaction window per project (ms): plays once after a turn.
    pub avatar_reply_until: HashMap<Id, u64>,
    /// Avatar picker open for this project.
    pub avatar_picker: Option<Id>,
    /// Last time we told the room we are typing, per chat.
    typing_sent_at: HashMap<Id, u64>,
    /// Set by a fresh sign-in: with no projects yet, create "Home" and land in it.
    first_project_pending: bool,
    pub projects: Vec<Project>,
    /// Picker contents; empty = the fixture's static list.
    pub models: Vec<String>,
    pub project_docs: HashMap<Id, ProjectDocView>,
    /// The skills library (F5), as last fetched from the source.
    pub skills: Vec<Skill>,
    pub chats: HashMap<Id, Vec<Chat>>,
    pub messages: HashMap<Id, Vec<Message>>,
    pub chat_ui: HashMap<Id, ChatUi>,
    pub blobs: HashMap<String, BlobView>,
    /// Decoded inline thumbnails of image attachments, by blob key (F10).
    pub thumbnails: HashMap<String, Arc<RenderImage>>,
    /// Thumbnail fetches in flight or failed (never asked twice).
    thumbnails_requested: HashSet<String>,
    pub browser_frame: Option<Arc<RenderImage>>,
    pub browser_frames_seen: u64,
    pub notices: Vec<Notice>,
    pub selected_project: Option<Id>,
    /// Bytes and chats kept on this Mac by the on-device store (Settings → About).
    pub store_usage: (i64, i64),
    pub active_tab: HashMap<Id, Id>,
    /// Feature flag: several chats per agent. Off → one chat per agent, the
    /// primary one (`primary_chat()`); extra chats stay in the data, unseen.
    pub multi_chat: bool,
    pub link: Link,
    pub rail_collapsed: bool,
    pub right_panel_open: bool,
    pub details_open: Option<DetailsTarget>,
    pub new_project_open: bool,
    /// Debug: per-message "Show details" disclosures in the transcript.
    pub show_details: bool,
    /// The user's one machine (projects are workspaces on it).
    pub machine: Machine,
    /// Settings → Connectors (F1).
    pub connectors: ConnectorsView,
    /// Settings → Vault (F8).
    pub vault: VaultView,
    /// Settings → Routines, per project (F3).
    pub routines: HashMap<Id, Vec<Routine>>,
    /// Run history per routine, newest first.
    pub routine_runs: HashMap<Id, Vec<RoutineRun>>,
    /// System notifications (D49); the Shell flips `window_active`.
    pub notifier: Notifier,
    pub window_active: bool,
    /// One-time explainer before the OS asks for notification permission.
    pub notify_prompt: bool,
    /// Telegram-style profile sheet (user / group / agent) and its tab.
    pub profile: Option<ProfilePage>,
    pub profile_tab: usize,
    /// Agent page: the Appearance (tree + colour) section is unfolded.
    pub profile_picker_open: bool,
    /// Agent page / user page "more" menu is open.
    pub profile_more_open: bool,
    /// Agent page: the owner is editing the description inline.
    pub profile_editing_description: bool,
    /// Spaces whose notifications are muted (`<config>/muted.json`).
    pub muted: HashSet<Id>,
    /// Last presence frame seen per handle (ms): "online" while fresh.
    pub presence_seen: HashMap<String, u64>,
    /// Answered this session: never re-arm it on a later sync, even when the
    /// marker file could not be written.
    notify_prompt_answered: bool,
    /// Last `FocusChat` sent to the source (dedupe).
    focus_sent: Option<Option<Id>>,
    /// Settings → Channels (F7).
    pub channels: ChannelsView,
    /// In-app update (D42): what the rail button and settings show.
    pub update: UpdateState,
    /// Settings → This Mac (F9).
    pub local: LocalView,
    /// F11: replies are spoken and a mic transcript sends at once.
    pub voice_mode: bool,
    /// F11: assistant messages whose speech is cached (speaker icon replays).
    pub spoken: HashSet<Id>,
    awaiting_new_project: bool,
    /// Calls v1: live calls I know about per chat (the "On a call · Join" bar).
    pub calls_in_chat: HashMap<Id, Call>,
    /// Someone is calling me (ringing): the call and who started it.
    pub incoming_call: Option<(Call, Sender)>,
    /// When the ring card came up (auto-dismiss after `RING_TIMEOUT_MS`).
    incoming_since: Option<u64>,
    /// The ring (callee) and ringback (caller) tones while they should sound.
    ring_tone: Option<TonePlayer>,
    ringback_tone: Option<TonePlayer>,
    /// `call_missed` ids whose "Missed call from …" line this device wrote.
    missed_written: HashSet<String>,
    /// `call_missed` pushes that arrived before their chat was loaded (with when).
    missed_pending: Vec<(CallMissed, u64)>,
    /// Call ids whose "Call ended" line this device wrote (the leave answer and
    /// the `call_ended` push can both name this device as the writer).
    ended_written: HashSet<String>,
    /// The call this device is in.
    pub my_call: Option<ActiveCall>,
    commands: mpsc::UnboundedSender<ClientCommand>,
    update_commands: mpsc::UnboundedSender<UpdateCommand>,
    call_commands: mpsc::UnboundedSender<CallCmd>,
    _pump: Task<()>,
    _notify_tick: Task<()>,
    _update_pump: Task<()>,
    _call_pump: Task<()>,
}

/// One tile in a call.
#[derive(Clone, Debug)]
pub struct CallParticipant {
    /// LiveKit identity (`u_<user_id>`); `local` for me.
    pub identity: String,
    pub sender: Sender,
    pub speaking: bool,
    pub muted: bool,
    pub video: Option<Arc<RenderImage>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallStatus {
    Connecting,
    Connected,
    Failed(String),
}

/// The call this device is in, as the UI shows it.
#[derive(Clone, Debug)]
pub struct ActiveCall {
    pub call: Call,
    pub status: CallStatus,
    pub muted: bool,
    pub camera: bool,
    pub participants: Vec<CallParticipant>,
    pub local_video: Option<Arc<RenderImage>>,
    pub joined_at: u64,
    /// Full call screen (true) or the slim bar under the header (false).
    pub expanded: bool,
    /// One line about a problem (no mic, no camera, reconnecting).
    pub notice: Option<String>,
    /// I started this call: ringback plays until someone else is in the room.
    pub outgoing: bool,
    /// From the 201 start response: "Robert's Arbos isn't open — they'll see a
    /// missed call" / "Ringing Robert's iPhone" while nobody has picked up
    /// (`None` = they are live).
    pub reachable: Option<String>,
}

impl ActiveCall {
    pub fn participant_mut(&mut self, identity: &str) -> Option<&mut CallParticipant> {
        self.participants
            .iter_mut()
            .find(|p| p.identity == identity)
    }
}

/// What the Details drawer (stub) describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailsTarget {
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

        let notify_tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(NOTIFY_TICK_MS))
                .await;
            if this.update(cx, |state, cx| state.notify_tick(cx)).is_err() {
                break;
            }
        });

        let update_state = UpdateState::new();
        let (update_event_tx, mut update_event_rx) = mpsc::unbounded_channel::<UpdateEvent>();
        let (update_command_tx, update_command_rx) = mpsc::unbounded_channel::<UpdateCommand>();
        let update_worker = Tokio::spawn(
            cx,
            update::run(update_state.clone(), update_command_rx, update_event_tx),
        );
        let update_pump = cx.spawn(async move |this, cx| {
            let _worker = update_worker;
            while let Some(event) = update_event_rx.recv().await {
                let applied = this.update(cx, |state, cx| state.apply_update(event, cx));
                if applied.is_err() {
                    break;
                }
            }
        });
        let (call_event_tx, mut call_event_rx) = mpsc::unbounded_channel::<CallEvent>();
        let (call_command_tx, call_command_rx) = mpsc::unbounded_channel::<CallCmd>();
        let call_worker = Tokio::spawn(cx, calls::run(call_command_rx, call_event_tx));
        let call_pump = cx.spawn(async move |this, cx| {
            let _worker = call_worker;
            while let Some(event) = call_event_rx.recv().await {
                let applied = this.update(cx, |state, cx| state.apply_call_event(event, cx));
                if applied.is_err() {
                    break;
                }
            }
        });

        Self {
            source_name,
            pairing: None,
            account: None,
            mark: decode_image(include_bytes!(
                "../../../site/assets/logo/arbos-macos-icon-1024.png"
            ))
            .map(Arc::new),
            first_project_pending: false,
            new_chat_open: false,
            people_query: String::new(),
            people_results: Vec::new(),
            people_busy: false,
            people_error: None,
            typing: HashMap::new(),
            avatars: HashMap::new(),
            awaiting_dm: None,
            invite_target: None,
            avatar_reply_until: HashMap::new(),
            avatar_picker: None,
            typing_sent_at: HashMap::new(),
            device_id: None,
            projects: Vec::new(),
            models: Vec::new(),
            project_docs: HashMap::new(),
            skills: Vec::new(),
            chats: HashMap::new(),
            messages: HashMap::new(),
            chat_ui: HashMap::new(),
            blobs: HashMap::new(),
            thumbnails: HashMap::new(),
            thumbnails_requested: HashSet::new(),
            browser_frame: None,
            browser_frames_seen: 0,
            notices: Vec::new(),
            selected_project: None,
            store_usage: (0, 0),
            active_tab: HashMap::new(),
            multi_chat: multi_chat_enabled(),
            link: Link::Connecting,
            rail_collapsed: false,
            right_panel_open: false,
            details_open: None,
            new_project_open: false,
            show_details: false,
            machine: Machine::default(),
            connectors: ConnectorsView::default(),
            vault: VaultView::default(),
            routines: HashMap::new(),
            routine_runs: HashMap::new(),
            notifier: Notifier::new(),
            window_active: true,
            notify_prompt: false,
            profile: None,
            profile_tab: 0,
            profile_picker_open: false,
            profile_more_open: false,
            profile_editing_description: false,
            muted: load_muted(),
            presence_seen: HashMap::new(),
            notify_prompt_answered: false,
            focus_sent: None,
            channels: ChannelsView::default(),
            update: update_state,
            local: LocalView::default(),
            voice_mode: false,
            spoken: HashSet::new(),
            awaiting_new_project: false,
            commands: command_tx,
            update_commands: update_command_tx,
            call_commands: call_command_tx,
            calls_in_chat: HashMap::new(),
            incoming_call: None,
            incoming_since: None,
            ring_tone: None,
            ringback_tone: None,
            missed_written: HashSet::new(),
            missed_pending: Vec::new(),
            ended_written: HashSet::new(),
            my_call: None,
            _pump: pump,
            _notify_tick: notify_tick,
            _update_pump: update_pump,
            _call_pump: call_pump,
        }
    }

    fn apply_update(&mut self, event: UpdateEvent, cx: &mut Context<Self>) {
        match event {
            UpdateEvent::Checking => {
                if matches!(self.update.phase, UpdatePhase::Idle | UpdatePhase::UpToDate) {
                    self.update.phase = UpdatePhase::Checking;
                }
            }
            UpdateEvent::UpToDate => {
                self.update.last_checked_at = Some(now_ms());
                if !self.update.busy() {
                    self.update.phase = UpdatePhase::UpToDate;
                }
            }
            UpdateEvent::Available(available) => {
                let available = *available;
                self.update.last_checked_at = Some(now_ms());
                if !self.update.busy() {
                    let fresh = self.update.available() != Some(&available);
                    self.update.phase = UpdatePhase::Available(available.clone());
                    if fresh {
                        self.push_notice(format!(
                            "Arbos {} is ready to install — press Update in the rail.",
                            available.item.version
                        ));
                    }
                }
            }
            UpdateEvent::Progress { got, total } => {
                if let Some(available) = self.update.available().cloned() {
                    self.update.phase = UpdatePhase::Downloading {
                        available,
                        got,
                        total,
                    };
                }
            }
            UpdateEvent::Installing => {
                if let Some(available) = self.update.available().cloned() {
                    self.update.phase = UpdatePhase::Installing(available);
                }
            }
            UpdateEvent::Installed(installed) => {
                if let Some(available) = self.update.available().cloned() {
                    self.update.phase = UpdatePhase::Restarting(available);
                }
                if self.update.needs_relocation {
                    self.update.needs_relocation = false;
                    self.push_notice(format!(
                        "Arbos now lives in {}; the copy you opened it from can be deleted.",
                        installed
                            .path()
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "Applications".into())
                    ));
                }
                // One frame shows "Restarting", then the relauncher takes over.
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(700))
                        .await;
                    let _ = this.update(cx, |state, cx| match update::relaunch(&installed) {
                        Ok(()) => cx.quit(),
                        Err(err) => {
                            update::log(format!("relaunch failed: {err:#}"));
                            state.push_notice(
                                "Arbos was updated but could not relaunch itself — quit and open Arbos again.",
                            );
                            state.update.phase = UpdatePhase::Failed {
                                message: format!("installed, but could not relaunch: {err:#}"),
                                available: state.update.available().cloned(),
                            };
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            UpdateEvent::Failed(message) => {
                // A quiet "Retry" in the rail is not enough: say what went wrong and offer the
                // download page.
                self.push_notice(format!(
                    "Update failed: {message} — press Retry, or get the newest version from {}.",
                    update::DOWNLOAD_URL
                ));
                self.update.phase = UpdatePhase::Failed {
                    available: self.update.available().cloned(),
                    message,
                };
            }
        }
        cx.notify();
    }

    pub fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if self.update.busy() {
            return;
        }
        let _ = self.update_commands.send(UpdateCommand::Check);
        cx.notify();
    }

    /// The failure notice's fallback: open arbos.life/download/mac.
    pub fn open_download_page(&mut self, cx: &mut Context<Self>) {
        update::open_download_page();
        cx.notify();
    }

    /// "Move to Applications" (translocated / read-only copy): copy, clear quarantine, relaunch.
    pub fn move_to_applications(&mut self, cx: &mut Context<Self>) {
        if self.update.busy() {
            return;
        }
        let _ = self.update_commands.send(UpdateCommand::MoveToApplications);
        cx.notify();
    }

    /// The rail button: download, verify, swap, relaunch in one click. From a translocated or
    /// read-only copy the new version is installed into Applications instead.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        if self.update.busy() {
            return;
        }
        if self.update.button_moves() {
            self.move_to_applications(cx);
            return;
        }
        let Some(available) = self.update.available().cloned() else {
            return;
        };
        self.update.phase = UpdatePhase::Downloading {
            available,
            got: 0,
            total: None,
        };
        let _ = self.update_commands.send(UpdateCommand::Install);
        cx.notify();
    }

    fn apply(&mut self, event: SyncEvent, cx: &mut Context<Self>) {
        match event {
            SyncEvent::NeedsPairing { relay_url } => {
                let mode = self
                    .pairing
                    .as_ref()
                    .map(|p| p.mode)
                    .unwrap_or(SignInMode::GitHub);
                self.pairing = Some(Pairing {
                    relay_url,
                    mode,
                    busy: false,
                    error: None,
                });
                self.device_id = None;
                cx.notify();
            }
            SyncEvent::Paired { device_id } => {
                self.pairing = None;
                self.device_id = Some(device_id);
                self.maybe_prompt_notifications();
                // Land in a chat: if this account has no project yet, make "Home".
                self.first_project_pending = true;
                cx.notify();
            }
            SyncEvent::Account(account) => {
                self.account = account;
                if self.account.is_some() {
                    self.maybe_prompt_notifications();
                }
                cx.notify();
            }
            SyncEvent::PeopleSearch { query, results } => {
                if query == self.people_query {
                    self.people_results = results;
                    self.people_busy = false;
                    let urls: Vec<String> = self
                        .people_results
                        .iter()
                        .map(|p| p.avatar_url.clone())
                        .collect();
                    for url in urls {
                        self.want_avatar(url);
                    }
                }
                cx.notify();
            }
            SyncEvent::CallReady {
                call,
                created,
                reachable,
            } => self.on_call_ready(call, created, reachable, cx),
            SyncEvent::CallStarted { call, sender } => self.on_call_started(call, sender, cx),
            SyncEvent::CallEnded(ended) => self.on_call_ended(ended, cx),
            SyncEvent::CallMissed(missed) => self.on_call_missed(missed, cx),
            SyncEvent::CallInfo(call) => self.on_call_info(call, cx),
            SyncEvent::CallSeen { chat_id, call_id } => {
                let known = self
                    .calls_in_chat
                    .get(&chat_id)
                    .map(|c| c.call_id == call_id)
                    .unwrap_or(false);
                if !known {
                    self.command(ClientCommand::FetchCall { call_id });
                }
            }
            SyncEvent::CallError { chat_id, message } => {
                let _ = chat_id;
                if matches!(
                    self.my_call.as_ref().map(|c| &c.status),
                    Some(CallStatus::Connecting)
                ) {
                    self.my_call = None;
                }
                self.push_notice(message);
                cx.notify();
            }
            SyncEvent::PeopleError(err) => {
                self.people_busy = false;
                self.people_error = Some(err);
                self.awaiting_dm = None;
                cx.notify();
            }
            SyncEvent::Presence { handle } => {
                self.presence_seen.insert(handle, now_ms());
                if self.profile.is_some() {
                    cx.notify();
                }
            }
            SyncEvent::Typing {
                chat_id,
                handle,
                active,
            } => {
                if active {
                    self.typing.insert(chat_id, (handle, now_ms() + 6_000));
                } else {
                    self.typing.remove(&chat_id);
                }
                cx.notify();
            }
            SyncEvent::Avatar { url, bytes } => {
                let image = decode_image(&bytes).map(Arc::new);
                self.avatars.insert(url, image);
                cx.notify();
            }
            SyncEvent::PairingFailed(err) => {
                if let Some(p) = &mut self.pairing {
                    p.busy = false;
                    p.error = Some(err);
                }
                cx.notify();
            }
            SyncEvent::ChatCreated {
                project_id,
                local_id,
                chat,
            } => {
                let tabs = self.chats.entry(project_id.clone()).or_default();
                match tabs.iter_mut().find(|c| c.id == local_id) {
                    Some(slot) => *slot = chat.clone(),
                    None => tabs.push(chat.clone()),
                }
                if let Some(msgs) = self.messages.remove(&local_id) {
                    self.messages.insert(chat.id.clone(), msgs);
                }
                if let Some(ui) = self.chat_ui.remove(&local_id) {
                    self.chat_ui.insert(chat.id.clone(), ui);
                }
                if self.active_tab.get(&project_id) == Some(&local_id) {
                    self.active_tab.insert(project_id, chat.id.clone());
                    self.command(ClientCommand::OpenChat {
                        chat_id: chat.id.clone(),
                    });
                }
                cx.emit(AppEvent::TranscriptChanged { chat_id: chat.id });
                cx.notify();
            }
            SyncEvent::ProjectDoc { project_id, view } => {
                self.project_docs.insert(project_id, view);
                cx.notify();
            }
            SyncEvent::Blob {
                project_id,
                key,
                mime,
                bytes,
                saved_to,
            } => {
                if self.selected_project.as_deref() != Some(project_id.as_str()) {
                    return;
                }
                if let Some(path) = &saved_to {
                    self.push_notice(format!("Saved to {}", path.display()));
                }
                self.blobs.insert(
                    key.clone(),
                    BlobView {
                        mime,
                        bytes,
                        saved_to,
                    },
                );
                self.details_open = Some(DetailsTarget::Blob(key));
                cx.notify();
            }
            SyncEvent::Thumbnail { key, bytes } => {
                if let Some(image) = decode_image(&bytes) {
                    self.thumbnails.insert(key, Arc::new(image));
                    if let Some(chat_id) = self.active_chat_id() {
                        cx.emit(AppEvent::TranscriptChanged { chat_id });
                    }
                    cx.notify();
                }
            }
            SyncEvent::AttachmentSaved { path } => {
                self.push_notice(format!("Saved to {}", path.display()));
                crate::ui::attachments::reveal(&path);
                cx.notify();
            }
            SyncEvent::BrowserFrame { project_id, jpeg } => {
                if self.selected_project.as_deref() != Some(project_id.as_str()) {
                    return;
                }
                if let Some(image) = decode_image(&jpeg) {
                    self.browser_frame = Some(Arc::new(image));
                    self.browser_frames_seen += 1;
                    cx.notify();
                }
            }
            SyncEvent::Notice(text) => {
                self.push_notice(text);
                cx.notify();
            }
            SyncEvent::Models(models) => {
                self.models = models;
                cx.notify();
            }
            SyncEvent::Projects(projects) => {
                // Signed in and synced (also the path without a sign-in event).
                if !projects.is_empty() {
                    self.maybe_prompt_notifications();
                }
                for p in &projects {
                    let crossed = p.spend_cap_usd > 0.0 && p.spend_today_usd >= p.spend_cap_usd;
                    let was_below = self
                        .project(&p.id)
                        .map(|old| old.spend_today_usd < old.spend_cap_usd)
                        .unwrap_or(false);
                    if crossed && was_below {
                        self.notify(
                            p.id.clone(),
                            None,
                            NotifyKind::SpendCapReached,
                            format!("${:.2} of ${:.2} today", p.spend_today_usd, p.spend_cap_usd),
                        );
                    }
                }
                for p in &projects {
                    for person in &p.people {
                        let url = person.avatar_url.clone();
                        self.want_avatar(url);
                    }
                }
                if let Some(handle) = self.awaiting_dm.clone() {
                    if let Some(space) = projects
                        .iter()
                        .find(|p| p.is_people() && p.people.iter().any(|x| x.handle == handle))
                    {
                        self.awaiting_dm = None;
                        self.new_chat_open = false;
                        let id = space.id.clone();
                        self.projects = projects.clone();
                        self.select_project(id, cx);
                    }
                }
                if self.first_project_pending {
                    self.first_project_pending = false;
                    if projects.is_empty() && !self.awaiting_new_project {
                        self.awaiting_new_project = true;
                        self.command(ClientCommand::CreateProject {
                            name: "Home".into(),
                        });
                    }
                }
                // A project this device just created becomes the selection.
                if self.awaiting_new_project {
                    let known: HashSet<Id> = self.projects.iter().map(|p| p.id.clone()).collect();
                    if let Some(fresh) = projects.iter().find(|p| !known.contains(&p.id)) {
                        self.awaiting_new_project = false;
                        let id = fresh.id.clone();
                        self.projects = projects.clone();
                        self.select_project(id, cx);
                    }
                }
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
            SyncEvent::Machine(machine) => {
                self.machine = machine;
                cx.notify();
            }
            SyncEvent::RestoreSelection {
                project_id,
                chat_id,
            } => {
                // Cold launch from the on-device store: back to the chat that
                // was open, instead of the default pick.
                if self.projects.iter().any(|p| p.id == project_id) {
                    if self.tabs(&project_id).iter().any(|c| c.id == chat_id) {
                        self.active_tab.insert(project_id.clone(), chat_id);
                    }
                    // select_project() coerces to the primary chat in single-chat mode.
                    self.select_project(project_id, cx);
                }
            }
            SyncEvent::StoreUsage { bytes, chats } => {
                self.store_usage = (bytes, chats);
                cx.notify();
            }
            SyncEvent::ProjectStatus {
                project_id,
                state,
                detail,
                progress,
            } => {
                let failed = state == BoxState::Error
                    && self
                        .project(&project_id)
                        .is_some_and(|p| p.state != BoxState::Error);
                if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
                    p.state = state;
                    p.state_detail = detail.clone();
                    p.wake_progress = progress;
                }
                if failed {
                    self.notify(
                        project_id,
                        None,
                        NotifyKind::WakeFailed,
                        detail.unwrap_or_else(|| "The box did not come up".into()),
                    );
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
                        self.active_tab.insert(project_id.clone(), first.id.clone());
                    }
                }
                self.chats.insert(project_id.clone(), chats);
                self.enforce_single_chat(&project_id);
                cx.notify();
            }
            SyncEvent::ChatSnapshot { chat_id, messages } => {
                let dropped = !messages.is_empty();
                let ui = self.chat_ui.entry(chat_id.clone()).or_default();
                if dropped {
                    ui.composer_dropped = true;
                }
                // Whole messages inserted at once (a person's line, a shared
                // agent's reply) arrive as a new snapshot, not an upsert: tell
                // the notifier about the ones we have not seen. A chat's first
                // snapshot is history, except what was written just now.
                let seen: HashSet<Id> = self
                    .messages
                    .get(&chat_id)
                    .map(|l| l.iter().map(|m| m.id.clone()).collect())
                    .unwrap_or_default();
                let first_load = !self.messages.contains_key(&chat_id);
                let fresh_after = now_ms().saturating_sub(SNAPSHOT_FRESH_MS);
                let arrived: Vec<Message> = messages
                    .iter()
                    .filter(|m| !seen.contains(&m.id))
                    .filter(|m| !first_load || m.created_at >= fresh_after)
                    .filter(|m| !m.is_streaming())
                    .cloned()
                    .collect();
                self.messages.insert(chat_id.clone(), messages);
                // Message counts decide an agent's primary chat (single-chat mode).
                if let Some(project_id) = self.project_of_chat(&chat_id).map(|p| p.id.clone()) {
                    self.enforce_single_chat(&project_id);
                }
                for m in &arrived {
                    self.notify_for_message(&chat_id, m, false, false, true);
                }
                self.request_thumbnails(&chat_id);
                cx.emit(AppEvent::TranscriptChanged { chat_id });
                cx.notify();
            }
            SyncEvent::MessageUpsert { chat_id, message } => {
                let message = *message;
                self.request_thumbnails_for(&chat_id, std::slice::from_ref(&message));
                let list = self.messages.entry(chat_id.clone()).or_default();
                let mut text_only = false;
                let mut appended = 0;
                let mut was_streaming = false;
                let mut had_open_question = false;
                let mut is_new = false;
                match list.iter_mut().find(|m| m.id == message.id) {
                    Some(existing) => {
                        let old_len = existing.text_len();
                        let new_len = message.text_len();
                        text_only = existing.status == message.status
                            && existing.parts.len() == message.parts.len()
                            && new_len > old_len
                            && same_shape(existing, &message);
                        appended = new_len.saturating_sub(old_len);
                        was_streaming = existing.is_streaming();
                        had_open_question = needs_input(existing);
                        *existing = message.clone();
                    }
                    None => {
                        is_new = true;
                        list.push(message.clone());
                    }
                }
                self.notify_for_message(
                    &chat_id,
                    &message,
                    was_streaming,
                    had_open_question,
                    is_new,
                );
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
            SyncEvent::Connectors(view) => {
                self.connectors = view;
                cx.notify();
            }
            SyncEvent::ConnectorAuth { provider, auth_url } => {
                cx.open_url(&auth_url);
                self.push_notice(format!(
                    "Opened the browser to connect {provider}; the relay finishes the sign-in."
                ));
                cx.notify();
            }
            SyncEvent::Vault(view) => {
                self.vault = view;
                cx.notify();
            }
            SyncEvent::Routines {
                project_id,
                routines,
            } => {
                self.routines.insert(project_id, routines);
                cx.notify();
            }
            SyncEvent::RoutineRuns { routine_id, runs } => {
                self.routine_runs.insert(routine_id, runs);
                cx.notify();
            }
            SyncEvent::Skills(mut skills) => {
                skills.sort_by_key(|s| s.name.to_lowercase());
                self.skills = skills;
                cx.notify();
            }
            SyncEvent::Channels(view) => {
                self.channels = view;
                cx.notify();
            }
            SyncEvent::LocalDevices(view) => {
                self.local = view;
                cx.notify();
            }
            SyncEvent::Transcript {
                request_id,
                text,
                error,
                mock,
            } => {
                cx.emit(AppEvent::VoiceTranscript {
                    request_id,
                    text,
                    error,
                    mock,
                });
            }
            SyncEvent::Speech {
                request_id,
                bytes,
                mime,
                error,
                mock,
            } => {
                cx.emit(AppEvent::VoiceSpeech {
                    request_id,
                    bytes,
                    mime,
                    error,
                    mock,
                });
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

    /// The chats the UI may show for a space: all of them with multi-chat on,
    /// or for a people chat; just the primary chat of an agent otherwise.
    pub fn visible_tabs(&self, project_id: &str) -> Vec<&Chat> {
        let is_people = self.project(project_id).is_some_and(|p| p.is_people());
        if self.multi_chat || is_people {
            self.tabs(project_id).iter().collect()
        } else {
            self.primary_chat(project_id).into_iter().collect()
        }
    }

    /// The one chat of an agent. The relay's `primary_chat` wins when it names a
    /// chat we have; otherwise the chat with the most messages, ties → earliest
    /// created (ULID order). Groups and system tabs only when nothing else exists.
    pub fn primary_chat(&self, project_id: &str) -> Option<&Chat> {
        let tabs = self.tabs(project_id);
        if let Some(named) = self
            .project(project_id)
            .and_then(|p| p.primary_chat.as_deref())
            .and_then(|id| tabs.iter().find(|c| c.id == id))
        {
            return Some(named);
        }
        let plain: Vec<&Chat> = tabs
            .iter()
            .filter(|c| !c.is_group() && !c.is_system)
            .collect();
        let pool = if plain.is_empty() {
            tabs.iter().collect()
        } else {
            plain
        };
        pool.into_iter().min_by(|a, b| {
            let (na, nb) = (self.messages_of(&a.id).len(), self.messages_of(&b.id).len());
            nb.cmp(&na).then_with(|| a.id.cmp(&b.id))
        })
    }

    /// Single-chat mode: the active tab of an agent is always its primary chat.
    /// Returns true when the active tab changed (and the chat was opened).
    fn enforce_single_chat(&mut self, project_id: &str) -> bool {
        if self.multi_chat || self.project(project_id).is_none_or(|p| p.is_people()) {
            return false;
        }
        let Some(primary) = self.primary_chat(project_id).map(|c| c.id.clone()) else {
            return false;
        };
        if self.active_tab.get(project_id) == Some(&primary) {
            return false;
        }
        self.active_tab
            .insert(project_id.to_string(), primary.clone());
        if self.selected_project.as_deref() == Some(project_id) {
            self.command(ClientCommand::OpenChat { chat_id: primary });
        }
        true
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
        self.visible_tabs(project_id)
            .iter()
            .filter(|c| c.unread)
            .count()
    }

    /// S8: the last assistant message ended with an `ask_user` question and
    /// nothing has been sent since. A typed widget question is answered from
    /// its card, so it does not switch the composer.
    pub fn chat_needs_answer(&self, chat_id: &str) -> bool {
        match self.messages_of(chat_id).last() {
            Some(m) if m.role == Role::Assistant && !m.is_streaming() => {
                if m.has_question_part() {
                    m.open_questions()
                        .any(|p| matches!(p, Part::Question { widget: None, .. }))
                } else {
                    m.parts.iter().any(|p| {
                        matches!(
                            p,
                            Part::Status {
                                code: StatusKind::AskUser,
                                ..
                            }
                        )
                    })
                }
            }
            Some(_) | None => false,
        }
    }

    /// The newest open plain question's id in the last assistant message (S8 composer answer).
    fn open_plain_question_id(&self, chat_id: &str) -> Option<String> {
        let m = self.messages_of(chat_id).last()?;
        m.open_questions().rev().find_map(|p| match p {
            Part::Question {
                question_id,
                widget: None,
                ..
            } => Some(question_id.clone()),
            _ => None,
        })
    }

    /// S18: the newest assistant message carries an approval card nobody has decided yet.
    pub fn chat_needs_approval(&self, chat_id: &str) -> bool {
        match self.messages_of(chat_id).last() {
            Some(m) if m.role == Role::Assistant && !m.is_streaming() => {
                m.pending_approvals().next().is_some()
            }
            Some(_) | None => false,
        }
    }

    pub fn composer_mode(&self, chat_id: &str) -> ComposerMode {
        if self.chat_needs_answer(chat_id) {
            ComposerMode::Answer
        } else if self.chat_needs_approval(chat_id) {
            ComposerMode::Approve
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
        if self.enforce_single_chat(&project_id) {
            self.sync_focus();
            cx.notify();
            return;
        }
        if let Some(chat_id) = self.active_tab.get(&project_id).cloned() {
            self.command(ClientCommand::OpenChat { chat_id });
        } else if let Some(first) = self.tabs(&project_id).first() {
            let id = first.id.clone();
            self.active_tab.insert(project_id, id.clone());
            self.command(ClientCommand::OpenChat { chat_id: id });
        }
        self.sync_focus();
        cx.notify();
    }

    pub fn select_tab(&mut self, chat_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        // Single-chat: an agent has one chat, whichever one was asked for.
        let chat_id = if self
            .visible_tabs(&project_id)
            .iter()
            .any(|c| c.id == chat_id)
        {
            chat_id
        } else {
            match self.primary_chat(&project_id) {
                Some(primary) if !self.multi_chat => primary.id.clone(),
                _ => chat_id,
            }
        };
        self.active_tab.insert(project_id, chat_id.clone());
        if let Some(chat) = self
            .chats
            .values_mut()
            .flat_map(|v| v.iter_mut())
            .find(|c| c.id == chat_id)
        {
            chat.unread = false;
        }
        self.sync_badge();
        self.command(ClientCommand::MarkRead {
            chat_id: chat_id.clone(),
        });
        self.command(ClientCommand::OpenChat { chat_id });
        self.sync_focus();
        cx.notify();
    }

    pub fn new_tab(&mut self, cx: &mut Context<Self>) {
        if !self.multi_chat {
            return;
        }
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
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
                kind: ChatKind::Chat,
                participants: Vec::new(),
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

    /// F6: a group chat hosted by the selected project with `participant` (and, when the active
    /// tab is already a group, the participant is added there instead).
    pub fn add_project_to_chat(&mut self, participant: Id, cx: &mut Context<Self>) {
        if !self.multi_chat {
            return;
        }
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        if let Some(group) = self.active_chat().filter(|c| c.is_group()).cloned() {
            if group.participants.contains(&participant) {
                return;
            }
            if let Some(slot) = self
                .chats
                .get_mut(&project_id)
                .and_then(|tabs| tabs.iter_mut().find(|c| c.id == group.id))
            {
                slot.participants.push(participant.clone());
            }
            self.command(ClientCommand::GroupAddParticipant {
                project_id,
                chat_id: group.id,
                participant,
            });
            cx.notify();
            return;
        }
        let name = self
            .projects
            .iter()
            .find(|p| p.id == participant)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "project".into());
        let chat_id = new_id();
        self.chats
            .entry(project_id.clone())
            .or_default()
            .push(Chat {
                id: chat_id.clone(),
                project_id: project_id.clone(),
                title: format!("Group · {name}"),
                is_pinned: false,
                is_system: false,
                kind: ChatKind::Group,
                participants: vec![project_id.clone(), participant.clone()],
                unread: false,
                last_message_at: now_ms(),
            });
        self.messages.insert(chat_id.clone(), Vec::new());
        self.chat_ui.insert(chat_id.clone(), ChatUi::default());
        self.active_tab.insert(project_id.clone(), chat_id.clone());
        self.command(ClientCommand::GroupCreate {
            project_id,
            chat_id: chat_id.clone(),
            participants: vec![participant],
            title: None,
        });
        self.command(ClientCommand::OpenChat { chat_id });
        cx.notify();
    }

    /// Returns false for the pinned tab (S3.8: it never closes).
    pub fn close_tab(&mut self, chat_id: Id, cx: &mut Context<Self>) -> bool {
        if !self.multi_chat {
            return false;
        }
        let Some(project_id) = self.selected_project.clone() else {
            return false;
        };
        let tabs = self.chats.entry(project_id.clone()).or_default();
        let Some(ix) = tabs.iter().position(|c| c.id == chat_id) else {
            return false;
        };
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

    pub fn send(&mut self, text: String, attachments: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.send_with(text, attachments, None, cx);
    }

    /// F11: a spoken message; `wav` is kept on the row as an `audio` attachment.
    pub fn send_voice(&mut self, text: String, wav: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        self.send_with(text, Vec::new(), Some(wav), cx);
    }

    fn send_with(
        &mut self,
        text: String,
        attachments: Vec<PathBuf>,
        voice_wav: Option<Arc<Vec<u8>>>,
        cx: &mut Context<Self>,
    ) {
        let Some(chat_id) = self.active_chat_id() else {
            return;
        };
        let message_id = new_id();
        let answer = self.chat_needs_answer(&chat_id);
        {
            let ui = self.chat_ui.entry(chat_id.clone()).or_default();
            if !ui.composer_dropped {
                ui.composer_dropped = true;
                ui.drop_started_at = Some(now_ms());
            }
        }
        // Title from the first message (§3.4); the source also queues a
        // `retitle` command for the box.
        if let Some(chat) = self
            .chats
            .values_mut()
            .flat_map(|v| v.iter_mut())
            .find(|c| c.id == chat_id && !c.is_pinned && c.title == "New chat")
        {
            chat.title = title_from(&text);
        }
        // Optimistic echo: the row shows at once; the source's frame with the
        // same id replaces it (§3.6).
        let mut parts: Vec<Part> = attachments
            .iter()
            .map(|path| Part::Attachment {
                id: new_id(),
                attachment: crate::model::Attachment {
                    name: path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("attachment")
                        .to_string(),
                    mime: "application/octet-stream".into(),
                    blob_ref: None,
                    bytes: None,
                    kind: None,
                    title: None,
                },
            })
            .collect();
        if voice_wav.is_some() {
            parts.push(Part::Attachment {
                id: new_id(),
                attachment: crate::model::Attachment {
                    name: "voice-memo.wav".into(),
                    mime: "audio/wav".into(),
                    blob_ref: None,
                    bytes: None,
                    kind: Some(AttachmentKind::Audio),
                    title: None,
                },
            });
        }
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
                decision: None,
                subagent_id: None,
                subagent_name: None,
                in_reply_to: None,
                routine_name: None,
                sender_name: None,
                sender_icon: None,
                sender_handle: None,
                sender_avatar: None,
                sender_avatar_color: None,
                mentions: Vec::new(),
                reply_to: None,
                asked_by_handle: None,
                owner_handle: None,
                usage: None,
                return_contract: None,
                waiting_user: false,
                waiting_approval: false,
                model: None,
                effort: None,
                created_at: now_ms(),
                completed_at: None,
                call_id: None,
                activity: None,
                pending: true,
                parts,
                hidden_parts: Vec::new(),
            });
        if answer {
            let question_id = self.open_plain_question_id(&chat_id);
            let value = question_id
                .as_ref()
                .map(|_| serde_json::Value::String(text.clone()));
            if let Some(qid) = &question_id {
                self.mark_question_answered(&chat_id, qid, value.clone());
            }
            self.command(ClientCommand::Answer {
                chat_id: chat_id.clone(),
                message_id,
                text,
                question_id,
                value,
            });
        } else if let Some(wav) = voice_wav {
            self.command(ClientCommand::SendVoice {
                chat_id: chat_id.clone(),
                message_id,
                text,
                wav,
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

    // ----- voice (F11) -----------------------------------------------------

    pub fn set_voice_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.voice_mode != on {
            self.voice_mode = on;
            if let Some(chat_id) = self.active_chat_id() {
                cx.emit(AppEvent::TranscriptChanged { chat_id });
            }
            cx.notify();
        }
    }

    /// Speech-to-text of a 16 kHz mono WAV through the relay proxy; the answer
    /// arrives as [`AppEvent::VoiceTranscript`] with the same `request_id`.
    pub fn transcribe(&mut self, request_id: u64, wav: Arc<Vec<u8>>) {
        let project_id = self.selected_project.clone().unwrap_or_default();
        self.command(ClientCommand::Transcribe {
            project_id,
            request_id,
            wav,
        });
    }

    /// Text-to-speech through the relay proxy; answered by [`AppEvent::VoiceSpeech`].
    pub fn speak(&mut self, request_id: u64, text: String) {
        let project_id = self.selected_project.clone().unwrap_or_default();
        self.command(ClientCommand::Speak {
            project_id,
            request_id,
            text,
        });
    }

    /// The reply's speech is cached: show the speaker icon under it.
    pub fn mark_spoken(&mut self, message_id: Id, cx: &mut Context<Self>) {
        if self.spoken.insert(message_id.clone()) {
            if let Some(chat_id) = self.chat_of_message(&message_id) {
                cx.emit(AppEvent::TranscriptChanged { chat_id });
            }
            cx.notify();
        }
    }

    pub fn replay_speech(&mut self, message_id: Id, cx: &mut Context<Self>) {
        cx.emit(AppEvent::ReplaySpeech { message_id });
    }

    fn chat_of_message(&self, message_id: &str) -> Option<Id> {
        self.messages
            .iter()
            .find(|(_, list)| list.iter().any(|m| m.id == message_id))
            .map(|(chat_id, _)| chat_id.clone())
    }

    /// Answer a typed widget card: the rendered text becomes the user's bubble at once
    /// (optimistic), the card flips to answered, and the structured value goes to the box.
    pub fn answer_question(
        &mut self,
        chat_id: Id,
        question_id: String,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let widget = self.messages_of(&chat_id).iter().find_map(|m| {
            m.parts.iter().find_map(|p| match p {
                Part::Question {
                    question_id: qid,
                    widget,
                    ..
                } if *qid == question_id => Some(widget.clone()),
                _ => None,
            })
        });
        let text = match widget.flatten() {
            Some(w) => w.answer_text(&value),
            None => format!(
                "Answer: {}",
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            ),
        };
        let message_id = new_id();
        {
            let ui = self.chat_ui.entry(chat_id.clone()).or_default();
            if !ui.composer_dropped {
                ui.composer_dropped = true;
                ui.drop_started_at = Some(now_ms());
            }
        }
        self.mark_question_answered(&chat_id, &question_id, Some(value.clone()));
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
                decision: None,
                subagent_id: None,
                subagent_name: None,
                in_reply_to: None,
                usage: None,
                return_contract: None,
                waiting_user: false,
                waiting_approval: false,
                model: None,
                effort: None,
                routine_name: None,
                sender_name: None,
                sender_icon: None,
                sender_handle: None,
                sender_avatar: None,
                sender_avatar_color: None,
                mentions: Vec::new(),
                reply_to: None,
                asked_by_handle: None,
                owner_handle: None,
                created_at: now_ms(),
                completed_at: None,
                call_id: None,
                activity: None,
                pending: true,
                parts: vec![Part::Text {
                    id: new_id(),
                    text: text.clone(),
                }],
                hidden_parts: Vec::new(),
            });
        self.command(ClientCommand::Answer {
            chat_id: chat_id.clone(),
            message_id,
            text,
            question_id: Some(question_id),
            value: Some(value),
        });
        cx.emit(AppEvent::TranscriptChanged { chat_id });
        cx.notify();
    }

    fn mark_question_answered(
        &mut self,
        chat_id: &str,
        question_id: &str,
        value: Option<serde_json::Value>,
    ) {
        let Some(list) = self.messages.get_mut(chat_id) else {
            return;
        };
        for m in list.iter_mut() {
            for p in m.parts.iter_mut() {
                if let Part::Question {
                    question_id: qid,
                    answer,
                    state,
                    answered_at,
                    ..
                } = p
                {
                    if qid == question_id {
                        *answer = value.clone();
                        *state = QuestionState::Answered;
                        *answered_at = Some(now_ms());
                        m.waiting_user = false;
                    }
                }
            }
        }
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

    /// S18: approve an approval card; the box dispatches and flips the card's state.
    pub fn approve(&mut self, chat_id: Id, approval_id: String, cx: &mut Context<Self>) {
        self.forward_vault_decision(&chat_id, &approval_id, true, cx);
        self.command(ClientCommand::Approve {
            chat_id,
            approval_id,
        });
        cx.notify();
    }

    /// S18: reject an approval card; nothing is dispatched.
    pub fn reject(
        &mut self,
        chat_id: Id,
        approval_id: String,
        reason: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.forward_vault_decision(&chat_id, &approval_id, false, cx);
        self.command(ClientCommand::Reject {
            chat_id,
            approval_id,
            reason,
        });
        cx.notify();
    }

    /// F8: a vault card carries the relay request id in its payload; the relay row is the
    /// authority (`POST /vault/requests/{id}/decide` needs a device token), so the card's
    /// decision goes there too.
    fn forward_vault_decision(
        &mut self,
        chat_id: &Id,
        approval_id: &str,
        approve: bool,
        cx: &mut Context<Self>,
    ) {
        let request_id = self
            .messages
            .get(chat_id)
            .into_iter()
            .flatten()
            .flat_map(|m| m.parts.iter())
            .find_map(|p| match p {
                Part::Approval {
                    approval_id: id,
                    payload,
                    ..
                } if id == approval_id => payload
                    .get("vaultRequestId")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                _ => None,
            });
        if let Some(request_id) = request_id {
            self.vault_decide(request_id, approve, cx);
        }
    }

    pub fn set_model(&mut self, model: String, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
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
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        let model = self.selected().map(|p| p.model.clone()).unwrap_or_default();
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

    pub fn machine_start(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::MachineStart);
        cx.notify();
    }

    pub fn machine_stop(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::MachineStop);
        cx.notify();
    }

    // ----- avatars

    /// Character slug a project shows (own choice or the hash pick).
    pub fn avatar_slug(&self, project: &Project) -> Option<String> {
        crate::ui::avatars::slug_for(project, &self.projects)
    }

    /// Animation state for a project's avatar from what its agent is doing.
    pub fn avatar_state(&self, project_id: &str) -> crate::ui::avatars::AvatarState {
        use crate::ui::avatars::AvatarState;
        if self.project_is_streaming(project_id) {
            return AvatarState::Working;
        }
        match self.avatar_reply_until.get(project_id) {
            Some(until) if *until > now_ms() => AvatarState::Reply,
            _ => AvatarState::Idle,
        }
    }

    /// Avatar colour id a project shows (`None` = mono / manifest default).
    pub fn avatar_color(&self, project: &Project) -> Option<String> {
        project
            .avatar_color
            .clone()
            .filter(|c| !c.trim().is_empty())
    }

    pub fn set_avatar_color(&mut self, project_id: Id, color: String, cx: &mut Context<Self>) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.avatar_color = Some(color.clone());
        }
        self.command(ClientCommand::SetAvatarColor { project_id, color });
        cx.notify();
    }

    pub fn set_avatar(&mut self, project_id: Id, slug: String, cx: &mut Context<Self>) {
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.avatar = Some(slug.clone());
        }
        self.avatar_picker = None;
        self.command(ClientCommand::SetAvatar { project_id, slug });
        cx.notify();
    }

    /// The agent's profile page (which holds the tree + colour picker).
    pub fn toggle_avatar_picker(&mut self, project_id: Id, cx: &mut Context<Self>) {
        match &self.profile {
            Some(ProfilePage::Agent { project_id: open }) if *open == project_id => {
                self.close_profile(cx);
            }
            _ => self.open_profile(ProfilePage::Agent { project_id }, cx),
        }
    }

    // ----- profile pages ------------------------------------------------------

    pub fn open_profile(&mut self, page: ProfilePage, cx: &mut Context<Self>) {
        self.avatar_picker = match &page {
            ProfilePage::Agent { project_id } => Some(project_id.clone()),
            _ => None,
        };
        self.profile = Some(page);
        self.profile_tab = 0;
        self.profile_picker_open = false;
        self.profile_more_open = false;
        self.profile_editing_description = false;
        cx.notify();
    }

    pub fn set_profile_editing_description(&mut self, on: bool, cx: &mut Context<Self>) {
        self.profile_editing_description = on;
        cx.notify();
    }

    /// Agent page: save the description (`""` clears) — optimistic, then PATCH.
    pub fn set_description(&mut self, project_id: Id, description: String, cx: &mut Context<Self>) {
        let description = description.trim().chars().take(500).collect::<String>();
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == project_id) {
            p.description = Some(description.clone()).filter(|d| !d.is_empty());
        }
        self.profile_editing_description = false;
        self.command(ClientCommand::SetDescription {
            project_id,
            description,
        });
        cx.notify();
    }

    /// Closes the sheet; `true` when one was open.
    pub fn close_profile(&mut self, cx: &mut Context<Self>) -> bool {
        self.avatar_picker = None;
        self.profile_more_open = false;
        let was = self.profile.take().is_some();
        if was {
            cx.notify();
        }
        was
    }

    pub fn set_profile_tab(&mut self, tab: usize, cx: &mut Context<Self>) {
        self.profile_tab = tab;
        cx.notify();
    }

    pub fn toggle_profile_picker(&mut self, cx: &mut Context<Self>) {
        self.profile_picker_open = !self.profile_picker_open;
        cx.notify();
    }

    pub fn toggle_profile_more(&mut self, cx: &mut Context<Self>) {
        self.profile_more_open = !self.profile_more_open;
        cx.notify();
    }

    /// Muted: the relay's `me.muted` for a people chat (synced across devices),
    /// else the local list (projects have no relay-side setting yet).
    pub fn is_muted(&self, space_id: &str) -> bool {
        self.project(space_id).is_some_and(|p| p.muted) || self.muted.contains(space_id)
    }

    pub fn toggle_mute(&mut self, space_id: Id, cx: &mut Context<Self>) {
        let now_muted = !self.is_muted(&space_id);
        let people = self.project(&space_id).is_some_and(|p| p.is_people());
        if let Some(p) = self.projects.iter_mut().find(|p| p.id == space_id) {
            p.muted = now_muted;
        }
        if now_muted {
            self.muted.insert(space_id.clone());
        } else {
            self.muted.remove(&space_id);
        }
        save_muted(&self.muted);
        if people {
            self.command(ClientCommand::SetMuted {
                space_id,
                muted: now_muted,
            });
        }
        cx.notify();
    }

    /// Profile pages: call a person (their DM) or an agent (Phase 2 — not yet).
    pub fn start_call(&mut self, target: CallTarget, video: bool, cx: &mut Context<Self>) {
        let kind = if video {
            CallKind::Video
        } else {
            CallKind::Voice
        };
        match target {
            CallTarget::User(handle) => {
                let dm = self
                    .people_spaces()
                    .find(|p| p.person().map(|x| x.handle == handle).unwrap_or(false))
                    .map(|p| p.id.clone());
                match dm {
                    Some(chat_id) => {
                        self.select_project(chat_id, cx);
                        self.start_call_in_chat(kind, cx);
                    }
                    None => {
                        self.push_notice(format!("Start a chat with @{handle} first, then call."));
                        cx.notify();
                    }
                }
            }
            CallTarget::Agent(project_id) => {
                let name = self
                    .project(&project_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "this agent".into());
                self.push_notice(format!("Calls with {name} are coming soon."));
                cx.notify();
            }
        }
    }

    /// Presence line for a person: "online", "last seen …", "invited".
    /// The relay's `online` / `last_seen_at` (API §9) first; a presence frame
    /// seen in a shared room this session can only make it fresher.
    pub fn presence_text(&self, person: &Person) -> String {
        if person.status == "invited" {
            return "invited · not signed in yet".into();
        }
        if self.is_online(person) {
            return "online".into();
        }
        let seen_frame = self.presence_seen.get(&person.handle).copied();
        let relay = person.last_seen_at.filter(|t| *t > 0).map(|t| t as u64);
        match seen_frame.into_iter().chain(relay).max() {
            Some(at) => last_seen_text(at),
            None => "last seen recently".into(),
        }
    }

    /// Online right now: the relay said so, or their device sent a presence
    /// frame in a room we share within the last 45 s.
    pub fn is_online(&self, person: &Person) -> bool {
        person.online
            || self
                .presence_seen
                .get(&person.handle)
                .is_some_and(|at| now_ms().saturating_sub(*at) < PRESENCE_ONLINE_MS)
    }

    /// A person by handle, from any space's member list or the directory.
    pub fn person_by_handle(&self, handle: &str) -> Option<Person> {
        self.projects
            .iter()
            .flat_map(|p| p.people.iter())
            .chain(self.people_results.iter())
            .find(|p| p.handle == handle)
            .cloned()
    }

    pub fn machine_restart(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::MachineRestart);
        cx.notify();
    }

    pub fn toggle_show_details(&mut self, cx: &mut Context<Self>) {
        self.show_details = !self.show_details;
        if let Some(chat_id) = self.active_chat_id() {
            cx.emit(AppEvent::TranscriptChanged { chat_id });
        }
        cx.notify();
    }

    pub fn toggle_right_panel(&mut self, cx: &mut Context<Self>) {
        self.right_panel_open = !self.right_panel_open;
        let on = self.right_panel_open;
        self.watch_browser(on, cx);
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

    // ----- system notifications (D49) --------------------------------------

    fn notify_gate(&self, project_id: &str, chat_id: Option<&str>) -> Gate {
        let project_shown = self.selected_project.as_deref() == Some(project_id);
        let chat_shown = match chat_id {
            Some(id) => self.active_chat_id().as_deref() == Some(id),
            None => true,
        };
        Gate {
            streaming: self.project_is_streaming(project_id),
            // Muted spaces are treated as "already seen": dropped, not queued.
            visible: (self.window_active && project_shown && chat_shown)
                || self.muted.contains(project_id),
        }
    }

    fn notify(&mut self, project_id: Id, chat_id: Option<&str>, kind: NotifyKind, detail: String) {
        self.notify_from(project_id, chat_id, kind, detail, None, None);
    }

    /// Like [`Self::notify`], as a chat message: `sender` titles the banner
    /// and `image` (the sender's chat picture) is attached.
    fn notify_from(
        &mut self,
        project_id: Id,
        chat_id: Option<&str>,
        kind: NotifyKind,
        detail: String,
        sender: Option<String>,
        image: Option<PathBuf>,
    ) {
        let Some(project) = self.project(&project_id) else {
            return;
        };
        let mut event = NotifyEvent::new(project.id.clone(), project.name.clone(), kind, detail);
        event.chat_id = chat_id.map(str::to_string);
        event.sender = sender;
        event.image = image;
        let gate = self.notify_gate(&project_id, chat_id);
        self.notifier.offer(event, now_ms(), gate);
    }

    /// A person's GitHub avatar as a round PNG for the notification, from the
    /// decoded image the rail already holds (`None` until it has loaded).
    fn person_notification_png(&self, url: &str) -> Option<PathBuf> {
        let img = self.avatars.get(url)?.as_ref()?;
        let bytes = img.as_bytes(0)?;
        let size = img.size(0);
        let (w, h) = (size.width.0 as u32, size.height.0 as u32);
        if w == 0 || h == 0 || bytes.len() < (w * h * 4) as usize {
            return None;
        }
        let mut rgba = bytes[..(w * h * 4) as usize].to_vec();
        let c = (w.min(h) as f32 - 1.0) / 2.0;
        let r = w.min(h) as f32 / 2.0;
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            px.swap(0, 2);
            let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
            let d = ((x - c).powi(2) + (y - c).powi(2)).sqrt();
            let cover = (r - d + 0.5).clamp(0.0, 1.0);
            px[3] = (px[3] as f32 * cover) as u8;
        }
        let key = format!("person-{:016x}", crate::ui::avatars::fnv1a64(url));
        notify::image_file(&key, w, h, &rgba)
    }

    /// The agent behind a reply: its name and chat image (a shared agent's
    /// own tree and colour when the message or member list carries them,
    /// else this project's).
    fn agent_identity(&self, project_id: &str, message: &Message) -> (String, Option<PathBuf>) {
        let project = self.projects.iter().find(|p| p.id == project_id);
        // `agent:<pid>` in a people chat → that member; `project:<pid>` in a
        // group chat → that project of mine.
        let shared_agent = message
            .sender
            .strip_prefix("agent:")
            .and_then(|pid| {
                project
                    .and_then(|p| p.agents.iter().find(|a| a.project_id == pid))
                    .map(|a| (a.name.clone(), a.avatar.clone(), a.avatar_color.clone()))
            })
            .or_else(|| {
                let pid = message.sender.strip_prefix("project:")?;
                let other = self.projects.iter().find(|p| p.id == pid)?;
                Some((
                    other.name.clone(),
                    self.avatar_slug(other),
                    self.avatar_color(other),
                ))
            });
        let name = message
            .sender_name
            .clone()
            .filter(|n| !n.is_empty())
            .or_else(|| shared_agent.as_ref().map(|(n, _, _)| n.clone()))
            .or_else(|| project.map(|p| p.name.clone()))
            .unwrap_or_else(|| "Agent".to_string());
        let slug = message
            .sender_avatar
            .clone()
            .filter(|a| !a.is_empty() && !a.contains("://"))
            .or_else(|| shared_agent.as_ref().and_then(|(_, s, _)| s.clone()))
            .or_else(|| project.and_then(|p| self.avatar_slug(p)));
        let color = message
            .sender_avatar_color
            .clone()
            .filter(|c| !c.is_empty())
            .or_else(|| shared_agent.as_ref().and_then(|(_, _, c)| c.clone()))
            .or_else(|| project.and_then(|p| self.avatar_color(p)));
        let image = slug
            .as_deref()
            .and_then(|s| crate::ui::avatars::notification_png(s, color.as_deref()));
        (name, image)
    }

    fn notify_for_message(
        &mut self,
        chat_id: &str,
        message: &Message,
        was_streaming: bool,
        had_open_question: bool,
        is_new: bool,
    ) {
        let Some(chat) = self
            .chats
            .values()
            .flat_map(|v| v.iter())
            .find(|c| c.id == chat_id)
        else {
            return;
        };
        let project_id = chat.project_id.clone();
        let title = chat.title.clone();
        let is_system = chat.is_system;
        // Single-chat mode: a chat the UI cannot open does not notify either.
        if !self
            .visible_tabs(&project_id)
            .iter()
            .any(|c| c.id == chat_id)
        {
            return;
        }

        let sender = message.sender.as_str();
        let in_people = self
            .projects
            .iter()
            .any(|p| p.id == project_id && p.is_people());
        if message.role == Role::User {
            // Someone else wrote in a people chat (D72 + D49): "@sam: …".
            if is_new && in_people && !self.is_mine(message) {
                let handle = message
                    .sender_handle
                    .clone()
                    .unwrap_or_else(|| "someone".into());
                let person = self
                    .projects
                    .iter()
                    .find(|p| p.id == project_id)
                    .and_then(|p| p.people.iter().find(|x| x.handle == handle).cloned());
                let name = message
                    .sender_name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .or_else(|| person.as_ref().map(|x| x.display_name()))
                    .unwrap_or_else(|| format!("@{handle}"));
                let url = message
                    .sender_avatar
                    .clone()
                    .filter(|a| a.contains("://"))
                    .or_else(|| person.as_ref().map(|x| x.avatar_url.clone()))
                    .unwrap_or_default();
                let image = self.person_notification_png(&url);
                self.notify_from(
                    project_id,
                    Some(chat_id),
                    NotifyKind::DirectMessage,
                    preview(message),
                    Some(name),
                    image,
                );
                return;
            }
            // A prompt another project injected is news; our own typed
            // messages and routine prompts are not (the routine's reply is).
            if is_new && (sender.starts_with("project:") || (is_system && sender != "user")) {
                self.notify(
                    project_id,
                    Some(chat_id),
                    NotifyKind::CrossProject,
                    preview(message),
                );
            }
            return;
        }
        if needs_input(message) && !had_open_question {
            let question = message
                .open_questions()
                .next_back()
                .and_then(|p| match p {
                    Part::Question { question, .. } => Some(question.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| preview(message));
            self.notify(project_id, Some(chat_id), NotifyKind::NeedsInput, question);
            return;
        }
        let finished = !message.is_streaming() && message.status != MessageStatus::Queued;
        if !finished || !(was_streaming || is_new) {
            return;
        }
        if message.role == Role::Assistant && was_streaming {
            // The avatar's short reply reaction, once per completed turn.
            let slug = self
                .projects
                .iter()
                .find(|p| p.id == project_id)
                .and_then(|p| self.avatar_slug(p));
            let len = slug
                .as_deref()
                .map(crate::ui::avatars::reply_duration_ms)
                .unwrap_or(1000);
            self.avatar_reply_until
                .insert(project_id.clone(), now_ms() + len);
        }
        if message.role == Role::Project || sender.starts_with("project:") {
            let (sender, image) = self.agent_identity(&project_id, message);
            self.notify_from(
                project_id,
                Some(chat_id),
                NotifyKind::CrossProject,
                preview(message),
                Some(sender),
                image,
            );
            return;
        }
        if message.role != Role::Assistant {
            return;
        }
        let failed = message.status == MessageStatus::Error;
        if let Some(routine) = self.routine_behind(chat_id, message) {
            let detail = if failed {
                format!("{routine}: run failed")
            } else {
                let p = preview(message);
                if p.is_empty() {
                    routine
                } else {
                    format!("{routine}: {p}")
                }
            };
            self.notify(project_id, Some(chat_id), NotifyKind::RoutineRan, detail);
            return;
        }
        let detail = if failed {
            format!("{title}: turn failed")
        } else {
            let p = preview(message);
            if p.is_empty() {
                title
            } else {
                p
            }
        };
        let (sender, image) = self.agent_identity(&project_id, message);
        self.notify_from(
            project_id,
            Some(chat_id),
            NotifyKind::TurnFinished,
            detail,
            Some(sender),
            image,
        );
    }

    /// The routine whose prompt this assistant reply answers (F3: the prompt is
    /// a user-role message with `sender: routine:<id>` and `routineName`).
    fn routine_behind(&self, chat_id: &str, reply: &Message) -> Option<String> {
        let list = self.messages_of(chat_id);
        let prompt = match reply.in_reply_to.as_deref() {
            Some(id) => list.iter().find(|m| m.id == id),
            None => {
                let at = list
                    .iter()
                    .position(|m| m.id == reply.id)
                    .unwrap_or(list.len());
                list[..at].iter().rev().find(|m| m.role == Role::User)
            }
        }?;
        let id = prompt.sender.strip_prefix("routine:")?;
        Some(
            prompt
                .routine_name
                .clone()
                .unwrap_or_else(|| format!("Routine {id}")),
        )
    }

    /// F2 approvals call this when a gate card appears; no caller on this
    /// branch yet.
    #[allow(dead_code)]
    pub fn notify_approval_requested(
        &mut self,
        project_id: Id,
        chat_id: Option<&str>,
        what: String,
    ) {
        self.notify(project_id, chat_id, NotifyKind::ApprovalRequested, what);
    }

    fn notify_tick(&mut self, cx: &mut Context<Self>) {
        self.tick_calls(cx);
        let gates: HashMap<Id, Gate> = self
            .projects
            .iter()
            .map(|p| (p.id.clone(), self.notify_gate(&p.id, None)))
            .collect();
        self.notifier
            .tick(now_ms(), |id| gates.get(id).copied().unwrap_or_default());
        for click in notify::take_clicks() {
            if let Some(rest) = click.strip_prefix("call:") {
                match rest.split_once(':') {
                    Some(("accept", _)) => self.accept_incoming(cx),
                    Some(("decline", _)) => self.decline_incoming(cx),
                    _ => {}
                }
                continue;
            }
            self.open_chat_from_notification(click, cx);
        }
        self.sync_badge();
        // Selection paths that bypass select_tab/select_project still report.
        self.sync_focus();
    }

    /// A click on a notification: bring the window up and show that chat.
    pub fn open_chat_from_notification(&mut self, chat_id: Id, cx: &mut Context<Self>) {
        let project_id = self
            .chats
            .values()
            .flat_map(|v| v.iter())
            .find(|c| c.id == chat_id)
            .map(|c| c.project_id.clone());
        let Some(project_id) = project_id else {
            return;
        };
        self.select_project(project_id, cx);
        self.select_tab(chat_id, cx);
        cx.activate(true);
    }

    /// Dock badge = chats with something unread (cleared as they are read).
    pub fn sync_badge(&self) {
        let total = self
            .chats
            .values()
            .flat_map(|v| v.iter())
            .filter(|c| c.unread)
            .count();
        notify::set_badge(total);
    }

    /// First launch after sign-in: show the explainer once, if the platform
    /// will ask for permission at all.
    fn maybe_prompt_notifications(&mut self) {
        if self.notify_prompt || self.notify_prompt_answered || !notify::needs_permission() {
            return;
        }
        if !notify::permission_asked(&crate::sync::credentials::config_dir()) {
            self.notify_prompt = true;
        }
    }

    pub fn allow_notifications(&mut self, cx: &mut Context<Self>) {
        notify::mark_permission_asked(&crate::sync::credentials::config_dir());
        notify::request_permission();
        self.notify_prompt = false;
        self.notify_prompt_answered = true;
        cx.notify();
    }

    pub fn dismiss_notify_prompt(&mut self, cx: &mut Context<Self>) {
        notify::mark_permission_asked(&crate::sync::credentials::config_dir());
        self.notify_prompt = false;
        self.notify_prompt_answered = true;
        cx.notify();
    }

    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.window_active != active {
            self.window_active = active;
            self.sync_focus();
            cx.notify();
        }
    }

    /// Tell the relay which chat is on screen in a focused window (API §7c:
    /// presence `focused_chat`), whenever that changes.
    fn sync_focus(&mut self) {
        let focused = if self.window_active {
            self.active_chat_id()
        } else {
            None
        };
        if self.focus_sent.as_ref() == Some(&focused) {
            return;
        }
        self.focus_sent = Some(focused.clone());
        self.command(ClientCommand::FocusChat { chat_id: focused });
    }

    pub fn set_notifications_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.notifier.enabled = enabled;
        cx.notify();
    }

    pub fn push_notice(&mut self, text: impl Into<String>) {
        let now = now_ms();
        self.notices
            .retain(|n| now.saturating_sub(n.at) < NOTICE_TTL_MS);
        self.notices.push(Notice {
            text: text.into(),
            at: now,
        });
    }

    pub fn live_notices(&self) -> Vec<Notice> {
        let now = now_ms();
        self.notices
            .iter()
            .filter(|n| now.saturating_sub(n.at) < NOTICE_TTL_MS)
            .cloned()
            .collect()
    }

    pub fn pair(&mut self, code: String, cx: &mut Context<Self>) {
        if let Some(p) = &mut self.pairing {
            p.busy = true;
            p.error = None;
        }
        self.command(ClientCommand::Pair { code });
        cx.notify();
    }

    /// "Sign in with GitHub": run the browser leg, then exchange the grant.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(p) = &mut self.pairing else {
            return;
        };
        if p.busy {
            return;
        }
        p.busy = true;
        p.error = None;
        let relay = p.relay_url.clone();
        cx.notify();
        let this = cx.entity().downgrade();
        auth::begin(relay, device_name(), cx, move |outcome, cx| {
            let _ = this.update(cx, |state, cx| {
                match outcome {
                    Ok(auth::SignIn::Grant(grant_code)) => {
                        state.command(ClientCommand::SignIn { grant_code });
                    }
                    Ok(auth::SignIn::PairCodeMode) => {
                        if let Some(p) = &mut state.pairing {
                            p.mode = SignInMode::PairCode;
                            p.busy = false;
                        }
                    }
                    Err(err) => {
                        if let Some(p) = &mut state.pairing {
                            p.busy = false;
                            p.error = Some(format!("{err:#}"));
                        }
                    }
                }
                cx.notify();
            });
        });
    }

    // ----- people chats (D72)

    /// `gh_<id>` of the signed-in user; the fixture signs in as `gh_1`.
    pub fn me_id(&self) -> String {
        self.account
            .as_ref()
            .map(|a| a.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| {
                if self.source_name == "fixture" {
                    crate::sync::fixture::ME_ID.into()
                } else {
                    String::new()
                }
            })
    }

    pub fn me_handle(&self) -> String {
        self.account
            .as_ref()
            .map(|a| a.login.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| {
                if self.source_name == "fixture" {
                    crate::sync::fixture::ME_HANDLE.into()
                } else {
                    String::new()
                }
            })
    }

    /// Projects of mine (not people chats, not shared with me) — the add-agent picker.
    pub fn my_projects(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter().filter(|p| !p.is_people() && !p.shared)
    }

    pub fn people_spaces(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter().filter(|p| p.is_people())
    }

    /// Is this message mine? (`sender: user:<my id>` or the plain `user` of a project chat.)
    pub fn is_mine(&self, message: &Message) -> bool {
        let sender = message.sender.as_str();
        sender == "user" || sender == format!("user:{}", self.me_id())
    }

    pub fn open_new_chat(&mut self, cx: &mut Context<Self>) {
        self.new_chat_open = true;
        self.new_project_open = false;
        self.invite_target = None;
        self.people_query.clear();
        self.people_results.clear();
        self.people_error = None;
        self.search_people(String::new(), cx);
    }

    /// Project header "Invite": the same sheet, but a pick adds a member.
    pub fn open_invite(&mut self, project_id: Id, cx: &mut Context<Self>) {
        self.open_new_chat(cx);
        self.invite_target = Some(project_id);
        cx.notify();
    }

    pub fn close_new_chat(&mut self, cx: &mut Context<Self>) {
        self.new_chat_open = false;
        self.awaiting_dm = None;
        self.invite_target = None;
        cx.notify();
    }

    /// The sheet's pick: invite into the target project or start a DM.
    pub fn pick_person(&mut self, handle: String, cx: &mut Context<Self>) {
        match self.invite_target.clone() {
            Some(project_id) => {
                self.new_chat_open = false;
                self.invite_target = None;
                self.invite_to_project(project_id, handle, cx);
            }
            None => self.start_dm(handle, cx),
        }
    }

    pub fn search_people(&mut self, query: String, cx: &mut Context<Self>) {
        self.people_query = query.clone();
        self.people_error = None;
        // An empty query lists everyone on Arbos (the sheet's "Arbos users").
        self.people_busy = true;
        self.command(ClientCommand::SearchPeople { query });
        cx.notify();
    }

    pub fn start_dm(&mut self, handle: String, cx: &mut Context<Self>) {
        let handle = handle.trim().trim_start_matches('@').to_string();
        if handle.is_empty() {
            return;
        }
        let existing = self
            .people_spaces()
            .find(|p| p.person().map(|x| x.handle == handle).unwrap_or(false))
            .map(|p| p.id.clone());
        if let Some(existing) = existing {
            self.new_chat_open = false;
            self.select_project(existing, cx);
            return;
        }
        self.people_busy = true;
        self.awaiting_dm = Some(handle.clone());
        self.command(ClientCommand::StartDm { handle });
        cx.notify();
    }

    /// Composer keystroke in a people chat: at most one presence ping per 3 s.
    pub fn typed_in(&mut self, chat_id: &Id) {
        let Some(space) = self.project_of_chat(chat_id) else {
            return;
        };
        if !space.is_people() {
            return;
        }
        let now = now_ms();
        let last = self.typing_sent_at.get(chat_id).copied().unwrap_or(0);
        if now.saturating_sub(last) > 3_000 {
            self.typing_sent_at.insert(chat_id.clone(), now);
            self.command(ClientCommand::SetTyping {
                chat_id: chat_id.clone(),
                active: true,
            });
        }
    }

    pub fn typing_in(&self, chat_id: &str) -> Option<String> {
        self.typing
            .get(chat_id)
            .filter(|(_, until)| *until > now_ms())
            .map(|(handle, _)| handle.clone())
    }

    fn project_of_chat(&self, chat_id: &str) -> Option<&Project> {
        let pid = self
            .chats
            .values()
            .flat_map(|v| v.iter())
            .find(|c| c.id == chat_id)
            .map(|c| c.project_id.clone())?;
        self.projects.iter().find(|p| p.id == pid)
    }

    pub fn add_agent(&mut self, chat_id: Id, project_id: Id, cx: &mut Context<Self>) {
        self.command(ClientCommand::AddAgent {
            chat_id,
            project_id,
        });
        cx.notify();
    }

    pub fn remove_agent(&mut self, chat_id: Id, project_id: Id, cx: &mut Context<Self>) {
        self.command(ClientCommand::RemoveAgent {
            chat_id,
            project_id,
        });
        cx.notify();
    }

    pub fn remove_member(&mut self, chat_id: Id, user_id: String, cx: &mut Context<Self>) {
        self.command(ClientCommand::RemoveMember { chat_id, user_id });
        cx.notify();
    }

    pub fn leave_chat(&mut self, chat_id: Id, cx: &mut Context<Self>) {
        if self.selected_project.as_deref() == Some(chat_id.as_str()) {
            self.selected_project = None;
        }
        self.command(ClientCommand::LeaveChat { chat_id });
        cx.notify();
    }

    pub fn block_user(&mut self, handle: String, cx: &mut Context<Self>) {
        self.command(ClientCommand::BlockUser { handle });
        cx.notify();
    }

    pub fn invite_to_project(&mut self, project_id: Id, handle: String, cx: &mut Context<Self>) {
        let handle = handle.trim().trim_start_matches('@').to_string();
        if handle.is_empty() {
            return;
        }
        self.command(ClientCommand::InviteToProject { project_id, handle });
        cx.notify();
    }

    pub fn remove_project_member(
        &mut self,
        project_id: Id,
        user_id: String,
        cx: &mut Context<Self>,
    ) {
        self.command(ClientCommand::RemoveProjectMember {
            project_id,
            user_id,
        });
        cx.notify();
    }

    /// Decoded avatar for a URL, requesting it once when unknown.
    pub fn avatar(&self, url: &str) -> Option<Arc<RenderImage>> {
        self.avatars.get(url).cloned().flatten()
    }

    fn want_avatar(&mut self, url: String) {
        if url.is_empty() || self.avatars.contains_key(&url) {
            return;
        }
        self.avatars.insert(url.clone(), None);
        self.command(ClientCommand::FetchAvatar { url });
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::SignOut);
        self.account = None;
        self.projects.clear();
        self.chats.clear();
        self.selected_project = None;
        cx.notify();
    }

    pub fn create_project(&mut self, name: String, cx: &mut Context<Self>) {
        self.new_project_open = false;
        self.awaiting_new_project = true;
        self.command(ClientCommand::CreateProject { name });
        self.push_notice("Creating project…");
        cx.notify();
    }

    pub fn fetch_blob(&mut self, key: String, save: bool, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.command(ClientCommand::FetchBlob {
            project_id,
            key,
            save,
        });
        cx.notify();
    }

    /// Ask the source for the bytes of every image attachment in a chat that has no thumbnail yet.
    fn request_thumbnails(&mut self, chat_id: &Id) {
        let messages = self.messages.get(chat_id).cloned().unwrap_or_default();
        self.request_thumbnails_for(chat_id, &messages);
    }

    fn request_thumbnails_for(&mut self, chat_id: &Id, messages: &[Message]) {
        let project_id = self
            .chats
            .iter()
            .find(|(_, tabs)| tabs.iter().any(|c| &c.id == chat_id))
            .map(|(pid, _)| pid.clone())
            .or_else(|| self.selected_project.clone());
        let Some(project_id) = project_id else {
            return;
        };
        for message in messages {
            for part in &message.parts {
                let Part::Attachment { attachment, .. } = part else {
                    continue;
                };
                if attachment.kind() != AttachmentKind::Image {
                    continue;
                }
                let Some(key) = attachment.blob_ref.clone() else {
                    continue;
                };
                if !self.thumbnails_requested.insert(key.clone()) {
                    continue;
                }
                self.command(ClientCommand::FetchThumbnail {
                    project_id: project_id.clone(),
                    key,
                });
            }
        }
    }

    /// Download an attachment to `~/Downloads/<name>`; the source answers with `AttachmentSaved`.
    pub fn download_attachment(&mut self, key: String, name: String, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.push_notice(format!("Downloading {name}…"));
        self.command(ClientCommand::DownloadAttachment {
            project_id,
            key,
            name,
        });
        cx.notify();
    }

    pub fn watch_browser(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        if !on {
            self.browser_frame = None;
        }
        self.command(ClientCommand::WatchBrowser { project_id, on });
        cx.notify();
    }

    // ----- connectors (F1) -------------------------------------------------

    pub fn refresh_connectors(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::RefreshConnectors);
        cx.notify();
    }

    pub fn connect_connector(
        &mut self,
        provider: String,
        token: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.push_notice(if token.is_some() {
            format!("Saving the {provider} token on the relay…")
        } else {
            format!("Starting {provider} sign-in…")
        });
        self.command(ClientCommand::ConnectorConnect { provider, token });
        cx.notify();
    }

    pub fn disconnect_connector(&mut self, id: Id, cx: &mut Context<Self>) {
        self.connectors.items.retain(|c| c.id != id);
        self.command(ClientCommand::ConnectorDisconnect { id });
        cx.notify();
    }

    // ----- this mac (F9) ---------------------------------------------------

    pub fn local_refresh(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::LocalRefresh);
        cx.notify();
    }

    pub fn local_set_allow_reads(&mut self, device_id: Id, allow: bool, cx: &mut Context<Self>) {
        if let Some(d) = self
            .local
            .devices
            .iter_mut()
            .find(|d| d.device_id == device_id)
        {
            d.allow_reads = allow;
        }
        self.command(ClientCommand::LocalSetAllowReads { device_id, allow });
        cx.notify();
    }

    pub fn local_forget(&mut self, device_id: Id, cx: &mut Context<Self>) {
        self.local.devices.retain(|d| d.device_id != device_id);
        self.command(ClientCommand::LocalForget { device_id });
        cx.notify();
    }

    // ----- vault (F8) ------------------------------------------------------

    pub fn vault_refresh(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::VaultRefresh);
        cx.notify();
    }

    pub fn vault_add_item(&mut self, item: VaultNewItem, cx: &mut Context<Self>) {
        self.push_notice(format!("Saving {} on the relay…", item.name));
        self.command(ClientCommand::VaultAddItem { item });
        cx.notify();
    }

    pub fn vault_delete_item(&mut self, id: Id, cx: &mut Context<Self>) {
        self.vault.items.retain(|i| i.id != id);
        self.command(ClientCommand::VaultDeleteItem { id });
        cx.notify();
    }

    pub fn vault_decide(&mut self, request_id: Id, approve: bool, cx: &mut Context<Self>) {
        if let Some(r) = self.vault.requests.iter_mut().find(|r| r.id == request_id) {
            r.status = if approve {
                VaultRequestStatus::Approved
            } else {
                VaultRequestStatus::Denied
            };
            r.decided_at = Some(now_ms() as i64);
        }
        self.command(ClientCommand::VaultDecide {
            request_id,
            approve,
        });
        cx.notify();
    }

    // ----- routines (F3) ---------------------------------------------------

    pub fn routines_of(&self, project_id: &str) -> &[Routine] {
        self.routines
            .get(project_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn routine_runs_of(&self, routine_id: &str) -> &[RoutineRun] {
        self.routine_runs
            .get(routine_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn routines_refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(project_id) = self.selected_project.clone() {
            self.command(ClientCommand::RoutinesRefresh { project_id });
        }
        cx.notify();
    }

    pub fn routine_create(
        &mut self,
        name: String,
        prompt: String,
        chat_id: Id,
        trigger: RoutineTrigger,
        cx: &mut Context<Self>,
    ) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.command(ClientCommand::RoutineCreate {
            project_id,
            name,
            prompt,
            chat_id,
            trigger,
        });
        cx.notify();
    }

    pub fn routine_set_enabled(&mut self, routine_id: Id, enabled: bool, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        if let Some(r) = self
            .routines
            .get_mut(&project_id)
            .and_then(|v| v.iter_mut().find(|r| r.id == routine_id))
        {
            r.enabled = enabled;
        }
        self.command(ClientCommand::RoutineSetEnabled {
            project_id,
            routine_id,
            enabled,
        });
        cx.notify();
    }

    pub fn routine_run_now(&mut self, routine_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.command(ClientCommand::RoutineRunNow {
            project_id,
            routine_id,
        });
        self.push_notice("Routine queued");
        cx.notify();
    }

    pub fn routine_load_runs(&mut self, routine_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        self.command(ClientCommand::RoutineRuns {
            project_id,
            routine_id,
        });
        cx.notify();
    }

    pub fn routine_delete(&mut self, routine_id: Id, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            return;
        };
        if let Some(v) = self.routines.get_mut(&project_id) {
            v.retain(|r| r.id != routine_id);
        }
        self.command(ClientCommand::RoutineDelete {
            project_id,
            routine_id,
        });
        cx.notify();
    }

    /// History "open": switch to the chat that holds the reply.
    pub fn open_chat_of_routine(&mut self, routine_id: &str, cx: &mut Context<Self>) {
        let chat_id = self
            .selected_project
            .as_deref()
            .and_then(|p| self.routines_of(p).iter().find(|r| r.id == routine_id))
            .map(|r| r.chat_id.clone());
        if let Some(chat_id) = chat_id {
            self.select_tab(chat_id, cx);
        }
    }

    // ----- skills (F5) -------------------------------------------------------

    pub fn refresh_skills(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::SkillsRefresh);
        cx.notify();
    }

    pub fn save_skill(&mut self, skill: Skill, cx: &mut Context<Self>) {
        // Optimistic: the list shows the edit at once; the source's `Skills`
        // event replaces it with the stored version.
        match self
            .skills
            .iter_mut()
            .find(|s| s.id == skill.id && !skill.id.is_empty())
        {
            Some(existing) => *existing = skill.clone(),
            None => self.skills.push(skill.clone()),
        }
        self.command(ClientCommand::SkillSave { skill });
        cx.notify();
    }

    pub fn delete_skill(&mut self, id: Id, cx: &mut Context<Self>) {
        self.skills.retain(|s| s.id != id);
        self.command(ClientCommand::SkillDelete { id });
        cx.notify();
    }

    // ----- channels (F7) ---------------------------------------------------

    pub fn refresh_channels(&mut self, cx: &mut Context<Self>) {
        self.command(ClientCommand::RefreshChannels);
        cx.notify();
    }

    /// Mint a pairing code for the selected project (its pinned chat).
    pub fn new_channel_pairing(&mut self, cx: &mut Context<Self>) {
        let Some(project_id) = self.selected_project.clone() else {
            self.push_notice("Select a project first.");
            cx.notify();
            return;
        };
        self.command(ClientCommand::ChannelPairNew {
            project_id,
            chat_id: None,
        });
        cx.notify();
    }

    pub fn unbind_channel(&mut self, id: Id, cx: &mut Context<Self>) {
        self.channels.bindings.retain(|b| b.id != id);
        self.command(ClientCommand::ChannelUnbind { id });
        cx.notify();
    }

    pub fn selected_doc(&self) -> Option<&ProjectDocView> {
        self.selected_project
            .as_deref()
            .and_then(|id| self.project_docs.get(id))
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

impl AppState {
    // ----- calls v1 ---------------------------------------------------------

    /// Start a call in the selected chat (or join the live one).
    pub fn start_call_in_chat(&mut self, kind: CallKind, cx: &mut Context<Self>) {
        let Some(chat_id) = self.selected_project.clone() else {
            return;
        };
        if self.my_call.is_some() {
            return;
        }
        // Already a live call here: join it instead of starting another.
        if let Some(call) = self.calls_in_chat.get(&chat_id).cloned() {
            self.join_call(call.call_id.clone(), cx);
            return;
        }
        self.my_call = Some(ActiveCall {
            call: Call {
                chat_id: chat_id.clone(),
                kind,
                ..Default::default()
            },
            status: CallStatus::Connecting,
            muted: false,
            camera: kind == CallKind::Video,
            participants: Vec::new(),
            local_video: None,
            joined_at: now_ms(),
            expanded: true,
            notice: None,
            outgoing: true,
            reachable: None,
        });
        self.command(ClientCommand::StartCall { chat_id, kind });
        self.sync_tones();
        cx.notify();
    }

    pub fn join_call(&mut self, call_id: String, cx: &mut Context<Self>) {
        if self.my_call.is_some() {
            return;
        }
        self.incoming_since = None;
        let known = self
            .calls_in_chat
            .values()
            .find(|c| c.call_id == call_id)
            .cloned()
            .unwrap_or_default();
        self.my_call = Some(ActiveCall {
            camera: known.kind == CallKind::Video,
            call: known,
            status: CallStatus::Connecting,
            muted: false,
            participants: Vec::new(),
            local_video: None,
            joined_at: now_ms(),
            expanded: true,
            notice: None,
            outgoing: false,
            reachable: None,
        });
        self.incoming_call = None;
        self.command(ClientCommand::JoinCall { call_id });
        cx.notify();
    }

    pub fn accept_incoming(&mut self, cx: &mut Context<Self>) {
        let Some((call, _)) = self.incoming_call.take() else {
            return;
        };
        if self.my_call.is_some() {
            self.leave_call(cx);
        }
        self.select_project(call.chat_id.clone(), cx);
        self.join_call(call.call_id, cx);
    }

    pub fn decline_incoming(&mut self, cx: &mut Context<Self>) {
        if let Some((call, _)) = self.incoming_call.take() {
            notify::withdraw_call(&call.call_id);
        }
        self.incoming_since = None;
        self.sync_tones();
        cx.notify();
    }

    /// Ringback while my outgoing call has nobody else in it; ring while the
    /// ring card is up. Idempotent: called after every call state change.
    fn sync_tones(&mut self) {
        let ringback = self
            .my_call
            .as_ref()
            .map(|c| {
                c.outgoing
                    && c.participants.is_empty()
                    && !matches!(c.status, CallStatus::Failed(_))
            })
            .unwrap_or(false);
        match (ringback, self.ringback_tone.is_some()) {
            (true, false) => self.ringback_tone = Some(TonePlayer::start(Tone::Ringback)),
            (false, true) => self.ringback_tone = None,
            _ => {}
        }
        let ring = self.incoming_call.is_some() && self.my_call.is_none();
        match (ring, self.ring_tone.is_some()) {
            (true, false) => self.ring_tone = Some(TonePlayer::start(Tone::Ring)),
            (false, true) => self.ring_tone = None,
            _ => {}
        }
    }

    /// Every notify tick: stop ringing after `RING_TIMEOUT_MS`, give up on an
    /// unanswered outgoing call after `RINGBACK_TIMEOUT_MS`.
    fn tick_calls(&mut self, cx: &mut Context<Self>) {
        if !self.missed_pending.is_empty() {
            let now = now_ms();
            let pending = std::mem::take(&mut self.missed_pending);
            for (missed, since) in pending {
                if now.saturating_sub(since) <= MISSED_PENDING_MS {
                    self.on_call_missed(missed, cx);
                }
            }
        }
        if let Some(since) = self.incoming_since {
            if now_ms().saturating_sub(since) > RING_TIMEOUT_MS {
                self.decline_incoming(cx);
            }
        }
        let give_up = self
            .my_call
            .as_ref()
            .map(|c| {
                c.outgoing
                    && c.participants.is_empty()
                    && now_ms().saturating_sub(c.joined_at) > RINGBACK_TIMEOUT_MS
            })
            .unwrap_or(false);
        if give_up {
            self.leave_call(cx);
            self.push_notice("No answer.");
            cx.notify();
        }
        self.sync_tones();
    }

    pub fn leave_call(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.my_call.take() else {
            return;
        };
        let _ = self.call_commands.send(CallCmd::Leave);
        if !active.call.call_id.is_empty() {
            self.command(ClientCommand::LeaveCall {
                call_id: active.call.call_id,
            });
        }
        self.sync_tones();
        cx.notify();
    }

    pub fn toggle_call_mute(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.my_call.as_mut() {
            c.muted = !c.muted;
            let _ = self.call_commands.send(CallCmd::SetMuted(c.muted));
            cx.notify();
        }
    }

    pub fn toggle_camera(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.my_call.as_mut() {
            c.camera = !c.camera;
            if !c.camera {
                c.local_video = None;
            }
            c.notice = None;
            let _ = self.call_commands.send(CallCmd::SetCamera(c.camera));
            cx.notify();
        }
    }

    pub fn toggle_call_expanded(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.my_call.as_mut() {
            c.expanded = !c.expanded;
            cx.notify();
        }
    }

    /// Who I am, as the relay would describe me in a call.
    fn me_sender(&self) -> Sender {
        let account = self.account.clone();
        Sender {
            kind: "user".into(),
            id: self.me_id(),
            name: account.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            handle: self.me_handle(),
            avatar: None,
            avatar_color: None,
            avatar_url: account.map(|a| a.avatar_url).unwrap_or_default(),
        }
    }

    fn on_call_ready(
        &mut self,
        call: Call,
        created: bool,
        reachable: Vec<Reachable>,
        cx: &mut Context<Self>,
    ) {
        self.calls_in_chat
            .insert(call.chat_id.clone(), call.clone());
        if created {
            let line = format!(
                "{} started a {}",
                self.me_sender().display_name(),
                call.kind.label()
            );
            self.command(ClientCommand::SystemLine {
                chat_id: call.chat_id.clone(),
                text: line,
                call_id: Some(call.call_id.clone()),
            });
        }
        let camera = self
            .my_call
            .as_ref()
            .map(|c| c.camera)
            .unwrap_or(call.kind == CallKind::Video);
        let Some(token) = call.token.clone() else {
            self.push_notice("The relay sent no join token.");
            self.my_call = None;
            cx.notify();
            return;
        };
        let active = self.my_call.get_or_insert_with(|| ActiveCall {
            call: call.clone(),
            status: CallStatus::Connecting,
            muted: false,
            camera,
            participants: Vec::new(),
            local_video: None,
            joined_at: now_ms(),
            expanded: true,
            notice: None,
            outgoing: false,
            reachable: None,
        });
        active.call = call.clone();
        active.status = CallStatus::Connecting;
        if created {
            active.outgoing = true;
            active.reachable = reachable_line(&reachable);
        }
        let _ = self.call_commands.send(CallCmd::Join {
            url: call.livekit_url.clone(),
            token,
            camera,
        });
        self.incoming_call = None;
        self.sync_tones();
        cx.notify();
    }

    fn on_call_started(&mut self, call: Call, sender: Sender, cx: &mut Context<Self>) {
        self.calls_in_chat
            .insert(call.chat_id.clone(), call.clone());
        let in_it = self
            .my_call
            .as_ref()
            .map(|c| c.call.call_id == call.call_id)
            .unwrap_or(false);
        if !in_it && call.is_live() {
            // Ring: the card in the window plus the native banner with Accept / Decline.
            let title = sender.display_name();
            let body = format!("Incoming {}", call.kind.label());
            let image = self.avatar_file_for(&sender.avatar_url).or_else(|| {
                self.tree_png_for(sender.avatar.as_deref(), sender.avatar_color.as_deref())
            });
            notify::deliver_call(&call.call_id, &call.chat_id, &title, &body, image);
            self.incoming_call = Some((call, sender));
            self.incoming_since = Some(now_ms());
        }
        self.sync_tones();
        cx.notify();
    }

    fn on_call_ended(&mut self, ended: CallEnded, cx: &mut Context<Self>) {
        let chat_id = if !ended.chat_id.is_empty() {
            Some(ended.chat_id.clone())
        } else {
            self.calls_in_chat
                .iter()
                .find(|(_, c)| c.call_id == ended.call_id)
                .map(|(id, _)| id.clone())
                .or_else(|| {
                    self.my_call
                        .as_ref()
                        .filter(|c| c.call.call_id == ended.call_id)
                        .map(|c| c.call.chat_id.clone())
                })
        };
        if let Some(chat) = &chat_id {
            if self
                .calls_in_chat
                .get(chat)
                .map(|c| c.call_id == ended.call_id)
                .unwrap_or(false)
            {
                self.calls_in_chat.remove(chat);
            }
        }
        if self
            .incoming_call
            .as_ref()
            .map(|(c, _)| c.call_id == ended.call_id)
            .unwrap_or(false)
        {
            self.incoming_call = None;
            notify::withdraw_call(&ended.call_id);
        }
        if self
            .my_call
            .as_ref()
            .map(|c| c.call.call_id == ended.call_id)
            .unwrap_or(false)
        {
            self.my_call = None;
            let _ = self.call_commands.send(CallCmd::Leave);
        }
        // Exactly one device writes the end line (contract §1) — and once: the
        // leave answer and the `call_ended` push may both name this device.
        if ended.writer_is_me {
            if let Some(chat) = chat_id {
                let already = self.chat_has_call_line(&chat, &ended.call_id, "Call ended");
                if !already && self.ended_written.insert(ended.call_id.clone()) {
                    self.command(ClientCommand::SystemLine {
                        chat_id: chat,
                        text: call_ended_line(ended.duration_s),
                        call_id: Some(ended.call_id.clone()),
                    });
                }
            }
        }
        if self.incoming_call.is_none() {
            self.incoming_since = None;
        }
        self.sync_tones();
        cx.notify();
    }

    /// A system line for `call_id` starting with `prefix` is already in the chat.
    fn chat_has_call_line(&self, chat_id: &str, call_id: &str, prefix: &str) -> bool {
        self.messages
            .get(chat_id)
            .map(|list| {
                list.iter().any(|m| {
                    m.role == Role::System
                        && m.call_id.as_deref() == Some(call_id)
                        && m.plain_text().starts_with(prefix)
                })
            })
            .unwrap_or(false)
    }

    /// `call_missed` (contract §8): the system line once per call, and a
    /// native "Missed voice call" banner from the caller.
    fn on_call_missed(&mut self, missed: CallMissed, cx: &mut Context<Self>) {
        if missed.call_id.is_empty() {
            return;
        }
        self.calls_in_chat
            .retain(|_, c| c.call_id != missed.call_id);
        if self
            .incoming_call
            .as_ref()
            .map(|(c, _)| c.call_id == missed.call_id)
            .unwrap_or(false)
        {
            self.incoming_call = None;
            self.incoming_since = None;
            notify::withdraw_call(&missed.call_id);
        }
        if !self.messages.contains_key(&missed.chat_id) {
            // The push beat the chat's doc (app just opened): write once it is here.
            if !self
                .missed_pending
                .iter()
                .any(|(m, _)| m.call_id == missed.call_id)
            {
                self.missed_pending.push((missed, now_ms()));
            }
            return;
        }
        let already = self.chat_has_call_line(&missed.chat_id, &missed.call_id, "Missed call");
        if !already && self.missed_written.insert(missed.call_id.clone()) {
            self.command(ClientCommand::SystemLine {
                chat_id: missed.chat_id.clone(),
                text: missed.line(),
                call_id: Some(missed.call_id.clone()),
            });
            let image = self.avatar_file_for(&missed.from.avatar_url).or_else(|| {
                self.tree_png_for(
                    missed.from.avatar.as_deref(),
                    missed.from.avatar_color.as_deref(),
                )
            });
            notify::deliver(&notify::Notification {
                project_id: missed.project_id.clone(),
                title: missed.from.display_name(),
                body: missed.notification_body(),
                count: 1,
                chat_id: Some(missed.chat_id.clone()),
                image,
            });
        }
        self.sync_tones();
        cx.notify();
    }

    fn on_call_info(&mut self, call: Call, cx: &mut Context<Self>) {
        if call.is_live() {
            if let Some(active) = self
                .my_call
                .as_mut()
                .filter(|c| c.call.call_id == call.call_id)
            {
                active.call.participants = call.participants.clone();
                active.call.ended_at = None;
            }
            self.calls_in_chat.insert(call.chat_id.clone(), call);
        } else {
            self.calls_in_chat.retain(|_, c| c.call_id != call.call_id);
            if self
                .incoming_call
                .as_ref()
                .map(|(c, _)| c.call_id == call.call_id)
                .unwrap_or(false)
            {
                self.incoming_call = None;
                self.incoming_since = None;
                notify::withdraw_call(&call.call_id);
            }
        }
        self.sync_tones();
        cx.notify();
    }

    /// Events from the LiveKit engine.
    fn apply_call_event(&mut self, event: CallEvent, cx: &mut Context<Self>) {
        let Some(active) = self.my_call.as_mut() else {
            return;
        };
        match event {
            CallEvent::Connecting => {
                if active.status == CallStatus::Connected {
                    active.notice = Some("Reconnecting…".into());
                } else {
                    active.status = CallStatus::Connecting;
                }
            }
            CallEvent::Connected => {
                active.status = CallStatus::Connected;
                if active.notice.as_deref() == Some("Reconnecting…") {
                    active.notice = None;
                }
            }
            CallEvent::Participant {
                identity,
                name,
                metadata,
                present,
            } => {
                if present {
                    let mut sender: Sender = serde_json::from_str(&metadata).unwrap_or_default();
                    if sender.name.is_empty() {
                        sender.name = name;
                    }
                    if sender.id.is_empty() {
                        sender.id = identity
                            .trim_start_matches("u_")
                            .trim_start_matches("a_")
                            .to_string();
                    }
                    // People in the call come from the relay list too: keep the richer one.
                    if sender.avatar_url.is_empty() {
                        if let Some(known) = active
                            .call
                            .participants
                            .iter()
                            .find(|p| p.identity() == identity)
                        {
                            sender = known.clone();
                        }
                    }
                    if !sender.avatar_url.is_empty() {
                        self.want_avatar(sender.avatar_url.clone());
                    }
                    let active = self.my_call.as_mut().unwrap();
                    match active.participant_mut(&identity) {
                        Some(p) => p.sender = sender,
                        None => active.participants.push(CallParticipant {
                            identity,
                            sender,
                            speaking: false,
                            muted: false,
                            video: None,
                        }),
                    }
                } else {
                    active.participants.retain(|p| p.identity != identity);
                }
            }
            CallEvent::Video { identity, frame } => {
                if identity == "local" {
                    active.local_video = Some(frame);
                } else if let Some(p) = active.participant_mut(&identity) {
                    p.video = Some(frame);
                }
            }
            CallEvent::VideoOff { identity } => {
                if identity == "local" {
                    active.local_video = None;
                } else if let Some(p) = active.participant_mut(&identity) {
                    p.video = None;
                }
            }
            CallEvent::Speaking(ids) => {
                for p in active.participants.iter_mut() {
                    p.speaking = ids.contains(&p.identity);
                }
            }
            CallEvent::RemoteMuted { identity, muted } => {
                if let Some(p) = active.participant_mut(&identity) {
                    p.muted = muted;
                }
            }
            CallEvent::CameraUnavailable(why) => {
                active.camera = false;
                active.local_video = None;
                active.notice = Some(format!("Camera unavailable: {why}"));
            }
            CallEvent::MicUnavailable(why) => {
                active.notice = Some(format!("No microphone: {why}"));
            }
            CallEvent::Error(message) => {
                if active.status == CallStatus::Connecting {
                    active.status = CallStatus::Failed(message);
                } else {
                    active.notice = Some(message);
                }
            }
            CallEvent::Disconnected => {
                if active.status == CallStatus::Connected {
                    // The engine dropped the room on its own: leave the relay side too.
                    let call_id = active.call.call_id.clone();
                    self.my_call = None;
                    if !call_id.is_empty() {
                        self.command(ClientCommand::LeaveCall { call_id });
                    }
                }
            }
        }
        self.sync_tones();
        cx.notify();
    }

    /// The decoded GitHub avatar for `url` as a PNG on disk (notification attachment).
    fn avatar_file_for(&self, url: &str) -> Option<PathBuf> {
        if url.is_empty() {
            return None;
        }
        self.person_notification_png(url)
    }

    fn tree_png_for(&self, slug: Option<&str>, color: Option<&str>) -> Option<PathBuf> {
        let slug = slug?;
        crate::ui::avatars::notification_png(slug, color)
    }
}

pub fn title_from(text: &str) -> String {
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

/// PNG/JPEG → BGRA frame for GPUI (`RenderImage` is BGRA).
pub fn decode_image(bytes: &[u8]) -> Option<RenderImage> {
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    let mut raw = img.into_raw();
    for px in raw.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(w, h, raw)?;
    Some(RenderImage::new(vec![image::Frame::new(buffer)]))
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

/// The message is waiting on the user: an open `question` part or the legacy
/// `waiting_user` status.
fn needs_input(m: &Message) -> bool {
    m.open_questions().next().is_some()
        || m.parts.iter().any(|p| {
            matches!(
                p,
                Part::Status {
                    code: StatusKind::AskUser,
                    ..
                }
            )
        })
}

/// First non-empty text line, clipped for a notification body.
fn preview(m: &Message) -> String {
    const MAX: usize = 96;
    let line = m
        .parts
        .iter()
        .find_map(|p| match p {
            Part::Text { text, .. } => text.lines().map(str::trim).find(|l| !l.is_empty()),
            _ => None,
        })
        .unwrap_or("");
    if line.chars().count() <= MAX {
        line.to_string()
    } else {
        let cut: String = line.chars().take(MAX - 1).collect();
        format!("{cut}…")
    }
}

pub fn init_source(cx: &mut App) -> Box<dyn SyncSource> {
    let _ = cx;
    crate::sync::source_from_env()
}

/// What the relay lists this device as (`device_name` on sign-in).
fn device_name() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "Mac".into());
    let host = host.trim_end_matches(".local");
    if cfg!(target_os = "macos") {
        format!("{host} (Mac)")
    } else {
        host.to_string()
    }
}
