//! Transcript cards: status rows (S4/S6/S8), tool folds and progress cards
//! (S7/S17), subagent chips, diff/file cards, the browser card (S16), and the
//! error chip (S10). All collapsed by default; details are lazy.

use gpui::{App, ClickEvent, Div, ElementId, FontWeight, SharedString, Stateful, Window, div, prelude::*, px};

use crate::model::{StatusKind, ToolStatus};
use crate::ui::motion::PulseClock;
use crate::ui::theme::{PANEL_RADIUS, Theme};
use crate::ui::transcript::ToolView;

fn card_shell(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border)
}

pub fn spinner(theme: &Theme, cx: &App) -> Div {
    let phase = PulseClock::phase(cx, 900);
    div()
        .size(px(8.0))
        .rounded_full()
        .bg(Theme::with_alpha(theme.accent, 0.35 + 0.65 * phase))
}

pub fn status_icon(status: Option<ToolStatus>, is_error: bool, theme: &Theme, cx: &App) -> Div {
    match (status, is_error) {
        (Some(ToolStatus::Running), _) => spinner(theme, cx),
        (Some(ToolStatus::Failed), _) | (_, true) => div()
            .text_size(px(11.0))
            .text_color(theme.danger)
            .child("✗"),
        (Some(ToolStatus::Ok), false) | (None, false) => div()
            .text_size(px(11.0))
            .text_color(theme.ok)
            .child("✓"),
    }
}

pub fn status_row(code: StatusKind, text: &str, live: bool, theme: &Theme, cx: &App) -> Div {
    match code {
        StatusKind::Deciding => div()
            .w_full()
            .py(px(4.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .when(live, |el| el.child(spinner(theme, cx)))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme.text_muted)
                    .italic()
                    .child(text.to_string()),
            ),
        StatusKind::SubagentWorking => card_shell(theme).py(px(4.0)).child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .when(live, |el| el.child(spinner(theme, cx)))
                .when(!live, |el| {
                    el.child(div().text_color(theme.ok).text_size(px(11.0)).child("✓"))
                })
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(text.to_string()),
                ),
        ),
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
        StatusKind::Handoff | StatusKind::Note | StatusKind::Other => div()
            .w_full()
            .py(px(4.0))
            .text_size(px(12.0))
            .text_color(theme.text_faint)
            .italic()
            .child(text.to_string()),
    }
}

fn fmt_duration(ms: Option<u64>) -> String {
    match ms {
        Some(ms) if ms >= 1000 => format!("{:.1} s", ms as f32 / 1000.0),
        Some(ms) => format!("{ms} ms"),
        None => String::new(),
    }
}

/// S7/S17: "Called N tools ▾" folded; expanded shows one card per call.
pub fn tool_group(
    row_id: &str,
    tools: &[ToolView],
    open: bool,
    live: bool,
    theme: &Theme,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_show_output: impl Fn(Option<String>, &mut App) + 'static + Clone,
    cx: &App,
) -> Stateful<Div> {
    let running = tools.iter().any(|t| t.status == Some(ToolStatus::Running));
    let failed = tools.iter().any(|t| t.is_error || t.status == Some(ToolStatus::Failed));
    let total_ms: u64 = tools.iter().filter_map(|t| t.duration_ms).sum();
    let summary = if tools.len() == 1 {
        let t = &tools[0];
        format!("{} {}", t.call.name, t.call.input_line())
    } else if running {
        format!("Running {} tools…", tools.len())
    } else {
        format!("Called {} tools", tools.len())
    };

    let mut header = div()
        .id(SharedString::from(format!("{row_id}-header")))
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(8.0))
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .rounded(px(PANEL_RADIUS))
        .on_click(on_toggle);
    header = header.child(if running {
        spinner(theme, cx)
    } else if failed {
        div().text_size(px(11.0)).text_color(theme.danger).child("✗")
    } else {
        div().text_size(px(11.0)).text_color(theme.ok).child("✓")
    });
    header = header
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(12.5))
                .text_color(theme.text)
                .overflow_hidden()
                .child(summary),
        )
        .when(total_ms > 0 && !running, |el| {
            el.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child(fmt_duration(Some(total_ms))),
            )
        })
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(if open { "▾" } else { "▸" }),
        );

    let mut card = card_shell(theme)
        .id(SharedString::from(row_id.to_string()))
        .my(px(4.0))
        .child(header);

    if open {
        let mut body = div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(theme.border);
        for (i, tool) in tools.iter().enumerate() {
            let show = on_show_output.clone();
            let blob = tool.output_ref.clone();
            let mut line = div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(7.0))
                .when(i > 0, |el| el.border_t_1().border_color(theme.border))
                .child(div().w(px(10.0)).flex().justify_center().child(status_icon(tool.status, tool.is_error, theme, cx)))
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(tool.call.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child(tool.call.input_line()),
                );
            if !tool.output_summary.is_empty() {
                line = line.child(
                    div()
                        .max_w(px(220.0))
                        .overflow_hidden()
                        .text_size(px(11.5))
                        .text_color(theme.text_faint)
                        .child(tool.output_summary.clone()),
                );
            }
            line = line.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child(fmt_duration(tool.duration_ms)),
            );
            if tool.status != Some(ToolStatus::Running) {
                line = line.child(
                    div()
                        .id(ElementId::NamedInteger(
                            SharedString::from(format!("{row_id}-out")),
                            i as u64,
                        ))
                        .text_size(px(11.0))
                        .text_color(theme.accent)
                        .cursor_pointer()
                        .child("Show output")
                        .on_click(move |_, _, cx| show(blob.clone(), cx)),
                );
            }
            body = body.child(line);
        }
        card = card.child(body);
    }
    let _ = live;
    card
}

/// A subagent spawn never folds (S7): its own chip with the name.
pub fn subagent_chip(tool: &ToolView, theme: &Theme) -> Div {
    let name = tool.subagent_name.clone().unwrap_or_default();
    div()
        .w_full()
        .py(px(4.0))
        .flex()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(5.0))
                .rounded_full()
                .bg(theme.accent_soft)
                .border_1()
                .border_color(Theme::with_alpha(theme.accent, 0.4))
                .text_size(px(12.0))
                .text_color(theme.text)
                .child(div().text_color(theme.accent).child("⇢"))
                .child(format!("Subagent {name}"))
                .child(
                    div()
                        .text_color(theme.text_faint)
                        .child(tool.call.input_line()),
                ),
        )
}

pub fn diff_card(
    row_id: &str,
    path: &str,
    added: u32,
    removed: u32,
    theme: &Theme,
    on_open: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    card_shell(theme)
        .id(SharedString::from(row_id.to_string()))
        .my(px(4.0))
        .cursor_pointer()
        .hover(|s| s.border_color(theme.border_strong))
        .on_click(move |_, _, cx| on_open(cx))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .child(div().text_size(px(11.0)).text_color(theme.text_faint).child("DIFF"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(path.to_string()),
                )
                .child(div().text_size(px(12.0)).text_color(theme.ok).child(format!("+{added}")))
                .child(div().text_size(px(12.0)).text_color(theme.danger).child(format!("−{removed}")))
                .child(div().text_size(px(11.0)).text_color(theme.text_faint).child("▸")),
        )
}

pub fn file_card(
    row_id: &str,
    path: &str,
    lines: u32,
    theme: &Theme,
    on_open: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    card_shell(theme)
        .id(SharedString::from(row_id.to_string()))
        .my(px(4.0))
        .cursor_pointer()
        .hover(|s| s.border_color(theme.border_strong))
        .on_click(move |_, _, cx| on_open(cx))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .child(div().text_size(px(11.0)).text_color(theme.text_faint).child("FILE"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(path.to_string()),
                )
                .child(div().text_size(px(11.5)).text_color(theme.text_muted).child(format!("{lines} lines")))
                .child(div().text_size(px(11.0)).text_color(theme.text_faint).child("▸")),
        )
}

/// S16: favicon slot, title, URL, last action, thumbnail, Watch.
pub fn browser_card(
    row_id: &str,
    title: &str,
    url: &str,
    last_action: &str,
    live: bool,
    theme: &Theme,
    on_watch: impl Fn(&mut App) + 'static,
) -> Stateful<Div> {
    let mut card = card_shell(theme).id(SharedString::from(row_id.to_string())).my(px(4.0));
    let header = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(8.0))
        .child(
            div()
                .size(px(14.0))
                .rounded(px(3.0))
                .bg(theme.surface_2)
                .border_1()
                .border_color(theme.border_strong),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .overflow_hidden()
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .overflow_hidden()
                        .child(url.to_string()),
                ),
        )
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(if live {
                    format!("· {last_action}")
                } else {
                    "closed".to_string()
                }),
        );
    card = card.child(header);
    if live {
        card = card.child(
            div()
                .flex()
                .items_end()
                .gap(px(12.0))
                .px(px(12.0))
                .pb(px(10.0))
                .child(
                    div()
                        .w(px(240.0))
                        .h(px(160.0))
                        .rounded(px(6.0))
                        .bg(theme.code_bg)
                        .border_1()
                        .border_color(theme.border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child("thumbnail · refreshes every ~2 s"),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{row_id}-watch")))
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(px(6.0))
                        .bg(theme.accent_soft)
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .cursor_pointer()
                        .hover(|s| s.bg(Theme::with_alpha(theme.accent, 0.3)))
                        .child("Watch")
                        .on_click(move |_, _, cx| on_watch(cx)),
                ),
        );
    }
    card
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
        .child(div().text_color(theme.danger).text_size(px(12.0)).child("✗"))
        .child(
            div()
                .flex_1()
                .text_size(px(12.5))
                .text_color(theme.text)
                .child(message.to_string()),
        )
        .child(div().text_size(px(11.0)).text_color(theme.text_faint).child(code.to_string()))
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
