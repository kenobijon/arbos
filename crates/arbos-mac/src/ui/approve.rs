//! S18 draft-and-approve card (feature `approve`), drawn as the Grok Bot
//! question card: the title is the question, the preview (a draft's To /
//! Subject / body or the gated command) is plain text under it, and the
//! answers are two lettered rows — A Approve, B Reject — in the white inner
//! list. The × top-right is Reject. The part shape is owned by
//! `arbos_core::approval`; Approve/Reject write the `approve` / `reject`
//! commands through `AppState`.

use gpui::{div, prelude::*, px, App, Div, SharedString, Stateful};

use crate::model::{ApprovalRisk, ApprovalState};
use crate::ui::theme::{Theme, TEXT_SMALL};
use crate::ui::widgets::{
    card_frame, card_header, option_list, option_row, CardLook, CloseHandler, RowState, ROW_PAD_X,
};

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

    /// The title as a question: "Send the weekly update to the beta list?".
    fn question(&self) -> String {
        let t = self.title.trim();
        if t.is_empty() {
            return "Approve this?".into();
        }
        if t.ends_with('?') {
            t.to_string()
        } else {
            format!("{}?", t.trim_end_matches('.'))
        }
    }
}

/// The card: question title, plain preview, A Approve / B Reject rows; × = Reject.
pub fn approval_card(
    row_id: &str,
    view: &ApprovalView,
    theme: &Theme,
    on_approve: impl Fn(&mut App) + 'static,
    on_reject: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    let look = CardLook::of(theme);
    let palette = look.palette;
    let pending = view.state == ApprovalState::Pending;
    let on_reject = std::rc::Rc::new(on_reject);

    let close: Option<CloseHandler> = pending.then(|| {
        let on_reject = on_reject.clone();
        Box::new(move |_: &gpui::ClickEvent, _: &mut gpui::Window, cx: &mut App| on_reject(cx))
            as CloseHandler
    });

    let mut preview = div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .px(px(1.0))
        .text_size(px(TEXT_SMALL))
        .line_height(px(TEXT_SMALL + 5.0))
        .text_color(theme.text_muted);
    let mut has_preview = false;
    for (label, value) in view.preview() {
        has_preview = true;
        preview = preview.child(if label.is_empty() {
            value
        } else {
            format!("{label}: {value}")
        });
    }

    let (approve_state, reject_state) = match view.state {
        ApprovalState::Pending => (RowState::Open, RowState::Open),
        ApprovalState::Approved => (RowState::Chosen, RowState::Faded),
        ApprovalState::Rejected => (RowState::Faded, RowState::Chosen),
        ApprovalState::Expired => (RowState::Faded, RowState::Faded),
    };
    let approve_row = option_row(
        format!("{row_id}-approve"),
        Some("A".into()),
        "Approve",
        approve_state,
        false,
        &look,
    )
    .when(pending, |el| el.on_click(move |_, _, cx| on_approve(cx)));
    let reject_row = option_row(
        format!("{row_id}-reject"),
        Some("B".into()),
        "Reject",
        reject_state,
        true,
        &look,
    )
    .when(pending, |el| {
        let on_reject = on_reject.clone();
        el.on_click(move |_, _, cx| on_reject(cx))
    });
    let mut list = option_list(&palette).child(approve_row).child(reject_row);

    if !pending {
        let mut note = view.state.label().to_string();
        if let Some(by) = &view.decided_by {
            note.push_str(&format!(" by {by}"));
        }
        if let Some(reason) = view.reason.as_deref().filter(|r| !r.trim().is_empty()) {
            note.push_str(&format!(" · {reason}"));
        }
        list = list.child(
            div()
                .border_t_1()
                .border_color(palette.hairline)
                .px(px(ROW_PAD_X))
                .py(px(7.0))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_faint)
                .child(note),
        );
    }

    card_frame(SharedString::from(row_id.to_string()), &palette)
        .child(card_header(
            format!("{row_id}-x"),
            &view.question(),
            close,
            &palette,
            theme,
        ))
        .when(has_preview, |el| el.child(preview))
        .child(list)
}
