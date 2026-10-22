//! The Create agent sheet (Jacob, 2026-10-22): Grok Bot's "Create New Bot"
//! layout with the D92 leaves — header with a close ×, the swaying preview of
//! the chosen leaf on its colour, a centred name field, the nine leaves in a
//! 5 / 4 grid, the ten autumn colours in two rows of five, and a footer with
//! Cancel + Create (disabled until named). It replaces the inline "New agent"
//! expansion of the New Message sheet. Every size is Grok's, measured on the
//! 2x reference (`media/leaf-avatars/create-sheet/ref-grok-create-new-bot.png`)
//! and written here in points; the mockup Jacob approved matched them to the
//! pixel.

use gpui::{
    div, prelude::*, px, Context, Div, ElementId, FontWeight, Hsla, MouseButton, SharedString,
};

use crate::ui::avatars::{self, leaf, LeafPick};
use crate::ui::frost::{frosted, PANEL_BLUR};
use crate::ui::icons::icon;
use crate::ui::shell::Shell;
use crate::ui::theme::{Appearance, Theme};

const WIDTH: f32 = 440.0;
const RADIUS: f32 = 16.0;
const HEADER: f32 = 54.5;
const PREVIEW: f32 = 114.0;
const PREVIEW_TOP: f32 = 62.0;
const FIELD_TOP: f32 = 183.0;
const FIELD_H: f32 = 35.0;
const FIELD_INSET: f32 = 16.5;
const LEAF_ROWS: [f32; 2] = [264.0, 320.0];
const LEAF_PITCH: f32 = 56.0;
const LEAF_DISC: f32 = 33.0;
const LEAF_RING: f32 = 40.0;
const SWATCH_ROWS: [f32; 2] = [386.0, 426.0];
const SWATCH_PITCH: f32 = 40.0;
const SWATCH: f32 = 24.0;
const SWATCH_RING: f32 = 32.0;
const FOOTER_TOP: f32 = 466.0;
const FOOTER_H: f32 = 56.5;
const PILL_H: f32 = 27.0;
const HEIGHT: f32 = FOOTER_TOP + FOOTER_H;

/// Grok's dark sheet palette (approved as is); the light theme keeps its own.
struct Palette {
    bg: Hsla,
    line: Hsla,
    title: Hsla,
    placeholder: Hsla,
    ring: Hsla,
    cancel_bg: Hsla,
    create_bg: Hsla,
    create_text: Hsla,
}

fn hex(v: u32) -> Hsla {
    gpui::Rgba {
        r: ((v >> 16) & 0xff) as f32 / 255.0,
        g: ((v >> 8) & 0xff) as f32 / 255.0,
        b: (v & 0xff) as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

fn palette(theme: &Theme) -> Palette {
    match theme.appearance {
        Appearance::Dark => Palette {
            bg: hex(0x1F1F1F),
            line: hex(0x353535),
            title: hex(0xF5F5F5),
            placeholder: hex(0x939393),
            ring: hex(0x505050),
            cancel_bg: hex(0x272727),
            create_bg: hex(0x3B3B3B),
            create_text: hex(0x737373),
        },
        Appearance::Light => Palette {
            bg: theme.card(),
            line: theme.border,
            title: theme.text,
            placeholder: theme.text_muted,
            ring: theme.border_strong,
            cancel_bg: theme.field,
            create_bg: theme.field,
            create_text: theme.text_faint,
        },
    }
}

/// The pick shown while the sheet is open: the owner's clicks, else a stable
/// default for this sheet (seeded by how many agents exist, so successive new
/// agents start on different leaves without the preview jumping while typing).
pub fn current_pick(shell: &Shell, cx: &Context<Shell>) -> LeafPick {
    let n = shell.state.read(cx).my_projects().count();
    let seed = format!("new-agent-{n}");
    leaf::pick_for(&seed, shell.new_agent_leaf, shell.new_agent_leaf_color)
}

fn ring(size: f32, selected: bool, colour: Hsla) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(if selected {
            colour
        } else {
            gpui::transparent_black()
        })
        .cursor_pointer()
}

pub fn sheet(shell: &Shell, cx: &mut Context<Shell>) -> Div {
    let theme = Theme::of(cx).clone();
    let p = palette(&theme);
    let pick = current_pick(shell, cx);
    let typed = shell.project_input.read(cx).text().trim().to_string();
    let can_create = !typed.is_empty();

    let header = div()
        .absolute()
        .left_0()
        .right_0()
        .top_0()
        .h(px(HEADER))
        .border_b_1()
        .border_color(p.line)
        .child(
            div()
                .absolute()
                .left(px(17.5))
                .top(px(16.0))
                .text_size(px(14.2))
                .line_height(px(20.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.title)
                .child("Create New Agent"),
        )
        .child(
            div()
                .id("create-agent-close")
                .absolute()
                .right(px(16.5))
                .top(px(15.0))
                .size(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(icon("xmark", 10.0, hex(0xD2D2D2)))
                .on_click(cx.listener(|this, _, window, cx| this.close_new_chat_sheet(window, cx))),
        );

    let preview = div()
        .absolute()
        .left_0()
        .right_0()
        .top(px(PREVIEW_TOP))
        .flex()
        .justify_center()
        .child(avatars::leaf_hero(pick, PREVIEW, 0xC0DE_1EAF));

    let field = div()
        .absolute()
        .left(px(FIELD_INSET))
        .right(px(FIELD_INSET))
        .top(px(FIELD_TOP))
        .h(px(FIELD_H))
        .rounded(px(8.0))
        .border_1()
        .border_color(p.line)
        .flex()
        .items_center()
        .px(px(12.0))
        .text_size(px(15.5))
        .text_color(if can_create { p.title } else { p.placeholder })
        .child(shell.project_input.clone());

    let mut rows: Vec<Div> = Vec::new();
    for (r, top) in LEAF_ROWS.iter().enumerate() {
        let leaves: Vec<u8> = if r == 0 {
            (0..5).collect()
        } else {
            (5..9).collect()
        };
        let mut row = div()
            .absolute()
            .left_0()
            .right_0()
            .top(px(top - LEAF_RING / 2.0))
            .flex()
            .justify_center();
        for index in leaves {
            let cell = LeafPick {
                leaf: index,
                color: pick.color,
            };
            row = row.child(
                ring(LEAF_RING, pick.leaf == index, p.ring)
                    .id(ElementId::Name(SharedString::from(format!(
                        "create-agent-leaf-{index}"
                    ))))
                    .mx(px((LEAF_PITCH - LEAF_RING) / 2.0))
                    .child(avatars::leaf_element(cell, LEAF_DISC))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.new_agent_leaf = Some(index);
                        cx.notify();
                    })),
            );
        }
        rows.push(row);
    }
    for (r, top) in SWATCH_ROWS.iter().enumerate() {
        let mut row = div()
            .absolute()
            .left_0()
            .right_0()
            .top(px(top - SWATCH_RING / 2.0))
            .flex()
            .justify_center();
        for j in 0..5u8 {
            let index = r as u8 * 5 + j;
            row = row.child(
                ring(SWATCH_RING, pick.color == index, p.ring)
                    .id(ElementId::Name(SharedString::from(format!(
                        "create-agent-colour-{index}"
                    ))))
                    .mx(px((SWATCH_PITCH - SWATCH_RING) / 2.0))
                    .child(
                        div()
                            .size(px(SWATCH))
                            .rounded_full()
                            .bg(avatars::leaf_disc(index)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.new_agent_leaf_color = Some(index);
                        cx.notify();
                    })),
            );
        }
        rows.push(row);
    }

    let pill = |id: &'static str, label: &'static str, w: f32, bg: Hsla, fg: Hsla| {
        div()
            .id(id)
            .absolute()
            .top(px(15.0))
            .w(px(w))
            .h(px(PILL_H))
            .rounded_full()
            .bg(bg)
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(14.2))
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg)
            .child(label)
    };
    let footer = div()
        .absolute()
        .left_0()
        .right_0()
        .top(px(FOOTER_TOP))
        .h(px(FOOTER_H))
        .border_t_1()
        .border_color(p.line)
        .child(
            pill("create-agent-cancel", "Cancel", 68.5, p.cancel_bg, p.title)
                .right(px(12.5 + 67.0 + 8.0))
                .cursor_pointer()
                .hover(|s| s.opacity(0.85))
                .on_click(cx.listener(|this, _, window, cx| this.close_new_chat_sheet(window, cx))),
        )
        .child(if can_create {
            pill(
                "create-agent-create",
                "Create",
                67.0,
                theme.primary,
                theme.on_primary,
            )
            .right(px(12.5))
            .cursor_pointer()
            .hover(|s| s.opacity(0.88))
            .on_click(cx.listener(|this, _, window, cx| this.submit_project(window, cx)))
        } else {
            pill(
                "create-agent-create",
                "Create",
                67.0,
                p.create_bg,
                p.create_text,
            )
            .right(px(12.5))
        });

    let card = div()
        .relative()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .max_w_full()
        .max_h_full()
        .rounded(px(RADIUS))
        .bg(p.bg)
        .border_1()
        .border_color(p.line)
        .shadow_lg()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(header)
        .child(preview)
        .child(field)
        .children(rows)
        .child(footer);
    // The same modal backdrop as the New Message sheet: a click outside closes.
    div()
        .absolute()
        .inset_0()
        .occlude()
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .bg(Theme::with_alpha(theme.bg, 0.55))
        .child(frosted(RADIUS, PANEL_BLUR, card))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| this.close_new_chat_sheet(window, cx)),
        )
}
