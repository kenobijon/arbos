//! The chat column body: a bottom-aligned `list()` with one row per markdown
//! block (zeron-study §4.1–§4.4). Rows are diffed by `(id, version)` and
//! spliced; streaming text fades in through the veil.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use gpui::{
    div, list, prelude::*, px, AnyElement, Context, Entity, FontWeight, ListAlignment,
    ListScrollEvent, ListState, Render, SharedString, StyledText, TextRun, Window,
};

use crate::app::{AppEvent, AppState, DetailsTarget};
use crate::model::{Attachment, Id, Message, MessageStatus, Part, Role, StatusKind, TurnKind};
use crate::ui::cards;
use crate::ui::frost::edge_faded;
use crate::ui::markdown::{inline_spans, split_blocks, Block};
use crate::ui::motion::PulseClock;
use crate::ui::theme::{Theme, BUBBLE_RADIUS};
use crate::ui::veil::Veil;

const OVERDRAW_PX: f32 = 320.0;
pub const COLUMN_MAX_WIDTH: f32 = 680.0;
/// 72 ch at 13.5 px (§3.5).
pub const ASSISTANT_MAX_WIDTH: f32 = 540.0;

#[derive(Clone, Debug)]
pub enum RowKind {
    User {
        text: String,
        attachments: Vec<Attachment>,
        pending: bool,
    },
    Markdown {
        block: Block,
        live: bool,
    },
    /// S8: the agent asked a question.
    AskUser {
        text: String,
    },
    /// Minimal "agent is working" line: one pulsing dot plus the short step
    /// label the box emits as status events. Collapses when the turn ends.
    Working {
        label: String,
    },
    Error {
        code: String,
        message: String,
    },
    /// Thin "Stopped by you" / handoff note.
    Note {
        text: String,
    },
    /// Under a finished assistant message: model · decision · tokens · Details.
    Meta {
        text: String,
    },
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

pub struct Transcript {
    state: Entity<AppState>,
    chat_id: Option<Id>,
    list: ListState,
    rows: Vec<Row>,
    veils: HashMap<String, Veil>,
    pinned_to_bottom: bool,
    spring: StickSpring,
    /// Set by the scroll callback; the next frame re-pins when the user
    /// stopped within `STICK_THRESHOLD_PX` of the end.
    repin_check_pending: bool,
}

impl Transcript {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW_PX));
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |event: &ListScrollEvent, _, cx| {
            let _ = weak.update(cx, |this, _| this.handle_scroll(event));
        });
        cx.observe(&state, |this, _, cx| this.sync(cx)).detach();
        cx.subscribe(&state, |this, _, event: &AppEvent, cx| match event {
            AppEvent::TranscriptText { chat_id, .. } | AppEvent::TranscriptChanged { chat_id } => {
                if this.chat_id.as_deref() == Some(chat_id.as_str()) {
                    this.sync(cx);
                }
            }
        })
        .detach();
        Self {
            state,
            chat_id: None,
            list,
            rows: Vec::new(),
            veils: HashMap::new(),
            pinned_to_bottom: true,
            spring: StickSpring::default(),
            repin_check_pending: false,
        }
    }

    /// Runs inside the list's own scroll callback while `ListState` is
    /// mutably borrowed, so it must not touch the list. Record the intent
    /// and let the next frame (`step_spring`) read pixel offsets.
    fn handle_scroll(&mut self, event: &ListScrollEvent) {
        if event.is_scrolled {
            let at_end = event.visible_range.end >= event.count;
            if at_end {
                self.pinned_to_bottom = true;
            } else {
                self.pinned_to_bottom = false;
                self.spring.velocity = 0.0;
                self.repin_check_pending = true;
            }
        }
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
        let messages: Vec<Message> = chat_id
            .as_deref()
            .map(|id| state.messages_of(id).to_vec())
            .unwrap_or_default();
        self.chat_id = chat_id;

        let new_rows = build_rows(&messages);

        // Veils: observe text growth of live markdown rows.
        for row in &new_rows {
            if let RowKind::Markdown { block, live: true } = &row.kind {
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

        if chat_changed {
            self.rows = new_rows;
            self.list.reset(self.rows.len());
            self.pinned_to_bottom = true;
            self.list.scroll_to_end();
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
            return;
        }
        let old_len = self.rows.len();
        self.rows = new_rows;
        self.list.splice(prefix..old_len, self.rows.len() - prefix);
        // While pinned the spring glides to the new end on the next frames.
        cx.notify();
    }

    fn render_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let now = Instant::now();
        let inner: AnyElement = match row.kind {
            RowKind::User {
                text,
                attachments,
                pending,
            } => {
                let mut bubble = div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .max_w(relative_width(0.70))
                    .px(px(14.0))
                    .py(px(9.0))
                    .rounded(px(BUBBLE_RADIUS))
                    .bg(theme.bubble)
                    .text_color(theme.text)
                    .text_size(px(13.5))
                    .line_height(px(20.0));
                if !attachments.is_empty() {
                    let mut chips = div().flex().flex_wrap().gap(px(6.0));
                    for a in attachments {
                        chips = chips.child(
                            div()
                                .px(px(8.0))
                                .py(px(2.0))
                                .rounded(px(6.0))
                                .bg(theme.surface_2)
                                .text_size(px(11.0))
                                .text_color(theme.text_muted)
                                .child(a.name),
                        );
                    }
                    bubble = bubble.child(chips);
                }
                bubble = bubble.child(text);
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .py(px(6.0))
                    .child(bubble.when(pending, |el| el.opacity(0.6)))
                    .into_any_element()
            }
            RowKind::Markdown { block, live } => {
                let veil = if live {
                    self.veils.get(&row.id).cloned()
                } else {
                    None
                };
                let fading = veil.as_ref().map(|v| v.is_active(now)).unwrap_or(false);
                if live || fading {
                    PulseClock::request(cx);
                }
                div()
                    .w_full()
                    .flex()
                    .justify_start()
                    .py(px(4.0))
                    .child(render_block(&row.id, &block, veil.as_ref(), now, &theme))
                    .into_any_element()
            }
            RowKind::AskUser { text } => {
                cards::status_row(StatusKind::AskUser, &text, false, &theme, cx).into_any_element()
            }
            RowKind::Working { label } => {
                PulseClock::request(cx);
                cards::working_row(&label, &theme, cx).into_any_element()
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
            RowKind::Note { text } => div()
                .w_full()
                .py(px(4.0))
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .italic()
                .child(text)
                .into_any_element(),
            RowKind::Meta { text } => {
                let state = self.state.clone();
                let chat_id = self.chat_id.clone().unwrap_or_default();
                let message_id = row.message_id.clone();
                div()
                    .id(SharedString::from(row.id.clone()))
                    .w_full()
                    .pb(px(6.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text_muted))
                    .child(text)
                    .child(div().text_color(theme.accent).child("Details"))
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
        };
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(px(28.0))
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
        div().size_full().child(edge_faded(
            24.0,
            true,
            false,
            list(self.list.clone(), cx.processor(Self::render_row)).size_full(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// "claude-haiku-5.5 · reply · 1.2k in / 180 out · $0.0021 · 3.4 s"
fn meta_line(message: &Message) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(model) = &message.model {
        parts.push(model.rsplit('/').next().unwrap_or(model).to_string());
    }
    if let Some(d) = &message.decision {
        parts.push(d.clone());
    } else if let Some(t) = message.turn {
        parts.push(format!("{t:?}").to_lowercase());
    }
    if let Some(u) = &message.usage {
        let num = |k: &str| u.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let input = num("input") + num("input_tokens") + num("prompt_tokens");
        let output = num("output") + num("output_tokens") + num("completion_tokens");
        if input > 0.0 || output > 0.0 {
            parts.push(format!(
                "{} in / {} out",
                fmt_tokens(input),
                fmt_tokens(output)
            ));
        }
        let cached = num("cacheRead");
        if cached > 0.0 {
            parts.push(format!("{} cached", fmt_tokens(cached)));
        }
        let cost = num("cost");
        if cost > 0.0 {
            parts.push(format!("${cost:.4}"));
        }
    }
    if let (Some(done), created) = (message.completed_at, message.created_at) {
        if done > created && created > 0 {
            parts.push(format!("{:.1} s", (done - created) as f64 / 1000.0));
        }
    }
    parts.join(" · ")
}

fn fmt_tokens(n: f64) -> String {
    if n >= 1000.0 {
        format!("{:.1}k", n / 1000.0)
    } else {
        format!("{}", n as u64)
    }
}

fn hash_version<T: Hash>(value: &T, live: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    (h.finish() << 1) | (live as u64)
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

/// Grok-bot transcript: prose only. Tool calls, raw JSON, status events,
/// return contracts and cards never render here (the Details drawer has
/// them); a streaming turn shows one minimal working line instead.
pub fn build_rows(messages: &[Message]) -> Vec<Row> {
    let mut rows = Vec::new();
    for message in messages {
        let streaming = message.is_streaming();
        match message.role {
            Role::User => {
                let text = message.plain_text();
                let attachments: Vec<Attachment> = message
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Attachment { attachment, .. } => Some(attachment.clone()),
                        _ => None,
                    })
                    .collect();
                let id = format!("{}#user", message.id);
                rows.push(Row {
                    version: hash_version(&(&text, message.pending, attachments.len()), false),
                    id,
                    message_id: message.id.clone(),
                    kind: RowKind::User {
                        text,
                        attachments,
                        pending: message.pending,
                    },
                });
            }
            Role::Assistant | Role::System | Role::Project => {
                let last_text_part = message
                    .parts
                    .iter()
                    .rposition(|p| matches!(p, Part::Text { .. }));
                for (pix, part) in message.parts.iter().enumerate() {
                    match part {
                        Part::Text { id, text } => {
                            let blocks: Vec<Block> = split_blocks(text)
                                .into_iter()
                                .filter(|b| !is_json_block(b))
                                .collect();
                            let is_last_text = Some(pix) == last_text_part;
                            let n = blocks.len();
                            for (bix, block) in blocks.into_iter().enumerate() {
                                let live = streaming && is_last_text && bix + 1 == n;
                                rows.push(Row {
                                    version: hash_version(&block, live),
                                    id: format!("{}#{}.{}", message.id, id, bix),
                                    message_id: message.id.clone(),
                                    kind: RowKind::Markdown { block, live },
                                });
                            }
                        }
                        Part::Status {
                            id,
                            code: StatusKind::AskUser,
                            message: text,
                        } => rows.push(Row {
                            version: hash_version(text, false),
                            id: format!("{}#{}", message.id, id),
                            message_id: message.id.clone(),
                            kind: RowKind::AskUser { text: text.clone() },
                        }),
                        // Thin notes the user should see: stopped, failed, fixture notes.
                        Part::Status {
                            id,
                            code: StatusKind::Note | StatusKind::Stopped | StatusKind::Failed,
                            message: text,
                        } => rows.push(Row {
                            version: hash_version(text, false),
                            id: format!("{}#{}", message.id, id),
                            message_id: message.id.clone(),
                            kind: RowKind::Note { text: text.clone() },
                        }),
                        Part::Error {
                            id,
                            code,
                            message: text,
                        } => rows.push(Row {
                            version: hash_version(&(code, text), false),
                            id: format!("{}#{}", message.id, id),
                            message_id: message.id.clone(),
                            kind: RowKind::Error {
                                code: code.clone(),
                                message: text.clone(),
                            },
                        }),
                        // Everything the agent did, not said: Details drawer only.
                        Part::Status { .. }
                        | Part::Tool { .. }
                        | Part::SubagentResult { .. }
                        | Part::Reasoning { .. }
                        | Part::Diff { .. }
                        | Part::File { .. }
                        | Part::Browser { .. }
                        | Part::Attachment { .. }
                        | Part::Unknown => {}
                    }
                }
                // A finished progress turn (status events only) collapses to
                // nothing; the prose result arrives as its own message.
                let is_progress = message.turn.map(TurnKind::is_progress).unwrap_or(false);
                let shown_any = rows.iter().any(|r| r.message_id == message.id);
                if !streaming && message.role == Role::Assistant && !shown_any && !is_progress {
                    let note = match message.status {
                        MessageStatus::Error => Some("The turn failed before it said anything."),
                        MessageStatus::Cancelled => Some("Stopped before it said anything."),
                        MessageStatus::Done | MessageStatus::Queued | MessageStatus::Streaming => {
                            None
                        }
                    };
                    if let Some(text) = note {
                        rows.push(Row {
                            version: 1,
                            id: format!("{}#empty", message.id),
                            message_id: message.id.clone(),
                            kind: RowKind::Note { text: text.into() },
                        });
                    }
                }
                if message.status == MessageStatus::Error
                    && !message
                        .parts
                        .iter()
                        .any(|p| matches!(p, Part::Error { .. }))
                {
                    rows.push(Row {
                        version: 1,
                        id: format!("{}#failed", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Error {
                            code: "turn_failed".into(),
                            message: "The agent could not finish this turn.".into(),
                        },
                    });
                }
                if streaming {
                    let label = message
                        .working_label()
                        .unwrap_or_else(|| "Working…".to_string());
                    rows.push(Row {
                        version: hash_version(&label, true),
                        id: format!("{}#working", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Working { label },
                    });
                } else if message.role == Role::Assistant && !is_progress {
                    let meta = meta_line(message);
                    if !meta.is_empty() {
                        rows.push(Row {
                            version: hash_version(&meta, false),
                            id: format!("{}#meta", message.id),
                            message_id: message.id.clone(),
                            kind: RowKind::Meta { text: meta },
                        });
                    }
                }
                if message.status == MessageStatus::Cancelled
                    && !message.parts.iter().any(|p| {
                        matches!(
                            p,
                            Part::Status {
                                code: StatusKind::Note,
                                ..
                            }
                        )
                    })
                {
                    rows.push(Row {
                        version: 0,
                        id: format!("{}#cancelled", message.id),
                        message_id: message.id.clone(),
                        kind: RowKind::Note {
                            text: "Stopped by you".into(),
                        },
                    });
                }
            }
        }
    }
    rows
}

// ---------------------------------------------------------------------------
// Markdown rendering with the veil
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn styled_text(
    id: &str,
    text: &str,
    base_color: gpui::Hsla,
    theme: &Theme,
    veil: Option<&Veil>,
    now: Instant,
    offset: usize,
) -> AnyElement {
    let spans = inline_spans(text);
    let mut full = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut links: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let sans = gpui::Font {
        family: theme.font_sans.into(),
        features: Default::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: gpui::FontStyle::Normal,
    };
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
    let styled = StyledText::new(SharedString::from(full)).with_runs(runs);
    if links.is_empty() {
        return styled.into_any_element();
    }
    let ranges: Vec<std::ops::Range<usize>> = links.iter().map(|(r, _)| r.clone()).collect();
    let hrefs: Vec<String> = links.into_iter().map(|(_, h)| h).collect();
    gpui::InteractiveText::new(SharedString::from(id.to_string()), styled)
        .on_click(ranges, move |ix, _, cx| {
            if let Some(href) = hrefs.get(ix) {
                cx.open_url(href);
            }
        })
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
) -> AnyElement {
    let base = div()
        .w_full()
        .min_w_0()
        .max_w(px(ASSISTANT_MAX_WIDTH))
        .text_color(theme.text)
        .text_size(px(13.5))
        .line_height(px(21.0));
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
                ))
                .into_any_element()
        }
        Block::Bullets(items) | Block::Numbered(items) => {
            let numbered = matches!(block, Block::Numbered(_));
            let mut col = base.flex().flex_col().gap(px(3.0));
            let mut offset = 0usize;
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
                                .w(px(16.0))
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
                        lang.clone()
                    })
                    .child(div().child("Copy")),
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
                color: theme.text,
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
                    .child(StyledText::new(SharedString::from(code.clone())).with_runs(runs)),
            );
            card.into_any_element()
        }
    }
}
