//! The one presence row over the composer ([feedback
//! typing-indicator-left-2026-10-23], Jacob 2026-10-23): who is typing (people)
//! or working (agents) in the open chat, as a stack of round profile pictures
//! and ONE line of text — "Sam is typing…", "Lium and Home are working" — that
//! starts flush with the chat column's left edge, like the bubbles and the
//! pill. It replaces two older pieces that could both show at once: the
//! transcript's "<name> is working" indicator row (which, in a people chat, sat
//! under a sender caption with no bubble yet → a doubled notice) and the
//! "@sam is typing…" line over the composer.

use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, App, Div, RenderImage};

use crate::app::AppState;
use crate::model::{Message, MessageStatus, Part, Person, Project, Role, StatusKind};
use crate::ui::avatars;
use crate::ui::identity;
use crate::ui::motion::PulseClock;
use crate::ui::people;
use crate::ui::theme::{Theme, TEXT_SMALL};

/// Round picture size: the avatar column's disc (`transcript::SENDER_DISC` — the
/// composer's `+` disc, which the sender discs beside the bubbles also use), so
/// the row's picture sits under the bubble discs and over the `+` pixel for
/// pixel (Jacob, 2026-10-23 15:15 / 15:20).
pub const AVATAR: f32 = crate::ui::transcript::SENDER_DISC;
/// Where the first picture starts inside the chat column (= the `+` disc's x).
pub const INSET: f32 = crate::ui::transcript::AVATAR_COLUMN_INSET;
/// How far each further picture tucks under the one before it (Grok / Telegram stack).
pub const OVERLAP: f32 = 8.0;
/// Pictures → text: the disc → bubble gap, so the text starts where the bubble body starts.
pub const GAP: f32 = crate::ui::transcript::SENDER_DISC_GAP;
/// Pictures shown before "+N".
pub const MAX_PICTURES: usize = 3;
/// Row height: the picture plus 3 pt of air above and below.
pub const ROW_HEIGHT: f32 = AVATAR + 6.0;
/// Air between the row and the composer pill.
pub const GAP_BELOW: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Typing,
    Working,
}

/// One person or agent on the row, newest first in [`Presence::participants`].
pub struct Participant {
    pub name: String,
    pub activity: Activity,
    pub picture: AnyElement,
}

pub struct Presence {
    pub participants: Vec<Participant>,
    /// The harness's own words ("Cloning RaoFoundation/subtensor") while exactly
    /// one agent works; `None` → "<name> is working".
    pub detail: Option<String>,
    /// The message waits for a sleeping machine (amber copy).
    pub queued: bool,
}

impl Presence {
    /// One line. Alone, a participant keeps the full display name; from two on,
    /// first names ("Mira and Sam are typing…"), so the line stays one line.
    pub fn text(&self) -> String {
        let several = self.participants.len() > 1;
        let name = |p: &Participant| -> String {
            if several {
                first_name(&p.name)
            } else {
                p.name.clone()
            }
        };
        let typing: Vec<String> = self
            .participants
            .iter()
            .filter(|p| p.activity == Activity::Typing)
            .map(name)
            .collect();
        let working: Vec<String> = self
            .participants
            .iter()
            .filter(|p| p.activity == Activity::Working)
            .map(name)
            .collect();
        let typing: Vec<&str> = typing.iter().map(String::as_str).collect();
        let working: Vec<&str> = working.iter().map(String::as_str).collect();
        sentence(&typing, &working, self.detail.as_deref(), self.queued)
    }
}

/// "Sam Okafor" → "Sam"; "@sam" and one-word names stay as they are.
pub fn first_name(name: &str) -> String {
    name.split_whitespace().next().unwrap_or(name).to_string()
}

/// "A", "A and B", "A, B and C", "A, B and 2 others".
fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [a] => a.to_string(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} others", rest.len()),
    }
}

fn verb(n: usize) -> &'static str {
    if n == 1 {
        "is"
    } else {
        "are"
    }
}

/// The row's one line. Typing and working groups each get a clause; both →
/// joined with " · ". Exactly one worker with a `detail` shows the detail;
/// a queued single worker "is waiting for the machine".
pub fn sentence(typing: &[&str], working: &[&str], detail: Option<&str>, queued: bool) -> String {
    let mut clauses: Vec<String> = Vec::new();
    if !typing.is_empty() {
        clauses.push(format!(
            "{} {} typing…",
            join_names(typing),
            verb(typing.len())
        ));
    }
    if !working.is_empty() {
        let clause = match (working, queued, detail) {
            ([one], true, _) => format!("{one} is waiting for the machine"),
            ([_], false, Some(d)) if !d.trim().is_empty() => d.trim().to_string(),
            _ => format!("{} {} working", join_names(working), verb(working.len())),
        };
        clauses.push(clause);
    }
    clauses.join(" · ")
}

/// Everyone typing or working in `chat_id`, newest first; `None` when the row
/// has nothing to say. Pictures are the same ones the header pill and the
/// rows use for that person / agent (D79 disc, D92 leaf, or the photo).
pub fn gather(state: &AppState, chat_id: &str, space: &Project, theme: &Theme) -> Option<Presence> {
    let now = crate::model::now_ms();
    let mut participants: Vec<Participant> = Vec::new();

    // People typing (room presence, people chats only).
    for handle in state.typing_in(chat_id) {
        let person = space.people.iter().find(|p| p.handle == handle).cloned();
        let (name, picture) = match person {
            Some(p) => {
                let image: Option<Arc<RenderImage>> = state.avatar(&p.avatar_url);
                (
                    p.display_name(),
                    people::avatar(image, &p, AVATAR, theme).into_any_element(),
                )
            }
            None => {
                let p = Person {
                    handle: handle.clone(),
                    ..Default::default()
                };
                (
                    format!("@{handle}"),
                    avatars::person(&p, AVATAR, theme).into_any_element(),
                )
            }
        };
        participants.push(Participant {
            name,
            activity: Activity::Typing,
            picture,
        });
    }

    // Agents working: a fresh stream or an open hand-off, per sender.
    let messages = state.messages_of(chat_id);
    let running = Message::chat_is_actively_working(messages, now);
    let queued = messages.iter().any(|m| {
        (m.role == Role::User && m.status == MessageStatus::Queued)
            || (m.is_streaming()
                && m.parts.iter().any(|p| {
                    matches!(
                        p,
                        Part::Status {
                            code: StatusKind::Queued,
                            ..
                        }
                    )
                }))
    });
    let mut detail: Option<String> = None;
    if running || queued {
        let recent = now.saturating_sub(Message::STREAM_FRESH_MS);
        let live = |m: &Message| {
            m.role == Role::Assistant
                && (m.is_streaming() && m.created_at >= recent
                    || Message::chat_has_open_subagent(std::slice::from_ref(m), now))
        };
        detail = messages
            .iter()
            .rev()
            .find(|m| m.is_streaming() && m.created_at >= recent)
            .and_then(Message::current_activity);
        if space.is_people() {
            let mut seen: Vec<String> = Vec::new();
            for m in messages.iter().rev().filter(|m| live(m)) {
                let key = m
                    .sender_handle
                    .clone()
                    .or_else(|| m.sender_name.clone())
                    .unwrap_or_else(|| m.sender.clone());
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key.clone());
                let agent = space
                    .agents
                    .iter()
                    .find(|a| a.handle == key || a.name == key || a.project_id == key);
                let (name, picture) = match agent {
                    Some(a) => (
                        a.name.clone(),
                        avatars::agent_member(a, AVATAR, theme).into_any_element(),
                    ),
                    None => {
                        let name = m.sender_name.clone().unwrap_or_else(|| key.clone());
                        (
                            name.clone(),
                            avatars::agent(&m.sender, &name, None, None, None, AVATAR, theme)
                                .into_any_element(),
                        )
                    }
                };
                participants.push(Participant {
                    name,
                    activity: Activity::Working,
                    picture,
                });
            }
        } else {
            participants.push(Participant {
                name: space.name.clone(),
                activity: Activity::Working,
                picture: avatars::project(space, AVATAR, theme).into_any_element(),
            });
        }
    }

    if participants.is_empty() {
        return None;
    }
    Some(Presence {
        participants,
        detail,
        queued,
    })
}

/// The row itself: the picture stack (identity slots, newest in front, cap +
/// "+N"), then one muted line that breathes with the pulse clock. Left-bound
/// inside whatever frames it — the shell puts it through
/// `transcript::column_row`, so its left edge is the composer's.
pub fn row(presence: Presence, theme: &Theme, cx: &App) -> Div {
    let phase = PulseClock::phase(cx, 1100);
    let text = presence.text();
    let total = presence.participants.len();
    // Taffy reports a zero content width for a row whose children carry
    // negative margins, so the stack states its width: one picture plus
    // (AVATAR − OVERLAP) per further disc (the "+N" disc counts as one).
    let discs = total.min(MAX_PICTURES) + usize::from(total > MAX_PICTURES);
    let stack_w = AVATAR + (discs.saturating_sub(1) as f32) * (AVATAR - OVERLAP);
    let mut stack = div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .w(px(stack_w))
        .h(px(AVATAR))
        .debug_selector(|| "presence-pictures".into());
    for (i, p) in presence
        .participants
        .into_iter()
        .take(MAX_PICTURES)
        .enumerate()
    {
        // The slot is the bubble disc's exact box (no border inside it — a rim
        // would shave a point off the picture). With several pictures a ring
        // in the page colour sits just *outside* each one so the overlaps read
        // (newest first on the left, each next one tucked 8 pt in).
        let slot = identity::slot(AVATAR, p.picture)
            .relative()
            .rounded_full()
            .when(total > 1, |el| {
                el.child(
                    div()
                        .absolute()
                        .inset(px(-1.0))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.bg),
                )
            })
            .debug_selector(move || format!("presence-picture-{i}"));
        stack = stack.child(if i == 0 { slot } else { slot.ml(px(-OVERLAP)) });
    }
    if total > MAX_PICTURES {
        stack = stack.child(
            div()
                .size(px(AVATAR))
                .ml(px(-OVERLAP))
                .flex_shrink_0()
                .rounded_full()
                .bg(theme.field)
                .border_1()
                .border_color(theme.bg)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(9.0))
                .text_color(theme.text_muted)
                .debug_selector(|| "presence-more".into())
                .child(format!("+{}", total - MAX_PICTURES)),
        );
    }
    let color = if presence.queued {
        theme.warn
    } else {
        theme.text_muted
    };
    div()
        .w_full()
        .h(px(ROW_HEIGHT))
        .mb(px(GAP_BELOW))
        .pl(px(INSET))
        .flex()
        .items_center()
        .justify_start()
        .gap(px(GAP))
        .debug_selector(|| "presence-row".into())
        .child(stack)
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(TEXT_SMALL))
                .text_color(color)
                .opacity(0.55 + 0.45 * phase)
                .debug_selector(|| "presence-text".into())
                .child(text),
        )
}

#[cfg(test)]
mod text_tests {
    use super::*;

    #[test]
    fn one_line_whatever_the_count() {
        assert_eq!(sentence(&["Sam"], &[], None, false), "Sam is typing…");
        assert_eq!(
            sentence(&["Sam", "Samir"], &[], None, false),
            "Sam and Samir are typing…"
        );
        assert_eq!(
            sentence(&["Sam", "Samir", "Kai", "Ava"], &[], None, false),
            "Sam, Samir and 2 others are typing…"
        );
        assert_eq!(sentence(&[], &["Lium"], None, false), "Lium is working");
        assert_eq!(
            sentence(&[], &["Lium", "Home"], None, false),
            "Lium and Home are working"
        );
        assert_eq!(
            sentence(&[], &["Lium", "Home", "Doug"], None, false),
            "Lium, Home and Doug are working"
        );
    }

    #[test]
    fn detail_and_queue_apply_to_a_single_worker_only() {
        assert_eq!(
            sentence(
                &[],
                &["Lium"],
                Some("Cloning RaoFoundation/subtensor"),
                false
            ),
            "Cloning RaoFoundation/subtensor"
        );
        assert_eq!(
            sentence(&[], &["Lium", "Home"], Some("Cloning"), false),
            "Lium and Home are working"
        );
        assert_eq!(
            sentence(&[], &["Lium"], None, true),
            "Lium is waiting for the machine"
        );
    }

    #[test]
    fn first_names_from_two_participants_on() {
        let p = |name: &str, activity| Participant {
            name: name.to_string(),
            activity,
            picture: gpui::Empty.into_any_element(),
        };
        let one = Presence {
            participants: vec![p("Sam Okafor", Activity::Typing)],
            detail: None,
            queued: false,
        };
        assert_eq!(one.text(), "Sam Okafor is typing…");
        let two = Presence {
            participants: vec![
                p("Mira Kaplan", Activity::Typing),
                p("Sam Okafor", Activity::Typing),
            ],
            detail: None,
            queued: false,
        };
        assert_eq!(two.text(), "Mira and Sam are typing…");
        assert_eq!(first_name("@sam"), "@sam");
    }

    #[test]
    fn typing_and_working_share_the_line() {
        assert_eq!(
            sentence(&["Sam"], &["Lium"], None, false),
            "Sam is typing… · Lium is working"
        );
        assert_eq!(sentence(&[], &[], None, false), "");
    }
}
