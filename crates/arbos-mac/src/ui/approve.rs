//! S18 draft-and-approve card (feature `approve`): title, risk + state chip,
//! a preview of what would happen (the draft's recipient/subject/body or the
//! command), and Approve / Reject while pending. The part shape is owned by
//! `arbos_core::approval`; Approve/Reject write the `approve` / `reject`
//! commands through `AppState`.

use gpui::{div, prelude::*, px, App, Div, FontWeight, Hsla, SharedString, Stateful};

use crate::model::{ApprovalRisk, ApprovalState};
use crate::ui::theme::{Appearance, Theme, PANEL_RADIUS};

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
        ApprovalState::Approved => theme.accent,
        ApprovalState::Rejected => theme.danger,
        ApprovalState::Expired => theme.text_faint,
    }
}

fn chip(text: impl Into<SharedString>, color: Hsla, theme: &Theme) -> Div {
    div()
        .px(px(7.0))
        .py(px(1.0))
        .rounded(px(5.0))
        .bg(Theme::with_alpha(
            color,
            if theme.appearance == Appearance::Dark {
                0.18
            } else {
                0.12
            },
        ))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(text.into())
}

fn button(
    id: impl Into<SharedString>,
    label: &str,
    fg: Hsla,
    bg: Hsla,
    on_click: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id.into())
        .px(px(12.0))
        .py(px(5.0))
        .rounded(px(7.0))
        .bg(bg)
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .cursor_pointer()
        .hover(|s| s.opacity(0.85))
        .child(label.to_string())
        .on_click(move |_, _, cx| on_click(cx))
}

/// The card. `on_approve` / `on_reject` run only while the card is pending.
pub fn approval_card(
    row_id: &str,
    view: &ApprovalView,
    theme: &Theme,
    on_approve: impl Fn(&mut App) + 'static,
    on_reject: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    let accent = state_color(view.state, theme);
    let pending = view.state == ApprovalState::Pending;

    let mut header = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(13.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(view.title.clone()),
        )
        .child(chip(view.risk.label(), theme.text_muted, theme))
        .child(chip(view.state.label(), accent, theme));
    if pending {
        header = header.child(
            div()
                .text_size(px(10.5))
                .text_color(theme.warn)
                .font_weight(FontWeight::SEMIBOLD)
                .child("WAITING FOR YOU"),
        );
    }

    let mut body = div().flex().flex_col().gap(px(4.0));
    for (label, value) in view.preview() {
        let mono = label.is_empty();
        let mut line = div().flex().gap(px(8.0)).text_size(px(12.5));
        if !label.is_empty() {
            line = line.child(
                div()
                    .w(px(56.0))
                    .flex_shrink_0()
                    .text_color(theme.text_faint)
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
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(6.0))
                        .bg(theme.code_bg)
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                })
                .child(value),
        );
        body = body.child(line);
    }

    let mut footer = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(px(11.0))
        .text_color(theme.text_faint);
    if pending {
        footer = footer
            .child(button(
                format!("{row_id}-approve"),
                "Approve",
                theme.bg,
                theme.text,
                on_approve,
            ))
            .child(button(
                format!("{row_id}-reject"),
                "Reject",
                theme.text,
                theme.surface_2,
                on_reject,
            ))
            .child(div().flex_1())
            .child("Nothing happens until you decide. Expires in 24 h.");
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

    div()
        .id(SharedString::from(row_id.to_string()))
        .w_full()
        .my(px(6.0))
        .flex()
        .flex_col()
        .gap(px(10.0))
        .px(px(14.0))
        .py(px(12.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(Theme::with_alpha(accent, 0.45))
        .border_l_2()
        .child(header)
        .child(body)
        .child(footer)
}
