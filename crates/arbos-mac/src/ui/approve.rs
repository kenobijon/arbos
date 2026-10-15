//! S18 draft-and-approve card (feature `approve`): title, risk + state chip,
//! a preview of what would happen (the draft's recipient/subject/body or the
//! command), and Approve / Reject while pending. The part shape is owned by
//! `arbos_core::approval`; Approve/Reject write the `approve` / `reject`
//! commands through `AppState`.

use gpui::{div, prelude::*, px, App, Div, FontWeight, Hsla, SharedString, Stateful};

use crate::model::{ApprovalRisk, ApprovalState};
use crate::ui::glass;
use crate::ui::theme::{Appearance, Theme, BUBBLE_RADIUS, TEXT_BODY, TEXT_SMALL, TEXT_TINY};

/// Everything the card renders, flattened from `Part::Approval`.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalView {
    pub approval_id: String,
    pub title: String,
    pub summary: String,
    pub risk: ApprovalRisk,
    pub state: ApprovalState,
    pub payload: serde_json::Value,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
}

impl ApprovalView {
    fn field(&self, key: &str) -> Option<&str> {
        self.payload
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
    }

    /// Preview lines: a draft shows To / Subject / body; a gated tool call shows the command.
    fn preview(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        match self.field("kind") {
            Some("draft_message") => {
                if let Some(to) = self.field("to") {
                    out.push(("To".into(), to.into()));
                }
                if let Some(subject) = self.field("subject") {
                    out.push(("Subject".into(), subject.into()));
                }
                if let Some(body) = self.field("body") {
                    out.push((String::new(), body.into()));
                }
            }
            Some("tool_call") => {
                if let Some(tool) = self.field("tool") {
                    out.push(("Tool".into(), tool.into()));
                }
                let input = self.payload.get("input");
                let command = input
                    .and_then(|i| i.get("command"))
                    .and_then(|c| c.as_str())
                    .map(str::to_owned)
                    .or_else(|| input.map(|i| i.to_string()));
                if let Some(command) = command {
                    out.push((String::new(), command));
                }
                if let Some(why) = self.field("why") {
                    out.push(("Why".into(), why.into()));
                }
            }
            Some(_) | None => {
                if !self.summary.is_empty() {
                    out.push((String::new(), self.summary.clone()));
                }
            }
        }
        out
    }
}

fn state_color(state: ApprovalState, theme: &Theme) -> Hsla {
    match state {
        ApprovalState::Pending => theme.warn,
        ApprovalState::Approved => theme.online,
        ApprovalState::Rejected => theme.danger,
        ApprovalState::Expired => theme.text_faint,
    }
}

fn chip(text: impl Into<SharedString>, color: Hsla, theme: &Theme) -> Div {
    div()
        .px(px(8.0))
        .py(px(2.0))
        .rounded_full()
        .bg(Theme::with_alpha(
            color,
            if theme.appearance == Appearance::Dark {
                0.16
            } else {
                0.12
            },
        ))
        .text_size(px(TEXT_TINY))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(text.into())
}

/// The card (Grok's inline widget): an assistant-bubble card with the title,
/// a risk + state chip, the preview, and two capsule buttons while pending.
pub fn approval_card(
    row_id: &str,
    view: &ApprovalView,
    theme: &Theme,
    on_approve: impl Fn(&mut App) + 'static,
    on_reject: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    let accent = state_color(view.state, theme);
    let pending = view.state == ApprovalState::Pending;

    let header = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(TEXT_BODY))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(view.title.clone()),
        )
        .child(chip(view.risk.label(), theme.text_muted, theme))
        .child(chip(view.state.label(), accent, theme));

    let mut body = div().flex().flex_col().gap(px(5.0));
    for (label, value) in view.preview() {
        let mono = label.is_empty();
        let mut line = div().flex().gap(px(8.0)).text_size(px(TEXT_SMALL));
        if !label.is_empty() {
            line = line.child(
                div()
                    .w(px(60.0))
                    .flex_shrink_0()
                    .text_color(theme.text_muted)
                    .child(label),
            );
        }
        line = line.child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme.text)
                .when(mono, |el| {
                    el.mt(px(4.0))
                        .px(px(12.0))
                        .py(px(9.0))
                        .rounded(px(10.0))
                        .bg(theme.code_bg)
                        .text_size(px(TEXT_SMALL))
                        .line_height(px(19.0))
                })
                .child(value),
        );
        body = body.child(line);
    }

    let mut footer = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .text_size(px(TEXT_TINY))
        .text_color(theme.text_muted);
    if pending {
        footer = footer
            .child(
                glass::quiet_button(format!("{row_id}-reject"), "Reject", theme)
                    .flex_1()
                    .on_click(move |_, _, cx| on_reject(cx)),
            )
            .child(
                glass::primary_button(format!("{row_id}-approve"), "Approve", theme)
                    .flex_1()
                    .on_click(move |_, _, cx| on_approve(cx)),
            );
    } else {
        let mut note = format!("{} ", view.state.label());
        if let Some(by) = &view.decided_by {
            note.push_str(&format!("by {by}"));
        }
        if let Some(reason) = view.reason.as_deref().filter(|r| !r.trim().is_empty()) {
            note.push_str(&format!(" · {reason}"));
        }
        footer = footer.child(note.trim().to_string());
    }

    let mut card = div()
        .id(SharedString::from(row_id.to_string()))
        .max_w(px(crate::ui::transcript::ASSISTANT_MAX_WIDTH))
        .w_full()
        .my(px(6.0))
        .flex()
        .flex_col()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(14.0))
        .rounded(px(BUBBLE_RADIUS))
        .bg(theme.bubble_assistant)
        .child(header)
        .child(body)
        .child(footer);
    if pending {
        card = card.child(
            div()
                .text_size(px(TEXT_TINY))
                .text_color(theme.text_muted)
                .child("Nothing happens until you decide. Expires in 24 h."),
        );
    }
    card
}
