//! The few non-prose rows the transcript still shows: the working line
//! (S4/S6/S17 collapsed into one indicator), the ask-user card (S8) and the
//! error chip (S10). Tool detail lives in the Details drawer.

use gpui::{div, prelude::*, px, App, Div, FontWeight, SharedString, Stateful};

use crate::model::StatusKind;
use crate::ui::motion::PulseClock;
use crate::ui::theme::{Theme, PANEL_RADIUS};

pub fn spinner(theme: &Theme, cx: &App) -> Div {
    let phase = PulseClock::phase(cx, 900);
    div()
        .size(px(8.0))
        .rounded_full()
        .bg(Theme::with_alpha(theme.accent, 0.35 + 0.65 * phase))
}

/// One subtle line: pulsing dot + the short step label from the box.
pub fn working_row(label: &str, theme: &Theme, cx: &App) -> Div {
    div()
        .w_full()
        .py(px(4.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(spinner(theme, cx))
        .child(
            div()
                .text_size(px(12.5))
                .text_color(theme.text_muted)
                .italic()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label.to_string()),
        )
}

pub fn status_row(code: StatusKind, text: &str, live: bool, theme: &Theme, cx: &App) -> Div {
    match code {
        StatusKind::AskUser => div()
            .w_full()
            .py(px(4.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .py(px(10.0))
                    .rounded(px(PANEL_RADIUS))
                    .bg(theme.card())
                    .border_l_2()
                    .border_color(theme.warn)
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.warn)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("WAITING FOR YOU"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.5))
                            .text_color(theme.text)
                            .child(text.to_string()),
                    ),
            ),
        StatusKind::Deciding
        | StatusKind::SubagentWorking
        | StatusKind::SubagentDone
        | StatusKind::ToolStart
        | StatusKind::Handoff
        | StatusKind::Note
        | StatusKind::Other => {
            if live {
                working_row(text, theme, cx)
            } else {
                div()
                    .w_full()
                    .py(px(4.0))
                    .text_size(px(12.0))
                    .text_color(theme.text_faint)
                    .italic()
                    .child(text.to_string())
            }
        }
    }
}

/// S10: red-tinted chip with the short message and Retry.
pub fn error_chip(
    row_id: &str,
    code: &str,
    message: &str,
    theme: &Theme,
    on_retry: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(SharedString::from(row_id.to_string()))
        .my(px(4.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.danger_soft)
        .border_1()
        .border_color(Theme::with_alpha(theme.danger, 0.4))
        .child(
            div()
                .text_color(theme.danger)
                .text_size(px(12.0))
                .child("✗"),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(12.5))
                .text_color(theme.text)
                .child(message.to_string()),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(code.to_string()),
        )
        .child(
            div()
                .id(SharedString::from(format!("{row_id}-retry")))
                .text_size(px(12.0))
                .text_color(theme.accent)
                .cursor_pointer()
                .child("Retry")
                .on_click(move |_, _, cx| on_retry(cx)),
        )
}
