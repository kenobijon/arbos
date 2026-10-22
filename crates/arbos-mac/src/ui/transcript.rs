//! The chat column body: a bottom-aligned `list()` with one row per markdown
//! block (zeron-study §4.1–§4.4). Rows are diffed by `(id, version)` and
//! spliced; streaming text fades in through the veil.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use gpui::{
    actions, div, list, prelude::*, px, Animation, AnimationExt, AnyElement, App, Context, Div,
    Entity, FontWeight, ListAlignment, ListScrollEvent, ListState, Render, SharedString, Stateful,
    TextRun, Window,
};

use crate::app::{AppEvent, AppState, DetailsTarget};
use crate::model::{
    now_ms, Attachment, AttachmentKind, Id, Message, MessageStatus, Part, Person, Role, StatusKind,
    TurnKind,
};
use crate::ui::approve::{approval_card, ApprovalView};
use crate::ui::attachments;
use crate::ui::avatars;
use crate::ui::cards;
use crate::ui::frost::edge_faded;
use crate::ui::icons::icon;
use crate::ui::markdown::{
    has_fenced_code, inline_spans, polish_assistant_prose, split_blocks, Block,
};
use crate::ui::motion::PulseClock;
use crate::ui::people;
use crate::ui::select::{Selectable, TextSelection};
use crate::ui::theme::{
    Theme, BUBBLE_PAD_X, BUBBLE_PAD_Y, BUBBLE_RADIUS, CHAT_BODY, CHAT_LINE_HEIGHT, COLUMN_MARGIN,
    LINE_HEIGHT, TEXT_SMALL, TEXT_TINY,
};
use crate::ui::veil::Veil;
use crate::ui::widgets::QuestionCard;
use serde_json::Value;

actions!(transcript, [Copy]);

const OVERDRAW_PX: f32 = 320.0;
pub const COLUMN_MAX_WIDTH: f32 = 820.0;
/// Assistant bubble cap (Grok: the bubble never spans the whole pane).
pub const ASSISTANT_MAX_WIDTH: f32 = 620.0;
/// A new time stamp line whenever messages are this far apart.
const STAMP_GAP_MS: u64 = 60 * 60 * 1000;
/// Vertical rhythm measured from Grok Bot's Mac app (parity it.36, 2 px/pt):
/// consecutive bubbles of one sender sit 6 pt apart, a change of sender opens
/// 20 pt, a time stamp has 20 pt above and 24 below, and the last message keeps
/// 54 pt clear of the composer.
const ROW_GAP_HALF: f32 = 3.0;
const TURN_GAP_EXTRA: f32 = 20.0 - 2.0 * ROW_GAP_HALF;
/// Measured glyph-to-bubble, like the reference: the stamp's 13 pt line box
/// carries ~4 pt of its own above and ~7 below the glyphs (measured).
const STAMP_PAD_TOP: f32 = 20.0 - ROW_GAP_HALF - 4.0;
const STAMP_PAD_BOTTOM: f32 = 24.0 - ROW_GAP_HALF - 7.0;
/// Clearance above the composer when pinned (54 pt glyph-to-pill, minus the
/// composer row's own 6 pt top padding and the last row's half-gap). Kept as a
/// trailing list row — not as container padding — so scrolling bubbles reach
/// the real edge of the input instead of hitting an opaque black strip.
const BOTTOM_INSET: f32 = 54.0 - ROW_GAP_HALF - 6.0;
/// People chats (redesign mocks a/f): a 24 pt sender disc at the bubble's
/// bottom-left, 8 pt from it; captions and earlier blocks keep the lane free.
const SENDER_DISC: f32 = 24.0;
const SENDER_DISC_GAP: f32 = 8.0;
const SENDER_LANE: f32 = SENDER_DISC + SENDER_DISC_GAP;
/// Messages for Mac fades a row's hover-only affordances (time, react glyph)
/// in and out rather than snapping them.
const HOVER_FADE_MS: f32 = 150.0;
/// The fade band under the floating header (one number for every chat kind).
pub const HEADER_FADE_BAND: f32 = crate::ui::header::metrics::FADE_BAND;
/// How long the hover copy glyph stays a checkmark after a successful copy.
const COPY_FEEDBACK_MS: u64 = 2_000;

/// Which bubble row the pointer is over, and the one it just left, with the
/// moment each happened: the fade is computed from those at render time.
#[derive(Default)]
struct HoverFade {
    over: Option<(String, Instant)>,
    left: Option<(String, Instant)>,
}

impl HoverFade {
    /// Record a hover change for `row_id`; `true` when something changed.
    fn set(&mut self, row_id: &str, hovered: bool, now: Instant) -> bool {
        let is_over = self.over.as_ref().is_some_and(|(id, _)| id == row_id);
        if hovered == is_over {
            return false;
        }
        if hovered {
            if let Some((prev, _)) = self.over.take() {
                self.left = Some((prev, now));
            }
            // Re-entering mid-fade starts the fade-in from the current level.
            let head_start = match &self.left {
                Some((id, at)) if id == row_id => {
                    let remaining = 1.0 - Self::progress(*at, now);
                    Duration::from_secs_f32(remaining * HOVER_FADE_MS / 1000.0)
                }
                _ => Duration::ZERO,
            };
            self.left = None;
            self.over = Some((row_id.to_string(), now - head_start));
        } else {
            self.over = None;
            self.left = Some((row_id.to_string(), now));
        }
        true
    }

    fn progress(since: Instant, now: Instant) -> f32 {
        (now.saturating_duration_since(since).as_millis() as f32 / HOVER_FADE_MS).clamp(0.0, 1.0)
    }

    /// Opacity for `row_id`'s affordances now, and whether it is still moving.
    fn opacity(&self, row_id: &str, now: Instant) -> (f32, bool) {
        match (&self.over, &self.left) {
            (Some((id, at)), _) if id == row_id => {
                let t = Self::progress(*at, now);
                (crate::ui::motion::ease_out_cubic(t), t < 1.0)
            }
            (_, Some((id, at))) if id == row_id => {
                let t = Self::progress(*at, now);
                (1.0 - crate::ui::motion::ease_out_cubic(t), t < 1.0)
            }
            _ => (0.0, false),
        }
    }
}

/// Grok's quote header inside a reply bubble: speech-bubble glyph, then the
/// replied-to message's first words in quotes. Clicking it reveals the original.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Quote {
    /// The message replied to (`replyTo` on the row).
    pub message_id: Id,
    /// Its first line, cut to [`QUOTE_CHARS`] characters with an ellipsis.
    pub text: String,
}

/// The quote header shows this many characters of the original before "…".
pub const QUOTE_CHARS: usize = 64;

impl Quote {
    /// The quote for a message that replies to `reply_to`; a reply whose original is not
    /// loaded (older than the snapshot) still shows a header so the reference is never lost.
    pub fn for_reply(reply_to: &str, messages: &[Message]) -> Self {
        let text = messages
            .iter()
            .find(|m| m.id == reply_to)
            .map(quote_text)
            .unwrap_or_else(|| "Earlier message".to_owned());
        Self {
            message_id: reply_to.to_owned(),
            text,
        }
    }
}

/// First words of a message for its quote header: the first non-empty line,
/// whitespace collapsed, cut on a word boundary; attachments name their file.
pub fn quote_text(message: &Message) -> String {
    let plain = message.plain_text();
    let line = plain
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let line = if line.is_empty() {
        match message.parts.iter().find_map(|p| match p {
            Part::Attachment { attachment, .. } => Some(attachment.name.clone()),
            _ => None,
        }) {
            Some(name) => name,
            None => return "Message".to_owned(),
        }
    } else {
        line
    };
    if line.chars().count() <= QUOTE_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(QUOTE_CHARS).collect();
    if let Some(ix) = cut.rfind(' ') {
        if ix > QUOTE_CHARS / 2 {
            cut.truncate(ix);
        }
    }
    format!("{}…", cut.trim_end_matches([',', ';', ':', '.']))
}

#[derive(Clone, Debug)]
pub enum RowKind {
    User {
        text: String,
        pending: bool,
        /// Caption for prompts not typed by the user (a routine, another project).
        sender_label: Option<String>,
        /// Another person's bubble shows their name above it only when the sender
        /// changed (Grok Bot's group style).
        named: bool,
        /// Another person in a people chat (D72): left bubble with their
        /// avatar and handle instead of my right bubble.
        from: Option<Person>,
        /// "9:41 PM", revealed at the bubble's outer edge on hover.
        time: Option<String>,
        /// Set when this message replies to another: Grok's quote header
        /// sits inside the bubble above the text.
        quote: Option<Quote>,
    },
    Markdown {
        block: Block,
        live: bool,
        /// Where this block sits in its message's bubble (first/last round
        /// the corners; `fill` makes a multi-block bubble span the cap width
        /// so every block has the same edge).
        first: bool,
        last: bool,
        fill: bool,
        /// "9:41 PM", revealed next to the react glyph on hover.
        time: Option<String>,
        /// In a people chat the message has a sender row above it: every block
        /// leaves the avatar lane free and the last one draws the sender's disc
        /// at the bubble's bottom-left (redesign mocks a/f). The sender seed.
        lane: Option<String>,
    },
    /// A finished reply with no words, cards or attachments: one plain line in the
    /// agent's bubble. `name` is the row's `senderName`; the agent id in `seed` resolves
    /// the name when the row carries none (older boxes), like the sender header does.
    NoAnswer {
        seed: String,
        name: Option<String>,
        time: Option<String>,
        lane: Option<String>,
    },
    /// Centered muted time line ("Today 9:39 PM").
    Stamp {
        label: String,
    },
    /// Emoji reaction chips under a bubble (iPhone parity): `(emoji, senders)`.
    Reactions {
        reactions: Vec<(String, Vec<String>)>,
        /// Under my right-hand bubble (chips align right).
        mine: bool,
        /// Under another person's bubble (chips clear the avatar column).
        indent: bool,
    },
    /// Grok's hand-off line: "Messaged ◉ A" / "N messages with ◉ A, ◉ B and ◉ C".
    Handoff {
        names: Vec<String>,
        total: u32,
    },
    /// An image or file in the chat (agent or user). `mine` right-aligns my sends.
    Attachment {
        attachment: Attachment,
        mine: bool,
        /// Someone else's (person or agent) in a people chat: keep the
        /// sender's avatar lane free, like the message's bubble.
        lane: bool,
    },
    /// S8: the agent asked a question.
    AskUser {
        text: String,
    },
    /// S18: draft-and-approve card.
    Approval {
        view: ApprovalView,
    },
    /// Error: one line, red glyph, Retry.
    /// A typed question widget (design §10): rendered by a [`QuestionCard`] view.
    Question {
        question_id: String,
    },
    Error {
        code: String,
        message: String,
    },
    /// Tiny inline glyph (■ stopped, ✗ failed) — no words.
    Glyph {
        glyph: &'static str,
    },
    /// The one working indicator for the chat: a pulsing dot under the last
    /// message while anything runs here. `queued` tints it amber.
    Indicator {
        queued: bool,
        /// "Cloning RaoFoundation/subtensor" — else "<name> is working".
        activity: Option<String>,
    },
    /// Debug-only "Show details" disclosure under a finished reply.
    Details,
    /// F6: who speaks next — another Project's glyph and name above its message.
    Sender {
        name: String,
        icon: Option<String>,
        /// Stable seed for the chip colour (the sender id).
        seed: String,
        /// Tree-character slug the relay put on the reply (`senderAvatar`).
        avatar: Option<String>,
        /// Its colour (`senderAvatarColor`); `None` = look it up / mono.
        avatar_color: Option<String>,
    },
    /// F11: small speaker under a reply; click speaks it (again).
    Speaker {
        /// The speech is already cached (instant replay) vs. will be synthesised.
        cached: bool,
        /// The last attempt to speak this reply failed; the row is a retry.
        failed: bool,
    },
    /// Empty height at the end of the list so the last bubble sits clear of
    /// the composer when pinned; scrolls away so mid-thread bubbles can reach
    /// the input's real edge (no opaque padding strip).
    BottomInset,
}

/// Which replies get a speaker row (F11).
#[derive(Clone, Copy)]
pub struct SpeechRows<'a> {
    /// Relay flag `features.voice_mode`; off = no speaker rows at all.
    pub enabled: bool,
    pub spoken: &'a HashSet<Id>,
    /// Replies whose speech failed (reason): their speaker row offers a retry.
    pub failed: &'a HashMap<Id, String>,
    pub voice_mode: bool,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub version: u64,
    pub message_id: Id,
    pub kind: RowKind,
}

/// Stick-to-bottom velocity spring (zeron-study §4.3). Each frame it moves a
/// fraction of the remaining distance to the end, damped, so streamed text
/// glides instead of jumping; a user wheel-up breaks the pin.
#[derive(Default)]
struct StickSpring {
    velocity: f32,
}

const SPRING_STIFFNESS: f32 = 0.22;
const SPRING_DAMPING: f32 = 0.55;
pub const STICK_THRESHOLD_PX: f32 = 70.0;
/// How far above the end the user must be before the ↓ button shows.
const JUMP_BUTTON_AFTER_PX: f32 = 160.0;

/// Right-click on a bubble: the shell opens the react menu there.
/// Hover reply: the shell sets the composer's reply target.
pub enum TranscriptEvent {
    ReactMenu {
        chat_id: Id,
        message_id: Id,
        position: gpui::Point<gpui::Pixels>,
    },
    Reply {
        chat_id: Id,
        message_id: Id,
    },
}

impl gpui::EventEmitter<TranscriptEvent> for Transcript {}

pub struct Transcript {
    state: Entity<AppState>,
    chat_id: Option<Id>,
    list: ListState,
    rows: Vec<Row>,
    /// Whether the right pane was open at the last sync: its opening or
    /// closing changes the column width, and the cached row layouts must go.
    pane_open: bool,
    veils: HashMap<String, Veil>,
    /// One stateful card per typed question, keyed by question id; dropped with its row.
    cards: HashMap<String, Entity<QuestionCard>>,
    pinned_to_bottom: bool,
    /// Reveal request (`AppState::reveal_nonce`) we already scrolled for.
    revealed: Option<u64>,
    spring: StickSpring,
    /// Set by the scroll callback; the next frame re-pins when the user
    /// stopped within `STICK_THRESHOLD_PX` of the end.
    repin_check_pending: bool,
    /// The working line's current text and when it changed (fade on swap).
    activity_shown: Option<(String, Instant)>,
    hover: HoverFade,
    /// Text selected in a bubble (drag / double-click / triple-click); ⌘C copies it.
    selection: Entity<TextSelection>,
    /// Messages whose sender differs from the one before: their first row opens
    /// the wider turn gap.
    turn_starts: HashSet<Id>,
    /// Message whose hover copy glyph is showing a checkmark, and when it started.
    copied_feedback: Option<(Id, Instant)>,
}

impl Transcript {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW_PX));
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |event: &ListScrollEvent, _, cx| {
            let _ = weak.update(cx, |this, _| this.handle_scroll(event));
        });
        let selection = cx.new(TextSelection::new);
        cx.observe(&selection, |_, _, cx| cx.notify()).detach();
        cx.observe(&state, |this, _, cx| this.sync(cx)).detach();
        cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::TranscriptText { chat_id, .. } | AppEvent::TranscriptChanged { chat_id } => {
                if this.chat_id.as_deref() == Some(chat_id.as_str()) {
                    this.sync(cx);
                }
            }
            AppEvent::VoiceTranscript { .. }
            | AppEvent::VoiceSpeech { .. }
            | AppEvent::ReplaySpeech { .. } => {}
        })
        .detach();
        Self {
            state,
            chat_id: None,
            list,
            rows: Vec::new(),
            pane_open: false,
            veils: HashMap::new(),
            cards: HashMap::new(),
            pinned_to_bottom: true,
            revealed: None,
            spring: StickSpring::default(),
            repin_check_pending: false,
            activity_shown: None,
            hover: HoverFade::default(),
            selection,
            turn_starts: HashSet::new(),
            copied_feedback: None,
        }
    }

    /// ⌘C with text selected in a bubble.
    fn copy_selection(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.selection.read(cx).selected_text() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }

    /// The row around a bubble: tracks the pointer so the time and react glyph
    /// can fade, and reports their opacity for this frame. With Reduce Motion
    /// the fade collapses to the snap it replaced.
    fn hover_row(
        &self,
        row_id: &str,
        now: Instant,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> (Stateful<Div>, f32) {
        let (opacity, moving) = self.hover.opacity(row_id, now);
        let opacity = if cx.reduce_motion() {
            opacity.round()
        } else {
            if moving {
                window.request_animation_frame();
            }
            opacity
        };
        let id = row_id.to_string();
        let row = div()
            .id(SharedString::from(format!("{row_id}#row")))
            .w_full()
            .flex()
            .items_center()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if this.hover.set(&id, *hovered, Instant::now()) {
                    cx.notify();
                }
            }));
        (row, opacity)
    }

    /// Runs inside the list's own scroll callback while `ListState` is
    /// mutably borrowed, so it must not touch the list. Record the intent
    /// and let the next frame (`step_spring`) read pixel offsets.
    fn handle_scroll(&mut self, event: &ListScrollEvent) {
        // `is_scrolled == false` means the bottom-aligned list snapped to its
        // end: the user is back at the tail. Direction is handled by the
        // wheel listener; this callback never re-pins on its own otherwise.
        if !event.is_scrolled {
            self.pinned_to_bottom = true;
            self.spring.velocity = 0.0;
        }
    }

    /// Wheel/trackpad over the transcript: scrolling up (positive delta,
    /// toward older messages) breaks the pin at once; scrolling down asks the
    /// next frame to re-pin when within `STICK_THRESHOLD_PX` of the end.
    fn on_wheel(&mut self, event: &gpui::ScrollWheelEvent, cx: &mut Context<Self>) {
        let dy = f32::from(event.delta.pixel_delta(px(20.0)).y);
        if dy > 0.0 {
            self.pinned_to_bottom = false;
            self.spring.velocity = 0.0;
            self.repin_check_pending = false;
        } else if dy < 0.0 {
            self.repin_check_pending = true;
        }
        cx.notify();
    }

    /// One spring step per frame while pinned and not yet at the end.
    fn step_spring(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if self.repin_check_pending {
            self.repin_check_pending = false;
            let max = f32::from(self.list.max_offset_for_scrollbar().y);
            let cur = -f32::from(self.list.scroll_px_offset_for_scrollbar().y);
            if max - cur <= STICK_THRESHOLD_PX {
                self.pinned_to_bottom = true;
            }
        }
        if !self.pinned_to_bottom {
            return;
        }
        let max = f32::from(self.list.max_offset_for_scrollbar().y);
        let cur = -f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        let remaining = max - cur;
        if remaining.abs() < 0.5 && self.spring.velocity.abs() < 0.5 {
            self.spring.velocity = 0.0;
            return;
        }
        self.spring.velocity =
            self.spring.velocity * (1.0 - SPRING_DAMPING) + remaining * SPRING_STIFFNESS;
        let step = if self.spring.velocity.abs() > remaining.abs() {
            remaining
        } else {
            self.spring.velocity
        };
        if step.abs() >= 0.25 {
            self.list.scroll_by(px(step));
            window.request_animation_frame();
        }
    }

    pub fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let chat_id = state.active_chat_id();
        let chat_changed = chat_id != self.chat_id;
        let pane_open = state.profile.is_some();
        let pane_changed = pane_open != self.pane_open;
        self.pane_open = pane_open;
        let messages: Vec<Message> = chat_id
            .as_deref()
            .map(|id| state.messages_of(id).to_vec())
            .unwrap_or_default();
        self.chat_id = chat_id;

        let show_details = state.show_details;
        let speech = SpeechRows {
            enabled: state.voice_mode_available(),
            spoken: &state.spoken,
            failed: &state.speech_failed,
            voice_mode: state.voice_mode,
        };
        let me_id = state.me_id();
        let thumb_keys: HashSet<&str> = state.thumbnails.keys().map(|k| k.as_str()).collect();
        let new_rows = build_rows(&messages, show_details, speech, &me_id, &thumb_keys);
        self.turn_starts = messages
            .windows(2)
            .filter(|w| (w[0].role, &w[0].sender) != (w[1].role, &w[1].sender))
            .map(|w| w[1].id.clone())
            .collect();

        // Veils: observe text growth of live markdown rows.
        for row in &new_rows {
            if let RowKind::Markdown {
                block, live: true, ..
            } = &row.kind
            {
                let len = block_text_len(block);
                let veil = self
                    .veils
                    .entry(row.id.clone())
                    .or_insert_with(|| Veil::seeded(0));
                veil.observe(len);
            }
        }
        self.veils
            .retain(|id, _| new_rows.iter().any(|r| r.id == *id));
        self.cards.retain(|qid, _| {
            new_rows
                .iter()
                .any(|r| matches!(&r.kind, RowKind::Question { question_id } if question_id == qid))
        });

        if chat_changed {
            self.rows = new_rows;
            self.list.reset(self.rows.len());
            self.pinned_to_bottom = true;
            self.revealed = None;
            self.list.scroll_to_end();
            // `ARBOS_SCROLL_UP=1`: open at the top instead (screenshot rig; the
            // GUI session accepts no synthetic wheel events).
            if std::env::var("ARBOS_SCROLL_UP").is_ok_and(|v| v == "1")
                && self.state.read(cx).reveal_message.is_none()
            {
                self.pinned_to_bottom = false;
                self.list.scroll_to(gpui::ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.0),
                });
            }
            self.reveal_hit(cx);
            cx.notify();
            return;
        }

        let prefix = self
            .rows
            .iter()
            .zip(new_rows.iter())
            .take_while(|(a, b)| a.id == b.id && a.version == b.version)
            .count();
        if prefix == self.rows.len() && prefix == new_rows.len() {
            if pane_changed {
                // Same rows, new column width: measure them again.
                self.list.reset(self.rows.len());
                if self.pinned_to_bottom {
                    self.list.scroll_to_end();
                }
                cx.notify();
            }
            self.reveal_hit(cx);
            return;
        }
        let old_len = self.rows.len();
        self.rows = new_rows;
        self.list.splice(prefix..old_len, self.rows.len() - prefix);
        self.reveal_hit(cx);
        // While pinned the spring glides to the new end on the next frames.
        cx.notify();
    }

    /// A home-search hit or a quote click asked to land on a message. Scroll
    /// once per request, when its row exists.
    fn reveal_hit(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let Some((chat, msg)) = state.reveal_message.clone() else {
            self.revealed = None;
            return;
        };
        let nonce = state.reveal_nonce;
        if self.chat_id.as_deref() != Some(chat.as_str()) {
            return;
        }
        if self.revealed == Some(nonce) {
            return;
        }
        let Some(ix) = self.rows.iter().position(|r| {
            r.message_id == msg
                && !matches!(
                    r.kind,
                    RowKind::Stamp { .. } | RowKind::Indicator { .. } | RowKind::BottomInset
                )
        }) else {
            return;
        };
        self.pinned_to_bottom = false;
        self.list.scroll_to(gpui::ListOffset {
            item_ix: ix,
            offset_in_item: px(0.0),
        });
        self.revealed = Some(nonce);
        cx.notify();
    }

    /// Plain text of a message in this chat, for copy / control-click.
    fn message_plain(&self, message_id: &Id, cx: &App) -> Option<String> {
        let chat_id = self.chat_id.as_ref()?;
        self.state
            .read(cx)
            .messages
            .get(chat_id)
            .and_then(|list| list.iter().find(|m| m.id == *message_id))
            .map(|m| m.plain_text())
    }

    fn copy_message(&mut self, message_id: &Id, cx: &mut Context<Self>) {
        if let Some(text) = self.message_plain(message_id, cx) {
            if !text.is_empty() {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                self.copied_feedback = Some((message_id.clone(), Instant::now()));
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(COPY_FEEDBACK_MS))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if this.copied_feedback.as_ref().is_some_and(|(_, at)| {
                            at.elapsed() >= Duration::from_millis(COPY_FEEDBACK_MS)
                        }) {
                            this.copied_feedback = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// Right-click → react menu. Control-click → copy the message text.
    fn react_target(
        &self,
        el: Stateful<Div>,
        message_id: &Id,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let Some(chat_id) = self.chat_id.clone() else {
            return el;
        };
        let (right_chat, right_msg) = (chat_id, message_id.clone());
        let ctrl_msg = message_id.clone();
        el.on_mouse_down(
            gpui::MouseButton::Right,
            cx.listener(move |_, event: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                cx.emit(TranscriptEvent::ReactMenu {
                    chat_id: right_chat.clone(),
                    message_id: right_msg.clone(),
                    position: event.position,
                });
            }),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                if !event.modifiers.control {
                    return;
                }
                cx.stop_propagation();
                this.copy_message(&ctrl_msg, cx);
            }),
        )
    }

    /// Grok's quote header at the top of a reply bubble: speech-bubble glyph,
    /// then the original's first words in quotes, one line, truncated. Click
    /// → scroll to the original and highlight it for a moment.
    fn quote_header(&self, row_id: &str, quote: Quote, text_color: gpui::Hsla) -> Stateful<Div> {
        let state = self.state.clone();
        let chat_id = self.chat_id.clone();
        let target = quote.message_id.clone();
        let muted = Theme::with_alpha(text_color, 0.72);
        div()
            .id(SharedString::from(format!("{row_id}#quote")))
            .flex()
            .items_center()
            .gap(px(6.0))
            .min_w_0()
            .cursor_pointer()
            .hover(|s| s.opacity(0.8))
            .child(icon("bubble", 12.0, muted))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(TEXT_SMALL))
                    .line_height(px(LINE_HEIGHT - 4.0))
                    .text_color(muted)
                    .child(format!("“{}”", quote.text)),
            )
            .on_click(move |_, _, cx| {
                if let Some(chat_id) = chat_id.clone() {
                    let target = target.clone();
                    state.update(cx, |s, cx| s.reveal_reply(chat_id, target, cx));
                }
            })
    }

    /// Hover-only reply + copy glyphs beside a bubble (fade with the row).
    /// After a successful copy the glyph becomes a checkmark for a moment.
    fn hover_reply_copy(
        &self,
        row_id: &str,
        message_id: &Id,
        opacity: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let chat_id = self.chat_id.clone();
        let reply_msg = message_id.clone();
        let copy_msg = message_id.clone();
        let just_copied = self.copied_feedback.as_ref().is_some_and(|(id, at)| {
            id == message_id && at.elapsed() < Duration::from_millis(COPY_FEEDBACK_MS)
        });
        // Keep the checkmark readable even if the pointer leaves mid-feedback.
        let copy_opacity = if just_copied { 1.0 } else { opacity };
        let copy_glyph = if just_copied { "check" } else { "copy" };
        let copy_color = if just_copied {
            theme.online
        } else {
            theme.text_muted
        };
        let btn = |id: SharedString, glyph: &'static str, opacity: f32, color: gpui::Hsla| {
            div()
                .id(id)
                .size(px(24.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .opacity(opacity)
                .hover(|s| s.bg(theme.surface_2))
                .child(icon(glyph, 16.0, color))
        };
        div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(
                btn(
                    SharedString::from(format!("{row_id}#reply")),
                    "reply",
                    opacity,
                    theme.text_muted,
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    if let Some(chat_id) = chat_id.clone() {
                        cx.emit(TranscriptEvent::Reply {
                            chat_id,
                            message_id: reply_msg.clone(),
                        });
                    }
                })),
            )
            .child(
                btn(
                    SharedString::from(format!("{row_id}#copy")),
                    copy_glyph,
                    copy_opacity,
                    copy_color,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.copy_message(&copy_msg, cx))),
            )
    }

    /// A sender row's agent, resolved like the chip: a shared agent's reply may
    /// carry only its handle, so name, disc seed and colour come from the chat's
    /// member list or my projects. Returns (name, disc seed, colour index, project id).
    fn agent_identity(
        &self,
        seed: &str,
        name: &str,
        cx: &Context<Self>,
    ) -> (String, String, Option<u8>, Option<Id>) {
        let a = self.agent_identity_full(seed, name, cx);
        (a.name, a.seed, a.color_index, a.project_id)
    }

    /// `agent_identity` plus the D92 leaf pair in effect.
    fn agent_identity_full(&self, seed: &str, name: &str, cx: &Context<Self>) -> AgentIdentity {
        let state = self.state.read(cx);
        let agent_pid = seed
            .strip_prefix("agent:")
            .or_else(|| seed.strip_prefix("project:"))
            .map(str::to_string);
        let member = agent_pid.as_deref().and_then(|pid| {
            state
                .selected()
                .and_then(|space| space.agents.iter().find(|a| a.project_id == pid))
                .map(|a| {
                    (
                        a.name.clone(),
                        a.avatar_color_index,
                        (a.avatar_leaf, a.avatar_leaf_color),
                    )
                })
                .or_else(|| {
                    state.projects.iter().find(|p| p.id == pid).map(|p| {
                        (
                            p.name.clone(),
                            p.avatar_color_index,
                            (p.avatar_leaf, p.avatar_leaf_color),
                        )
                    })
                })
        });
        let resolved_name = match &member {
            Some((n, _, _)) if name == "Agent" || name == "Project" => n.clone(),
            _ => name.to_string(),
        };
        let color_index = member.as_ref().and_then(|(_, c, _)| *c);
        let leaf = member.as_ref().map(|(_, _, l)| *l).unwrap_or((None, None));
        let disc_seed = agent_pid.clone().unwrap_or_else(|| seed.to_string());
        AgentIdentity {
            name: resolved_name,
            seed: disc_seed,
            color_index,
            leaf,
            project_id: agent_pid,
        }
    }

    /// The 24 pt disc for an agent's message in a people chat.
    fn agent_disc(&self, seed: &str, cx: &Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let a = self.agent_identity_full(seed, "Agent", cx);
        avatars::agent(
            &a.seed,
            &a.name,
            a.color_index,
            a.leaf.0,
            a.leaf.1,
            SENDER_DISC,
            &theme,
        )
    }

    /// One assistant bubble block (the Markdown row and the no-answer line share it).
    #[allow(clippy::too_many_arguments)]
    fn markdown_bubble(
        &mut self,
        row_id: &str,
        message_id: &Id,
        block: Block,
        live: bool,
        first: bool,
        last: bool,
        fill: bool,
        time: Option<String>,
        lane: Option<String>,
        now: Instant,
        theme: &Theme,
        selectable: &Selectable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let veil = if live {
            self.veils.get(row_id).cloned()
        } else {
            None
        };
        let fading = veil.as_ref().map(|v| v.is_active(now)).unwrap_or(false);
        if live || fading {
            PulseClock::request(cx);
        }
        // One assistant bubble, drawn block by block: the first block
        // rounds the top, the last the bottom, the rest run straight.
        // People-chat agent bubbles get the same coloured left-edge
        // sliver as person messages, from the agent's disc colour.
        let agent_accent = lane.as_deref().map(|seed| {
            let a = self.agent_identity_full(seed, "Agent", cx);
            avatars::agent_accent(&a.seed, a.color_index, a.leaf.0, a.leaf.1, theme)
        });
        let r = px(BUBBLE_RADIUS);
        let bubble = self
            .react_target(
                div().id(SharedString::from(format!("{}#bubble", row_id))),
                message_id,
                cx,
            )
            .max_w(px(ASSISTANT_MAX_WIDTH))
            // A flex item's auto min-width is its content's; without
            // this the bubble keeps 620 pt when the right pane opens
            // and runs under it instead of re-wrapping.
            .min_w_0()
            .when(fill, |el| el.w_full())
            .px(px(BUBBLE_PAD_X))
            .when(first, |el| el.pt(px(BUBBLE_PAD_Y)).rounded_t(r))
            .when(!first, |el| el.pt(px(3.0)))
            .when(last, |el| el.pb(px(BUBBLE_PAD_Y)).rounded_b(r))
            .when(!last, |el| el.pb(px(3.0)))
            .bg(theme.bubble_assistant)
            .when_some(agent_accent, |el, color| {
                el.border_l_2().border_color(Theme::with_alpha(color, 0.7))
            })
            .child(render_block(
                row_id,
                &block,
                veil.as_ref(),
                now,
                theme,
                selectable,
            ));
        let (row_el, reveal) = self.hover_row(row_id, now, window, cx);
        // Hover: reply + copy, then Grok Mac's muted react glyph (opens
        // the same bar as a right-click).
        let hover_actions = (last && !live).then(|| {
            let actions = self.hover_reply_copy(row_id, message_id, reveal, theme, cx);
            let chat_id = self.chat_id.clone();
            let message_id = message_id.clone();
            let react = div()
                .id(SharedString::from(format!("{}#react", row_id)))
                .size(px(24.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .opacity(reveal)
                .hover(|s| s.bg(theme.surface_2))
                .child(icon("face-smile", 16.0, theme.text_muted))
                .on_click(cx.listener(move |_, event: &gpui::ClickEvent, _, cx| {
                    if let Some(chat_id) = chat_id.clone() {
                        cx.emit(TranscriptEvent::ReactMenu {
                            chat_id,
                            message_id: message_id.clone(),
                            position: event.position(),
                        });
                    }
                }));
            div()
                .ml(px(6.0))
                .flex()
                .items_center()
                .gap(px(2.0))
                .child(actions)
                .child(react)
        });
        // Messages for Mac: the bubble's time shows beside the glyph on hover.
        let hover_time = (last && !live)
            .then(|| hover_time(time, reveal, theme).map(|t| t.ml(px(8.0))))
            .flatten();
        // People chats: the sender's disc at the bubble's bottom-left on
        // the last block; earlier blocks keep the lane free (mocks a/f).
        let disc = lane.as_deref().map(|seed| {
            if last {
                // The disc opens the agent's profile, pushed over the group's pane.
                let (_, _, _, agent_pid) = self.agent_identity(seed, "Agent", cx);
                let open_state = self.state.clone();
                div()
                    .id(SharedString::from(format!("{row_id}#disc")))
                    .flex_shrink_0()
                    .child(self.agent_disc(seed, cx))
                    .when_some(agent_pid, |el, pid| {
                        el.cursor_pointer().on_click(move |_, _, cx| {
                            let pid = pid.clone();
                            open_state.update(cx, |s, cx| {
                                s.push_profile(
                                    crate::app::ProfilePage::Agent { project_id: pid },
                                    cx,
                                )
                            });
                        })
                    })
                    .into_any_element()
            } else {
                div().w(px(SENDER_DISC)).flex_shrink_0().into_any_element()
            }
        });
        row_el
            .justify_start()
            .items_end()
            .when(first, |el| el.pt(px(ROW_GAP_HALF)))
            .when(last, |el| el.pb(px(ROW_GAP_HALF)))
            .when(lane.is_some(), |el| el.gap(px(SENDER_DISC_GAP)))
            .children(disc)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .child(bubble)
                    .children(hover_actions)
                    .children(hover_time),
            )
            .into_any_element()
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let now = Instant::now();
        let selectable = Selectable::new(&self.selection, theme.accent_soft, cx);
        let inner: AnyElement = match row.kind {
            RowKind::User {
                text,
                pending,
                sender_label,
                from,
                named,
                time,
                quote,
            } => {
                // My bubble is dark in both appearances: white text, a
                // stronger highlight than the soft one on the grey bubbles.
                let (text_color, bubble_selectable) = if from.is_some() {
                    (theme.text, selectable.clone())
                } else {
                    (
                        theme.bubble_text(),
                        selectable.with_color(Theme::with_alpha(theme.accent, 0.45)),
                    )
                };
                let quote_header = quote.map(|q| self.quote_header(&row.id, q, text_color));
                // Triple-backtick fences render as code cards (same as agent
                // replies); other text stays plain so "# heading" is still literal.
                // Bare `http(s)://…` URLs are still accented and clickable.
                let body =
                    user_message_body(&row.id, &text, text_color, &theme, &bubble_selectable);
                let bubble = self
                    .react_target(
                        div().id(SharedString::from(format!("{}#bubble", row.id))),
                        &row.message_id,
                        cx,
                    )
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .max_w(relative_width(0.72))
                    .px(px(BUBBLE_PAD_X))
                    .py(px(BUBBLE_PAD_Y))
                    .rounded(px(BUBBLE_RADIUS))
                    .bg(theme.bubble)
                    .text_color(theme.bubble_text())
                    .text_size(px(CHAT_BODY))
                    .line_height(px(CHAT_LINE_HEIGHT))
                    .children(quote_header)
                    .child(body);
                if let Some(person) = from {
                    // Someone else: left-aligned, their avatar and name above.
                    // A small coloured left-edge sliver marks a person message
                    // so it reads apart from an agent bubble: the dominant
                    // colour of their picture, else their D79 disc colour.
                    // Must not early-return: the match arm still needs the
                    // COLUMN_MARGIN wrapper (same padding as agent bubbles).
                    let (image, color) = {
                        let state = self.state.read(cx);
                        (
                            state.avatar(&person.avatar_url),
                            state.sender_accent(&person, &theme),
                        )
                    };
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px(6.0))
                        .py(px(ROW_GAP_HALF))
                        .when(named, |el| {
                            el.child({
                                // Click the caption → the person's profile page (pushed; ‹ returns).
                                let open_state = self.state.clone();
                                let handle = person.handle.clone();
                                let sid = self.chat_id.as_ref().and_then(|c| {
                                    self.state.read(cx).chats.iter().find_map(|(pid, tabs)| {
                                        tabs.iter().any(|t| &t.id == c).then(|| pid.clone())
                                    })
                                });
                                // The caption starts where the bubble's text does.
                                div()
                                    .id(SharedString::from(format!("{}#who", row.id)))
                                    .pl(px(SENDER_LANE + BUBBLE_PAD_X))
                                    .cursor_pointer()
                                    .child(
                                        div()
                                            .text_size(px(TEXT_TINY))
                                            .text_color(theme.text_muted)
                                            .child(person.display_name()),
                                    )
                                    .on_click(move |_, _, cx| {
                                        let (handle, sid) = (handle.clone(), sid.clone());
                                        open_state.update(cx, |s, cx| {
                                            s.push_profile(
                                                crate::app::ProfilePage::User {
                                                    handle,
                                                    space_id: sid,
                                                },
                                                cx,
                                            )
                                        });
                                    })
                            })
                        })
                        .child({
                            let (row_el, reveal) = self.hover_row(&row.id, now, window, cx);
                            let actions =
                                self.hover_reply_copy(&row.id, &row.message_id, reveal, &theme, cx);
                            row_el
                                .justify_start()
                                .items_end()
                                .gap(px(SENDER_DISC_GAP))
                                .child({
                                    // The disc opens the same profile as the name.
                                    let open_state = self.state.clone();
                                    let handle = person.handle.clone();
                                    let sid = self.chat_id.as_ref().and_then(|c| {
                                        self.state.read(cx).chats.iter().find_map(|(pid, tabs)| {
                                            tabs.iter().any(|t| &t.id == c).then(|| pid.clone())
                                        })
                                    });
                                    div()
                                        .id(SharedString::from(format!("{}#disc", row.id)))
                                        .flex_shrink_0()
                                        .cursor_pointer()
                                        .child(people::avatar(image, &person, SENDER_DISC, &theme))
                                        .on_click(move |_, _, cx| {
                                            let (handle, sid) = (handle.clone(), sid.clone());
                                            open_state.update(cx, |s, cx| {
                                                s.push_profile(
                                                    crate::app::ProfilePage::User {
                                                        handle,
                                                        space_id: sid,
                                                    },
                                                    cx,
                                                )
                                            });
                                        })
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .child(
                                            bubble
                                                .bg(theme.bubble_assistant)
                                                .text_color(theme.text)
                                                .border_l_2()
                                                .border_color(Theme::with_alpha(color, 0.7))
                                                .when(pending, |el| el.opacity(0.6)),
                                        )
                                        .child(actions.ml(px(6.0)))
                                        .children(
                                            hover_time(time, reveal, &theme).map(|t| t.ml(px(8.0))),
                                        ),
                                )
                        })
                        .into_any_element()
                } else {
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_end()
                        .gap(px(3.0))
                        .py(px(ROW_GAP_HALF))
                        .when_some(sender_label, |el, label| {
                            el.child(
                                div()
                                    .pr(px(6.0))
                                    .text_size(px(TEXT_TINY))
                                    .text_color(theme.text_muted)
                                    .child(label),
                            )
                        })
                        .child({
                            // My bubble sits right, so its outer edge (and time) is on the left.
                            let (row_el, reveal) = self.hover_row(&row.id, now, window, cx);
                            let actions =
                                self.hover_reply_copy(&row.id, &row.message_id, reveal, &theme, cx);
                            row_el
                                .justify_end()
                                .children(hover_time(time, reveal, &theme).map(|t| t.mr(px(8.0))))
                                .child(actions.mr(px(6.0)))
                                .child(bubble.when(pending, |el| el.opacity(0.6)))
                        })
                        .into_any_element()
                }
            }
            RowKind::Markdown {
                block,
                live,
                first,
                last,
                fill,
                time,
                lane,
            } => self.markdown_bubble(
                &row.id,
                &row.message_id,
                block,
                live,
                first,
                last,
                fill,
                time,
                lane,
                now,
                &theme,
                &selectable,
                window,
                cx,
            ),
            RowKind::NoAnswer {
                seed,
                name,
                time,
                lane,
            } => {
                let hint = name.as_deref().unwrap_or("Agent");
                let (resolved, _, _, _) = self.agent_identity(&seed, hint, cx);
                let who = if resolved == "Agent" || resolved.trim().is_empty() {
                    "The agent".to_owned()
                } else {
                    resolved
                };
                let block = Block::Paragraph(format!("{who} didn't answer. Ask again."));
                self.markdown_bubble(
                    &row.id,
                    &row.message_id,
                    block,
                    false,
                    true,
                    true,
                    false,
                    time,
                    lane,
                    now,
                    &theme,
                    &selectable,
                    window,
                    cx,
                )
            }
            RowKind::Stamp { label } => div()
                .w_full()
                .flex()
                .justify_center()
                .pt(px(STAMP_PAD_TOP))
                .pb(px(STAMP_PAD_BOTTOM))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                // System / time lines can carry a tracking URL; paint and open
                // it like a chat link.
                .child(styled_text(
                    &format!("{}.s", row.id),
                    &label,
                    theme.text_muted,
                    &theme,
                    None,
                    now,
                    0,
                    &selectable,
                ))
                .into_any_element(),
            RowKind::Reactions {
                reactions,
                mine,
                indent,
            } => {
                // iPhone parity: small capsules under the bubble, mine tinted;
                // click toggles my reaction on that emoji.
                let me = format!("user:{}", self.state.read(cx).me_id());
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let mut chips = div().flex().flex_wrap().gap(px(4.0));
                for (emoji, senders) in reactions {
                    let on = senders.contains(&me);
                    let (state, chat, message_id, e) = (
                        self.state.clone(),
                        chat_id.clone(),
                        row.message_id.clone(),
                        emoji.clone(),
                    );
                    chips = chips.child(
                        div()
                            .id(SharedString::from(format!("{}#{}", row.id, emoji)))
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded_full()
                            .bg(if on {
                                Theme::with_alpha(theme.accent, 0.22)
                            } else {
                                theme.bubble_assistant
                            })
                            .border_1()
                            .border_color(if on {
                                Theme::with_alpha(theme.accent, 0.55)
                            } else {
                                theme.border
                            })
                            .cursor_pointer()
                            .child(div().text_size(px(14.0)).child(emoji.clone()))
                            .when(senders.len() > 1, |el| {
                                el.child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.text_muted)
                                        .child(senders.len().to_string()),
                                )
                            })
                            .on_click(move |_, _, cx| {
                                let (chat, message_id, e) =
                                    (chat.clone(), message_id.clone(), e.clone());
                                state
                                    .update(cx, |s, cx| s.toggle_reaction(chat, message_id, e, cx));
                            }),
                    );
                }
                div()
                    .w_full()
                    .flex()
                    .when(mine, |el| el.justify_end())
                    .when(!mine, |el| el.justify_start())
                    .when(indent, |el| el.pl(px(2.0)))
                    .mt(px(-2.0))
                    .pb(px(ROW_GAP_HALF))
                    .child(chips)
                    .into_any_element()
            }
            RowKind::Handoff { names, total } => {
                // "Messaged ◉ A" while only the brief went out; "N messages with
                // ◉ A, ◉ B and ◉ C" once replies came back (Grok refs 13/58).
                let state = self.state.read(cx);
                let lead = if total <= names.len() as u32 {
                    "Messaged".to_string()
                } else {
                    format!("{total} messages with")
                };
                let mut line = div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(lead);
                let n = names.len();
                for (i, name) in names.iter().enumerate() {
                    let glyph: AnyElement = match state
                        .projects
                        .iter()
                        .find(|p| p.name.eq_ignore_ascii_case(name))
                    {
                        Some(space) => {
                            crate::ui::avatars::project(space, 16.0, &theme).into_any_element()
                        }
                        // Not one of my projects (a subagent): initials on the name's gradient.
                        None => crate::ui::avatars::element(name, name, None, 16.0, &theme)
                            .into_any_element(),
                    };
                    line = line.child(glyph).child(div().child(name.clone()));
                    if n >= 2 && i + 2 == n {
                        line = line.child(div().child("and"));
                    } else if i + 2 < n {
                        line = line.child(div().ml(px(-5.0)).child(","));
                    }
                }
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .pt(px(10.0))
                    .pb(px(6.0))
                    .child(line)
                    .into_any_element()
            }
            RowKind::Attachment {
                attachment,
                mine,
                lane,
            } => {
                let preview = match attachment.blob_ref.as_ref() {
                    None => attachments::Preview::Unavailable,
                    Some(k) => {
                        let state = self.state.read(cx);
                        match state.thumbnails.get(k) {
                            Some(image) => attachments::Preview::Ready(image.clone()),
                            None if state.thumbnails_failed.contains(k) => {
                                attachments::Preview::Unavailable
                            }
                            None => attachments::Preview::Loading,
                        }
                    }
                };
                div()
                    .w_full()
                    .py(px(4.0))
                    .flex()
                    .when(mine, |el| el.justify_end())
                    .when(lane, |el| el.pl(px(SENDER_LANE)))
                    .child(attachments::card(
                        &row.id,
                        &attachment,
                        preview,
                        self.state.clone(),
                        &theme,
                    ))
                    .into_any_element()
            }
            RowKind::AskUser { text } => {
                cards::status_row(StatusKind::AskUser, &text, false, &theme, cx).into_any_element()
            }
            RowKind::Question { question_id } => {
                let Some(chat_id) = self.chat_id.clone() else {
                    return div().into_any_element();
                };
                let widget = self
                    .state
                    .read(cx)
                    .messages_of(&chat_id)
                    .iter()
                    .find_map(|m| {
                        m.parts.iter().find_map(|p| match p {
                            Part::Question {
                                question_id: qid,
                                widget: Some(w),
                                ..
                            } if *qid == question_id => Some(w.clone()),
                            _ => None,
                        })
                    });
                let Some(widget) = widget else {
                    return div().into_any_element();
                };
                let state = self.state.clone();
                let card = self
                    .cards
                    .entry(question_id.clone())
                    .or_insert_with(|| {
                        cx.new(|cx| QuestionCard::new(state, chat_id, question_id, &widget, cx))
                    })
                    .clone();
                div()
                    .w_full()
                    .flex()
                    .justify_start()
                    .child(card)
                    .into_any_element()
            }
            RowKind::Approval { view } => {
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let approval_id = view.approval_id.clone();
                let (state_a, chat_a, id_a) =
                    (self.state.clone(), chat_id.clone(), approval_id.clone());
                let (state_r, chat_r, id_r) =
                    (self.state.clone(), chat_id.clone(), approval_id.clone());
                let (state_s, chat_s, id_s) = (self.state.clone(), chat_id, approval_id);
                approval_card(
                    &row.id,
                    &view,
                    &theme,
                    move |cx| {
                        state_a.update(cx, |s, cx| s.approve(chat_a.clone(), id_a.clone(), cx));
                    },
                    move |cx| {
                        state_r
                            .update(cx, |s, cx| s.reject(chat_r.clone(), id_r.clone(), None, cx));
                    },
                    move |choice, cx| {
                        state_s.update(cx, |s, cx| {
                            s.answer_approval(chat_s.clone(), id_s.clone(), choice, cx)
                        });
                    },
                )
                .into_any_element()
            }
            RowKind::Error { code, message } => {
                let state = self.state.clone();
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let message_id = row.message_id.clone();
                cards::error_chip(&row.id, &code, &message, &theme, move |cx| {
                    state.update(cx, |s, cx| s.retry(chat_id.clone(), message_id.clone(), cx));
                })
                .into_any_element()
            }
            RowKind::Glyph { glyph } => div()
                .w_full()
                .py(px(2.0))
                .text_size(px(10.0))
                .text_color(theme.text_muted)
                .child(glyph)
                .into_any_element(),
            RowKind::Indicator { queued, activity } => {
                PulseClock::request(cx);
                // Freshness is time-capped (stale streams / open hand-offs). Re-check
                // on each pulse so a leftover `streaming` row cannot pin forever;
                // when it expires, rebuild rows without the indicator.
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let still = {
                    let state = self.state.read(cx);
                    Message::chat_is_actively_working(state.messages_of(&chat_id), now_ms())
                };
                if !still {
                    cx.spawn(async move |this, cx| {
                        let _ = this.update(cx, |this, cx| this.sync(cx));
                    })
                    .detach();
                    return div().into_any_element();
                }
                // A new activity fades in over 260 ms.
                let key = activity.clone().unwrap_or_default();
                let since = match &self.activity_shown {
                    Some((shown, at)) if *shown == key => *at,
                    _ => {
                        self.activity_shown = Some((key, now));
                        now
                    }
                };
                let fade = (since.elapsed().as_millis() as f32 / 260.0).clamp(0.0, 1.0);
                // "<name> is working" with the worker's picture (Grok's one line).
                let state = self.state.read(cx);
                let (name, picture): (String, Option<gpui::Div>) = match state.selected() {
                    Some(space) if space.is_people() => {
                        let msgs = state.messages_of(&chat_id);
                        let recent = now_ms().saturating_sub(Message::STREAM_FRESH_MS);
                        let agent = msgs
                            .iter()
                            .rev()
                            .find(|m| {
                                m.role == Role::Assistant
                                    && (m.is_streaming() && m.created_at >= recent
                                        || Message::chat_has_open_subagent(
                                            std::slice::from_ref(*m),
                                            now_ms(),
                                        ))
                            })
                            .and_then(|m| m.sender_handle.clone().or_else(|| m.sender_name.clone()))
                            .unwrap_or_else(|| "Agent".into());
                        let glyph = space
                            .agents
                            .iter()
                            .find(|a| a.handle == agent || a.name == agent)
                            .map(|a| people::agent_glyph(a, 20.0, &theme));
                        (agent, glyph)
                    }
                    Some(space) => (
                        space.name.clone(),
                        Some(crate::ui::avatars::project(space, 20.0, &theme)),
                    ),
                    None => ("Agent".into(), None),
                };
                cards::working_line(&name, picture, queued, activity.as_deref(), &theme, cx)
                    .opacity(fade)
                    .into_any_element()
            }
            RowKind::Sender {
                name,
                icon,
                seed,
                avatar,
                avatar_color,
            } => {
                let _ = (&avatar, &avatar_color, &icon);
                let (name, _disc_seed, _color_index, agent_pid) =
                    self.agent_identity(&seed, &name, cx);
                // The caption starts where the bubble's text does; the disc sits
                // at the bubble's bottom-left (last Markdown row, mocks a/f).
                let open_state = self.state.clone();
                let caption = div()
                    .id(SharedString::from(format!("{}#chip", row.id)))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .pl(px(SENDER_LANE + BUBBLE_PAD_X))
                    // Same rhythm as a person's caption: the row gap above, 6 pt
                    // to the bubble below (mock a).
                    .pt(px(ROW_GAP_HALF))
                    .pb(px(6.0 - ROW_GAP_HALF))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(name),
                    )
                    .when(agent_pid.is_some(), |el| {
                        el.cursor_pointer().on_click(move |_, _, cx| {
                            if let Some(pid) = agent_pid.clone() {
                                open_state.update(cx, |s, cx| {
                                    s.push_profile(
                                        crate::app::ProfilePage::Agent { project_id: pid },
                                        cx,
                                    )
                                });
                            }
                        })
                    });
                caption.into_any_element()
            }
            RowKind::Details => {
                let state = self.state.clone();
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let message_id = row.message_id.clone();
                div()
                    .id(SharedString::from(row.id.clone()))
                    .w_full()
                    .pb(px(4.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text_muted))
                    .child("Show details ▸")
                    .on_click(move |_, _, cx| {
                        state.update(cx, |s, cx| {
                            s.open_details(
                                DetailsTarget::Message {
                                    chat_id: chat_id.clone(),
                                    message_id: message_id.clone(),
                                },
                                cx,
                            )
                        });
                    })
                    .into_any_element()
            }
            RowKind::Speaker { cached, failed } => {
                let state = self.state.clone();
                let message_id = row.message_id.clone();
                // The failure lives under the reply it belongs to, as a retry.
                let label = speaker_label(cached, failed);
                div()
                    .id(SharedString::from(row.id.clone()))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .w(px(if failed { 220.0 } else { 96.0 }))
                    .pb(px(ROW_GAP_HALF))
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text_muted))
                    .child(div().text_size(px(12.0)).child("▶"))
                    .child(label)
                    .on_click(move |_, _, cx| {
                        state.update(cx, |s, cx| s.replay_speech(message_id.clone(), cx));
                    })
                    .into_any_element()
            }
            RowKind::BottomInset => {
                // No chrome — just the clearance height. Return early so the
                // shared column padding / highlight wrapper does not paint a
                // strip over scrolling bubbles.
                return div().w_full().h(px(BOTTOM_INSET)).into_any_element();
            }
        };
        let highlighted =
            self.state.read(cx).highlight_message.as_deref() == Some(row.message_id.as_str());
        // The first row of a message from a new sender opens the turn gap; a
        // stamp between them already carries its own spacing.
        let turn_start = ix
            .checked_sub(1)
            .and_then(|i| self.rows.get(i))
            .is_some_and(|prev| {
                prev.message_id != row.message_id
                    && !matches!(prev.kind, RowKind::Stamp { .. } | RowKind::BottomInset)
                    && self.turn_starts.contains(&row.message_id)
            });
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(COLUMN_MARGIN))
            .when(turn_start, |el| el.pt(px(TURN_GAP_EXTRA)))
            .when(highlighted, |el| {
                el.bg(Theme::with_alpha(theme.accent, 0.14))
            })
            .child(div().w_full().max_w(px(COLUMN_MAX_WIDTH)).child(inner))
            .into_any_element()
    }
}

fn relative_width(fraction: f32) -> gpui::DefiniteLength {
    gpui::relative(fraction)
}

impl Render for Transcript {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.step_spring(window, cx);
        let theme = Theme::of(cx).clone();
        // Grok Mac (ref 58): a 40 pt ↓ circle centred above the composer once
        // the user has scrolled up; clicking re-pins and the spring glides down.
        let max = f32::from(self.list.max_offset_for_scrollbar().y);
        let cur = -f32::from(self.list.scroll_px_offset_for_scrollbar().y);
        let show_jump = !self.pinned_to_bottom && max - cur > JUMP_BUTTON_AFTER_PX;
        // A selection lives as long as the transcript keeps keyboard focus:
        // clicking into the composer (or anywhere else) drops the highlight,
        // so a highlight always means ⌘C will copy it.
        let focus = self.selection.read(cx).focus_handle.clone();
        if !focus.is_focused(window) {
            self.selection.update(cx, |s, cx| {
                if s.clear_settled() {
                    cx.notify();
                }
            });
        }
        div()
            .relative()
            .size_full()
            .key_context("Transcript")
            .track_focus(&focus)
            .on_action(cx.listener(Self::copy_selection))
            .on_scroll_wheel(cx.listener(|this, event, _, cx| this.on_wheel(event, cx)))
            // Grok's header band (Jacob, Oct 22 12:49 CR, superseding the opaque
            // band): the list runs up under the floating pill and fades out
            // over `HEADER_FADE_BAND` — Grok's text is gone at the window edge
            // and fully readable ~50 pt down ([feedback mac-header-fade-grok]).
            .child(edge_faded(
                HEADER_FADE_BAND,
                true,
                false,
                list(self.list.clone(), cx.processor(Self::render_row)).size_full(),
            ))
            .when(show_jump, |el| {
                el.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(12.0))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .id("jump-to-latest")
                                .size(px(40.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(theme.card())
                                .border_1()
                                .border_color(theme.border_strong)
                                .shadow_md()
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.surface_2))
                                .child(icon("arrow-down", 15.0, theme.text))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pinned_to_bottom = true;
                                    this.repin_check_pending = false;
                                    cx.notify();
                                }))
                                .with_animation(
                                    "jump-in",
                                    Animation::new(Duration::from_millis(180)),
                                    |el, t| el.opacity(t),
                                ),
                        ),
                )
            })
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// Who an agent lane belongs to: display name, disc seed, D79 colour, D92
/// leaf pair in effect, and the project id when the lane names one.
struct AgentIdentity {
    name: String,
    seed: String,
    color_index: Option<u8>,
    leaf: (Option<u8>, Option<u8>),
    project_id: Option<Id>,
}

fn hash_version<T: Hash>(value: &T, live: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    (h.finish() << 1) | (live as u64)
}

/// Text of the speaker row under a reply (F11). A failure is said under the
/// reply it belongs to and reads as a retry; it never becomes a global line.
pub fn speaker_label(cached: bool, failed: bool) -> &'static str {
    if failed {
        "Could not play — try again"
    } else if cached {
        "Replay"
    } else {
        "Speak"
    }
}

fn block_text_len(block: &Block) -> usize {
    match block {
        Block::Paragraph(t) | Block::Heading { text: t, .. } => t.len(),
        Block::Bullets(items) | Block::Numbered(items) => items.iter().map(|s| s.len() + 1).sum(),
        Block::Code { code, .. } => code.len(),
        Block::Table { header, rows } => {
            header.iter().map(|s| s.len() + 1).sum::<usize>()
                + rows.iter().flatten().map(|s| s.len() + 1).sum::<usize>()
        }
    }
}

/// "Routine · Morning stand-up" for a prompt a routine injected; `None` for the
/// user's own messages (`sender` empty or `user`).
fn sender_caption(message: &Message) -> Option<String> {
    let sender = message.sender.as_str();
    if sender.is_empty() || sender == "user" || sender.starts_with("user:") {
        return None;
    }
    if let Some(id) = sender.strip_prefix("routine:") {
        let name = message.routine_name.as_deref().unwrap_or(id);
        return Some(format!("Routine · {name}"));
    }
    Some(sender.replace(':', " · "))
}

/// The bubble's own clock ("9:41 PM"), the iPhone's drag-to-reveal time.
/// UTType-style labels some clients leave as the only text on a media-only send.
fn is_generic_media_label(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "image" | "photo" | "video" | "attachment" | "(attachment)"
    )
}

fn clock_label(ms: u64) -> Option<String> {
    use chrono::{Local, TimeZone};
    if ms == 0 {
        return None;
    }
    Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|t| t.format("%-I:%M %p").to_string())
}

/// The 11 pt muted time beside a bubble, at the row's current hover opacity.
fn hover_time(time: Option<String>, opacity: f32, theme: &Theme) -> Option<Div> {
    time.map(|label| {
        div()
            .flex_shrink_0()
            .text_size(px(11.0))
            .text_color(theme.text_muted)
            .opacity(opacity)
            .child(label)
    })
}

/// A text block that is really machine output (a JSON object/array) is not
/// prose; it belongs in the Details drawer, never in the transcript.
fn is_json_block(block: &Block) -> bool {
    let text = match block {
        Block::Paragraph(t) => t.as_str(),
        Block::Code { lang, code } if lang.is_empty() || lang.eq_ignore_ascii_case("json") => {
            code.as_str()
        }
        Block::Code { .. }
        | Block::Heading { .. }
        | Block::Bullets(_)
        | Block::Numbered(_)
        | Block::Table { .. } => return false,
    };
    let t = text.trim();
    (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(t).is_ok()
}

/// The transcript shows only what the agent said and made: prose, files,
/// questions, errors. Everything the agent did (tool calls, status events,
/// JSON, usage) is hidden; the Details drawer has it. One indicator per chat
/// pulses under the last message while anything runs.
///
/// `thumb_keys` are blob refs that already have a decoded preview; they are
/// mixed into row versions so a late thumbnail reloads the picture row.
pub fn build_rows(
    messages: &[Message],
    show_details: bool,
    speech: SpeechRows<'_>,
    me_id: &str,
    thumb_keys: &HashSet<&str>,
) -> Vec<Row> {
    let mine = |m: &Message| {
        m.sender == "user" || m.sender.is_empty() || m.sender == format!("user:{me_id}")
    };
    let mut rows = Vec::new();
    let now = now_ms();
    // One indicator while a recent stream or open hand-off is live; stale
    // `streaming` rows (dead box / leftover under a later user line) do not count.
    let running = Message::chat_is_actively_working(messages, now);
    let mut queued = false;
    let last_reply = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant && !m.is_streaming());
    let mut last_stamp_at: u64 = 0;
    // Who spoke last in a people chat: a name sits above a bubble only when the sender
    // changed or a date stamp broke the run (Grok Bot's group style).
    let mut prev_sender: Option<String> = None;
    for (mix, message) in messages.iter().enumerate() {
        let streaming = message.is_streaming();
        if message.created_at > 0 && message.created_at.saturating_sub(last_stamp_at) > STAMP_GAP_MS
        {
            last_stamp_at = message.created_at;
            prev_sender = None;
            let label = crate::ui::sidebar::stamp_label(message.created_at);
            rows.push(Row {
                version: hash_version(&label, false),
                id: format!("{}#stamp", message.id),
                message_id: message.id.clone(),
                kind: RowKind::Stamp { label },
            });
        }
        let rows_before_message = rows.len();
        match message.role {
            Role::User => {
                let text = message.plain_text();
                let attachment_parts: Vec<(String, Attachment)> = message
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Attachment { id, attachment } => {
                            Some((id.clone(), attachment.clone()))
                        }
                        _ => None,
                    })
                    .collect();
                let has_image = attachment_parts
                    .iter()
                    .any(|(_, a)| a.kind() == AttachmentKind::Image);
                let from = if message.sender.starts_with("user:") && !mine(message) {
                    Some(Person {
                        id: message.sender.trim_start_matches("user:").to_string(),
                        handle: message
                            .sender_handle
                            .clone()
                            .unwrap_or_else(|| "someone".into()),
                        name: message.sender_name.clone(),
                        avatar_url: message.sender_avatar.clone().unwrap_or_default(),
                        status: "member".into(),
                        role: String::new(),
                        last_seen_at: None,
                        online: false,
                        avatar_color_index: None,
                    })
                } else {
                    None
                };
                let sender_label = if from.is_some() {
                    None
                } else {
                    sender_caption(message)
                };
                let named = prev_sender.as_deref() != Some(message.sender.as_str());
                // Image preview / file card rows (same cards as agent attachments).
                let is_mine = from.is_none();
                for (part_id, attachment) in &attachment_parts {
                    let has_thumb = attachment
                        .blob_ref
                        .as_deref()
                        .is_some_and(|k| thumb_keys.contains(k));
                    rows.push(Row {
                        version: hash_version(
                            &(
                                &attachment.name,
                                &attachment.title,
                                attachment.bytes,
                                &attachment.blob_ref,
                                is_mine,
                                has_thumb,
                            ),
                            false,
                        ),
                        id: format!("{}#{}", message.id, part_id),
                        message_id: message.id.clone(),
                        kind: RowKind::Attachment {
                            attachment: attachment.clone(),
                            mine: is_mine,
                            lane: !is_mine,
                        },
                    });
                }
                // Skip a bare "Image"/"Photo" bubble when the attachment already shows it.
                let show_text = !text.is_empty() && !(has_image && is_generic_media_label(&text));
                if show_text || attachment_parts.is_empty() {
                    let quote = message
                        .reply_to
                        .as_deref()
                        .filter(|id| !id.is_empty())
                        .map(|id| Quote::for_reply(id, messages));
                    rows.push(Row {
                        version: hash_version(
                            &(
                                &text,
                                message.pending,
                                &sender_label,
                                from.as_ref().map(|p| &p.handle),
                                &quote,
                            ),
                            false,
                        ),
                        id: format!("{}#user", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::User {
                            text,
                            pending: message.pending,
                            sender_label,
                            from,
                            named,
                            time: clock_label(message.created_at),
                            quote,
                        },
                    });
                }
                if message.status == MessageStatus::Queued {
                    queued = true;
                }
                push_reactions(&mut rows, message, mine(message));
            }
            Role::System => {
                // Call lines and other system notes: one centred muted line.
                let text = message.plain_text();
                let line = text.lines().next().unwrap_or_default().trim().to_string();
                if !line.is_empty() {
                    rows.push(Row {
                        version: hash_version(&line, false),
                        id: format!("{}#system", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Stamp { label: line },
                    });
                }
            }
            Role::Assistant | Role::Project => {
                if matches!(
                    message.turn,
                    Some(TurnKind::New | TurnKind::Spawn | TurnKind::Wake)
                ) {
                    // The box writes `subagentName` on the row; older fixtures only
                    // name it on the spawn tool call.
                    let named = message.subagent_name.clone().or_else(|| {
                        message.parts.iter().find_map(|p| match p {
                            Part::Tool {
                                subagent_name: Some(n),
                                ..
                            } => Some(n.clone()),
                            _ => None,
                        })
                    });
                    if let Some(name) = named {
                        let count = message.handoff_messages.unwrap_or(1).max(1);
                        // Consecutive hand-offs merge into one line.
                        if let Some(Row {
                            kind: RowKind::Handoff { names, total },
                            version,
                            ..
                        }) = rows.last_mut()
                        {
                            if !names.contains(&name) {
                                names.push(name);
                            }
                            *total += count;
                            *version = hash_version(&(&*names, *total), false);
                        } else {
                            rows.push(Row {
                                version: hash_version(&(&name, count), false),
                                id: format!("{}#handoff", message.id),
                                message_id: message.id.clone(),
                                kind: RowKind::Handoff {
                                    names: vec![name],
                                    total: count,
                                },
                            });
                        }
                    }
                }
                let mut lane: Option<String> = None;
                let in_people =
                    message.sender.starts_with("agent:") && message.asked_by_handle.is_some();
                if message.role == Role::Project || in_people {
                    // A people-chat agent (or another Project, F6) speaks in its own lane. The
                    // name sits above the bubble only when the sender changed (Grok Bot's group
                    // style); the disc on the last block names it otherwise. No meta line —
                    // "answering @x · owned by @y" moved to the profile pane (Jacob, 2026-10-22).
                    lane = Some(message.sender.clone());
                    if prev_sender.as_deref() != Some(message.sender.as_str()) {
                        let fallback = if message.role == Role::Project {
                            message
                                .sender
                                .strip_prefix("project:")
                                .map(str::to_owned)
                                .unwrap_or_else(|| "Project".to_owned())
                        } else {
                            "Agent".to_owned()
                        };
                        let name = message.sender_name.clone().unwrap_or(fallback);
                        rows.push(Row {
                            version: hash_version(
                                &(&name, &message.sender_icon, &message.sender_avatar_color),
                                false,
                            ),
                            id: format!("{}#sender", message.id),
                            message_id: message.id.clone(),
                            kind: RowKind::Sender {
                                name,
                                icon: message.sender_icon.clone(),
                                seed: message.sender.clone(),
                                avatar: message
                                    .sender_avatar
                                    .clone()
                                    .filter(|a| !a.is_empty() && !a.contains("://")),
                                avatar_color: message
                                    .sender_avatar_color
                                    .clone()
                                    .filter(|a| !a.is_empty()),
                            },
                        });
                    }
                }
                let last_text_part = message
                    .parts
                    .iter()
                    .rposition(|p| matches!(p, Part::Text { .. }));
                // Blocks across every text part form one bubble.
                // Polish assistant prose on display so glued ".Word" and a trailing
                // tool-narration sentence (already in the doc) still read as chat.
                let polished: Vec<(String, String)> = message
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { id, text } => Some((id.clone(), polish_assistant_prose(text))),
                        _ => None,
                    })
                    .collect();
                let total_blocks: usize = polished
                    .iter()
                    .map(|(_, text)| {
                        split_blocks(text)
                            .into_iter()
                            .filter(|b| !is_json_block(b))
                            .count()
                    })
                    .sum();
                let fill = total_blocks > 1;
                let mut block_ix = 0usize;
                let mut had_prose = false;
                // Error parts wait until the prose is known: under a real answer they stay
                // the chip with Retry; on an answer with no words they become the words.
                let mut errors: Vec<(String, String, String)> = Vec::new();
                let rows_before_parts = rows.len();
                for (pix, part) in message.parts.iter().enumerate() {
                    match part {
                        Part::Text { id, .. } => {
                            let text = polished
                                .iter()
                                .find(|(pid, _)| pid == id)
                                .map(|(_, t)| t.as_str())
                                .unwrap_or("");
                            if text.trim().is_empty() {
                                continue;
                            }
                            let blocks: Vec<Block> = split_blocks(text)
                                .into_iter()
                                .filter(|b| !is_json_block(b))
                                .collect();
                            let is_last_text = Some(pix) == last_text_part;
                            let n = blocks.len();
                            for (bix, block) in blocks.into_iter().enumerate() {
                                had_prose = true;
                                let live = streaming && is_last_text && bix + 1 == n;
                                let first = block_ix == 0;
                                // While streaming, the newest block is the bottom for now.
                                let last = block_ix + 1 == total_blocks || (live && streaming);
                                rows.push(Row {
                                    version: hash_version(&(&block, first, last, fill), live),
                                    id: format!("{}#{}.{}", message.id, id, bix),
                                    message_id: message.id.clone(),
                                    kind: RowKind::Markdown {
                                        block,
                                        live,
                                        first,
                                        last,
                                        fill,
                                        time: clock_label(message.created_at),
                                        lane: lane.clone(),
                                    },
                                });
                                block_ix += 1;
                            }
                        }
                        Part::Attachment { id, attachment } => {
                            let has_thumb = attachment
                                .blob_ref
                                .as_deref()
                                .is_some_and(|k| thumb_keys.contains(k));
                            rows.push(Row {
                                version: hash_version(
                                    &(
                                        &attachment.name,
                                        &attachment.title,
                                        attachment.bytes,
                                        &attachment.blob_ref,
                                        has_thumb,
                                    ),
                                    false,
                                ),
                                id: format!("{}#{}", message.id, id),
                                message_id: message.id.clone(),
                                kind: RowKind::Attachment {
                                    attachment: attachment.clone(),
                                    mine: false,
                                    lane: lane.is_some(),
                                },
                            })
                        }
                        // Legacy `waiting_user` row: only when no `question` part carries it.
                        Part::Status {
                            id,
                            code: StatusKind::AskUser,
                            message: text,
                        } if !message.has_question_part() => rows.push(Row {
                            version: hash_version(text, false),
                            id: format!("{}#{}", message.id, id),
                            message_id: message.id.clone(),
                            kind: RowKind::AskUser { text: text.clone() },
                        }),
                        Part::Question {
                            id,
                            question_id,
                            question,
                            widget,
                            answer,
                            state,
                            ..
                        } => match widget {
                            Some(_) => rows.push(Row {
                                version: hash_version(
                                    &(question, answer.as_ref().map(Value::to_string), *state),
                                    false,
                                ),
                                id: format!("{}#{}", message.id, id),
                                message_id: message.id.clone(),
                                kind: RowKind::Question {
                                    question_id: question_id.clone(),
                                },
                            }),
                            None => rows.push(Row {
                                version: hash_version(&(question, *state), false),
                                id: format!("{}#{}", message.id, id),
                                message_id: message.id.clone(),
                                kind: RowKind::AskUser {
                                    text: question.clone(),
                                },
                            }),
                        },
                        // S18: the card renders; its `waiting_approval` status row is implied.
                        Part::Approval {
                            id,
                            approval_id,
                            title,
                            summary,
                            risk,
                            payload,
                            state,
                            decided_by,
                            reason,
                            ..
                        } => {
                            let view = ApprovalView {
                                approval_id: approval_id.clone(),
                                title: title.clone(),
                                summary: summary.clone(),
                                risk: *risk,
                                state: *state,
                                payload: payload.clone(),
                                decided_by: decided_by.clone(),
                                reason: reason.clone(),
                            };
                            rows.push(Row {
                                version: hash_version(
                                    &(approval_id, title, state.label(), decided_by, reason),
                                    false,
                                ),
                                id: format!("{}#{}", message.id, id),
                                message_id: message.id.clone(),
                                kind: RowKind::Approval { view },
                            });
                        }
                        Part::Status {
                            code: StatusKind::Queued,
                            ..
                        } => {
                            if streaming {
                                queued = true;
                            }
                        }
                        Part::Error {
                            id,
                            code,
                            message: text,
                        } => errors.push((id.clone(), code.clone(), text.clone())),
                        // Everything the agent did, not said: Details drawer only.
                        Part::Status { .. }
                        | Part::Tool { .. }
                        | Part::SubagentResult { .. }
                        | Part::Reasoning { .. }
                        | Part::Diff { .. }
                        | Part::File { .. }
                        | Part::Browser { .. }
                        | Part::Skills { .. }
                        | Part::Unknown => {}
                    }
                }
                // A finished reply that shows nothing — no prose, no card, no attachment —
                // would leave only the sender header (Lium in the "Testing" group,
                // 2026-10-21: six "answering @…" lines over nothing). Say so in the bubble,
                // plainly, like the limit announcements: the error part's own line when the
                // box wrote one, else a stock line.
                let shows_nothing = rows.len() == rows_before_parts && !had_prose;
                let finished = matches!(message.status, MessageStatus::Done | MessageStatus::Error);
                // A row that only did work (tool calls, a hand-off) may legitimately end
                // without words — its answer came as a progress bubble; only a row that holds
                // nothing but empty text and status gets the stock line.
                let only_empty = message.parts.iter().all(|p| {
                    matches!(
                        p,
                        Part::Text { .. } | Part::Status { .. } | Part::Error { .. }
                    )
                });
                let failed = !errors.is_empty() || matches!(message.status, MessageStatus::Error);
                if shows_nothing && finished && !streaming && (failed || only_empty) {
                    let line = errors
                        .first()
                        .map(|(_, _, text)| text.trim())
                        .filter(|t| !t.is_empty())
                        .map(str::to_owned);
                    let kind = match line {
                        Some(line) => RowKind::Markdown {
                            block: Block::Paragraph(line),
                            live: false,
                            first: true,
                            last: true,
                            fill: false,
                            time: clock_label(message.created_at),
                            lane: lane.clone(),
                        },
                        None => RowKind::NoAnswer {
                            seed: message.sender.clone(),
                            name: message.sender_name.clone().filter(|n| !n.trim().is_empty()),
                            time: clock_label(message.created_at),
                            lane: lane.clone(),
                        },
                    };
                    rows.push(Row {
                        version: hash_version(
                            &(
                                &message.sender,
                                &message.sender_name,
                                errors.first().map(|(_, _, text)| text),
                            ),
                            false,
                        ),
                        id: format!("{}#failed", message.id),
                        message_id: message.id.clone(),
                        kind,
                    });
                } else {
                    for (id, code, text) in &errors {
                        rows.push(Row {
                            version: hash_version(&(code, text), false),
                            id: format!("{}#{}", message.id, id),
                            message_id: message.id.clone(),
                            kind: RowKind::Error {
                                code: code.clone(),
                                message: text.clone(),
                            },
                        });
                    }
                }
                // Stopped turns leave one tiny glyph, no words.
                let glyph = match message.status {
                    MessageStatus::Cancelled => Some("■"),
                    MessageStatus::Error
                    | MessageStatus::Done
                    | MessageStatus::Queued
                    | MessageStatus::Streaming => None,
                };
                if let Some(glyph) = glyph {
                    rows.push(Row {
                        version: 1,
                        id: format!("{}#glyph", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Glyph { glyph },
                    });
                }
                if !streaming {
                    push_reactions(&mut rows, message, false);
                }
                // Speaker (F11): every reply whose speech is cached or failed,
                // plus the newest finished reply while voice mode is on.
                let cached = speech.spoken.contains(&message.id);
                let failed = speech.failed.contains_key(&message.id);
                if speech.enabled
                    && !streaming
                    && message.role == Role::Assistant
                    && had_prose
                    && (cached || failed || (speech.voice_mode && last_reply == Some(mix)))
                {
                    rows.push(Row {
                        version: hash_version(&(cached, failed), false),
                        id: format!("{}#speaker", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Speaker { cached, failed },
                    });
                }
                if show_details && !streaming && message.role == Role::Assistant && had_prose {
                    rows.push(Row {
                        version: 1,
                        id: format!("{}#details", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Details,
                    });
                }
            }
        }
        if rows.len() > rows_before_message {
            prev_sender = Some(message.sender.clone());
        }
    }
    if running || queued {
        let last = messages.last().map(|m| m.id.clone()).unwrap_or_default();
        let activity = messages
            .iter()
            .rev()
            .find(|m| {
                m.is_streaming() && m.created_at >= now.saturating_sub(Message::STREAM_FRESH_MS)
            })
            .and_then(Message::current_activity);
        rows.push(Row {
            version: hash_version(&(queued, &activity), true),
            id: "chat#indicator".into(),
            message_id: last,
            kind: RowKind::Indicator { queued, activity },
        });
    }
    // Trailing clearance: scrolls with the list so mid-thread bubbles can
    // reach the composer, while the pinned end still keeps 54 pt above it.
    if !rows.is_empty() {
        rows.push(Row {
            version: 1,
            id: "chat#bottom-inset".into(),
            message_id: String::new(),
            kind: RowKind::BottomInset,
        });
    }
    rows
}

/// One chips row under a message that carries reactions.
fn push_reactions(rows: &mut Vec<Row>, message: &Message, mine: bool) {
    let reactions = message.reaction_list();
    if reactions.is_empty() {
        return;
    }
    let indent = message.role == Role::User && !mine;
    rows.push(Row {
        version: hash_version(&(&reactions, mine, indent), false),
        id: format!("{}#reactions", message.id),
        message_id: message.id.clone(),
        kind: RowKind::Reactions {
            reactions,
            mine,
            indent,
        },
    });
}

// ---------------------------------------------------------------------------
// Markdown rendering with the veil
// ---------------------------------------------------------------------------

fn sans_font(theme: &Theme) -> gpui::Font {
    gpui::Font {
        family: theme.font_sans.into(),
        features: Default::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: gpui::FontStyle::Normal,
    }
}

/// Selectable user-bubble text: literal (no bold/italic/heading paint) except
/// bare `http(s)://…` URLs, which get accent + underline and open on click.
fn plain_text(
    id: &str,
    text: &str,
    color: gpui::Hsla,
    theme: &Theme,
    selectable: &Selectable,
) -> AnyElement {
    let font = sans_font(theme);
    let mut full = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let mut plain = String::new();

    let flush_plain = |plain: &mut String,
                       full: &mut String,
                       runs: &mut Vec<TextRun>,
                       font: &gpui::Font,
                       color: gpui::Hsla| {
        if plain.is_empty() {
            return;
        }
        runs.push(TextRun {
            len: plain.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        full.push_str(plain);
        plain.clear();
    };

    while i < chars.len() {
        let is_url = chars[i] == 'h'
            && (slice_starts_at(&chars, i, "https://") || slice_starts_at(&chars, i, "http://"));
        if is_url {
            flush_plain(&mut plain, &mut full, &mut runs, &font, color);
            let end = (i..chars.len())
                .find(|&j| chars[j].is_whitespace() || chars[j] == ')' || chars[j] == '"')
                .unwrap_or(chars.len());
            let mut url_end = end;
            while url_end > i && matches!(chars[url_end - 1], '.' | ',' | ';' | ':') {
                url_end -= 1;
            }
            let url: String = chars[i..url_end].iter().collect();
            let start = full.len();
            links.push((start..start + url.len(), url.clone()));
            runs.push(TextRun {
                len: url.len(),
                font: font.clone(),
                color: theme.accent,
                background_color: None,
                underline: Some(gpui::UnderlineStyle {
                    color: Some(Theme::with_alpha(theme.accent, 0.6)),
                    thickness: px(1.0),
                    wavy: false,
                }),
                strikethrough: None,
            });
            full.push_str(&url);
            i = url_end;
            continue;
        }
        plain.push(chars[i]);
        i += 1;
    }
    flush_plain(&mut plain, &mut full, &mut runs, &font, color);

    selectable
        .text(id.to_string(), full, runs)
        .with_links(links)
        .into_any_element()
}

fn slice_starts_at(chars: &[char], at: usize, needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    at + n.len() <= chars.len() && chars[at..at + n.len()] == n[..]
}

/// User / people-chat bubble body: plain prose, or prose mixed with fenced
/// ``` code cards when the message contains triple backticks.
fn user_message_body(
    row_id: &str,
    text: &str,
    color: gpui::Hsla,
    theme: &Theme,
    selectable: &Selectable,
) -> AnyElement {
    if !has_fenced_code(text) {
        return plain_text(&format!("{row_id}.u"), text, color, theme, selectable);
    }
    let blocks = split_blocks(text);
    let mut col = div().flex().flex_col().gap(px(6.0)).w_full().min_w_0();
    for (i, block) in blocks.into_iter().enumerate() {
        let id = format!("{row_id}.u{i}");
        col = match block {
            Block::Code { lang, code } => col.child(code_fence_card(
                &id,
                &lang,
                &code,
                color,
                theme,
                selectable,
                None,
                Instant::now(),
            )),
            Block::Paragraph(t) => col.child(plain_text(&id, &t, color, theme, selectable)),
            Block::Heading { text: t, .. } => {
                col.child(plain_text(&id, &t, color, theme, selectable))
            }
            Block::Bullets(items) => {
                let t = items
                    .iter()
                    .map(|item| format!("• {item}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                col.child(plain_text(&id, &t, color, theme, selectable))
            }
            Block::Numbered(items) => {
                let t = items
                    .iter()
                    .enumerate()
                    .map(|(n, item)| format!("{}. {item}", n + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                col.child(plain_text(&id, &t, color, theme, selectable))
            }
            Block::Table { header, rows } => {
                // Rare in a typed bubble; fall back to a readable plain dump.
                let mut lines = vec![header.join(" | ")];
                for row in rows {
                    lines.push(row.join(" | "));
                }
                col.child(plain_text(&id, &lines.join("\n"), color, theme, selectable))
            }
        };
    }
    col.into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn styled_text(
    id: &str,
    text: &str,
    base_color: gpui::Hsla,
    theme: &Theme,
    veil: Option<&Veil>,
    now: Instant,
    offset: usize,
    selectable: &Selectable,
) -> AnyElement {
    let spans = inline_spans(text);
    let mut full = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let sans = sans_font(theme);
    let mono = gpui::Font {
        family: theme.font_mono.into(),
        ..sans.clone()
    };
    for span in spans {
        let style = span.style;
        let mut font = if style.code {
            mono.clone()
        } else {
            sans.clone()
        };
        if style.bold {
            font.weight = FontWeight::SEMIBOLD;
        }
        if style.italic {
            font.style = gpui::FontStyle::Italic;
        }
        let start = full.len();
        if let Some(href) = span.href.clone() {
            links.push((start..start + span.text.len(), href));
        }
        runs.push(TextRun {
            len: span.text.len(),
            font,
            color: if style.link { theme.accent } else { base_color },
            background_color: if style.code {
                Some(theme.code_bg)
            } else {
                None
            },
            underline: if style.link {
                Some(gpui::UnderlineStyle {
                    color: Some(Theme::with_alpha(theme.accent, 0.6)),
                    thickness: px(1.0),
                    wavy: false,
                })
            } else {
                None
            },
            strikethrough: None,
        });
        full.push_str(&span.text);
    }
    let runs = match veil {
        Some(veil) => apply_veil(runs, &veil.spans(now), offset),
        None => runs,
    };
    selectable
        .text(id.to_string(), full, runs)
        .with_links(links)
        .into_any_element()
}

/// Split runs at chunk boundaries and multiply alpha in (zeron veil.rs).
fn apply_veil(
    runs: Vec<TextRun>,
    spans: &[(std::ops::Range<usize>, f32)],
    offset: usize,
) -> Vec<TextRun> {
    if spans.is_empty() {
        return runs;
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    for run in runs {
        let run_range = pos..pos + run.len;
        let mut cursor = run_range.start;
        // Boundaries inside this run.
        let mut cuts: Vec<usize> = spans
            .iter()
            .flat_map(|(r, _)| [r.start.saturating_sub(offset), r.end.saturating_sub(offset)])
            .filter(|c| *c > run_range.start && *c < run_range.end)
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        cuts.push(run_range.end);
        for cut in cuts {
            if cut <= cursor {
                continue;
            }
            let mid = cursor;
            let alpha = spans
                .iter()
                .find(|(r, _)| {
                    let s = r.start.saturating_sub(offset);
                    let e = r.end.saturating_sub(offset);
                    mid >= s && mid < e
                })
                .map(|(_, a)| *a)
                .unwrap_or(1.0);
            let mut color = run.color;
            color.a *= alpha;
            out.push(TextRun {
                len: cut - cursor,
                color,
                ..run.clone()
            });
            cursor = cut;
        }
        pos = run_range.end;
    }
    out
}

fn render_block(
    row_id: &str,
    block: &Block,
    veil: Option<&Veil>,
    now: Instant,
    theme: &Theme,
    selectable: &Selectable,
) -> AnyElement {
    let base = div()
        .w_full()
        .min_w_0()
        .text_color(theme.text)
        .text_size(px(CHAT_BODY))
        .line_height(px(CHAT_LINE_HEIGHT));
    match block {
        Block::Paragraph(text) => base
            .child(styled_text(
                &format!("{row_id}.p"),
                text,
                theme.text,
                theme,
                veil,
                now,
                0,
                selectable,
            ))
            .into_any_element(),
        Block::Heading { level, text } => {
            let size = match level {
                1 => 20.0,
                2 => 17.0,
                _ => 15.0,
            };
            base.text_size(px(size))
                .font_weight(FontWeight::SEMIBOLD)
                .pt(px(6.0))
                .child(styled_text(
                    &format!("{row_id}.h"),
                    text,
                    theme.text,
                    theme,
                    veil,
                    now,
                    0,
                    selectable,
                ))
                .into_any_element()
        }
        Block::Bullets(items) | Block::Numbered(items) => {
            let numbered = matches!(block, Block::Numbered(_));
            let mut col = base.flex().flex_col().gap(px(3.0));
            let mut offset = 0usize;
            // Two-digit markers ("10.") need a wider gutter than "•".
            let marker_w = if numbered && items.len() >= 10 {
                24.0
            } else {
                16.0
            };
            for (i, item) in items.iter().enumerate() {
                let marker = if numbered {
                    format!("{}.", i + 1)
                } else {
                    "•".to_string()
                };
                col = col.child(
                    div()
                        .flex()
                        .gap(px(8.0))
                        .child(
                            div()
                                .w(px(marker_w))
                                .flex_shrink_0()
                                .text_color(theme.text_muted)
                                .child(marker),
                        )
                        .child(div().flex_1().min_w_0().child(styled_text(
                            &format!("{row_id}.li{i}"),
                            item,
                            theme.text,
                            theme,
                            veil,
                            now,
                            offset,
                            selectable,
                        ))),
                );
                offset += item.len() + 1;
            }
            col.into_any_element()
        }
        Block::Table { header, rows } => {
            let cols = header.len().max(1);
            let cell = |text: &str, head: bool, i: usize| {
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(10.0))
                    .py(px(6.0))
                    .when(i > 0, |el| el.border_l_1().border_color(theme.border))
                    .text_size(px(12.5))
                    .when(head, |el| el.font_weight(FontWeight::SEMIBOLD))
                    .text_color(if head { theme.text } else { theme.text_muted })
                    .child(text.to_string())
            };
            let mut table = div()
                .flex()
                .flex_col()
                .w_full()
                .rounded(px(8.0))
                .border_1()
                .border_color(theme.border)
                .overflow_hidden();
            let mut head_row = div().flex().bg(theme.code_bg);
            for (i, h) in header.iter().enumerate() {
                head_row = head_row.child(cell(h, true, i));
            }
            table = table.child(head_row);
            for (ri, row) in rows.iter().enumerate() {
                let mut r = div()
                    .flex()
                    .border_t_1()
                    .border_color(theme.border)
                    .when(ri % 2 == 1, |el| {
                        el.bg(Theme::with_alpha(theme.code_bg, 0.4))
                    });
                for i in 0..cols {
                    r = r.child(cell(row.get(i).map(String::as_str).unwrap_or(""), false, i));
                }
                table = table.child(r);
            }
            table.into_any_element()
        }
        Block::Code { lang, code } => {
            code_fence_card(row_id, lang, code, theme.text, theme, selectable, veil, now)
        }
    }
}

/// Fenced ``` block: lang label, Copy, mono body on `code_bg` (Grok's code card).
#[allow(clippy::too_many_arguments)]
fn code_fence_card(
    row_id: &str,
    lang: &str,
    code: &str,
    color: gpui::Hsla,
    theme: &Theme,
    selectable: &Selectable,
    veil: Option<&Veil>,
    now: Instant,
) -> AnyElement {
    let mut card = div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(8.0))
        .bg(theme.code_bg)
        .border_1()
        .border_color(theme.border);
    card = card.child(
        div()
            .flex()
            .justify_between()
            .items_center()
            .px(px(10.0))
            .py(px(4.0))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(11.0))
            .text_color(theme.text_faint)
            .child(if lang.is_empty() {
                "code".to_string()
            } else {
                lang.to_string()
            })
            .child({
                // Copies the block (Grok's code header); the label is the hit target.
                let source = code.to_string();
                div()
                    .id(SharedString::from(format!("{row_id}#copy")))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2).text_color(theme.text))
                    .child("Copy")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(source.clone()));
                    })
            }),
    );
    let mono = gpui::Font {
        family: theme.font_mono.into(),
        features: Default::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: gpui::FontStyle::Normal,
    };
    let runs = vec![TextRun {
        len: code.len(),
        font: mono,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    let runs = match veil {
        Some(v) => apply_veil(runs, &v.spans(now), 0),
        None => runs,
    };
    card = card.child(
        div()
            .px(px(10.0))
            .py(px(8.0))
            .text_size(px(12.5))
            .line_height(px(18.0))
            .child(selectable.text(format!("{row_id}.code"), code.to_string(), runs)),
    );
    card.into_any_element()
}
