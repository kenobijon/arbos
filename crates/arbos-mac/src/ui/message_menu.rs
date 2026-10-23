//! The message bubble's menu — right-click on the Mac, the iPhone's
//! long-press (`MessageMenu.swift`): **Copy, Reply, Forward, Delete**
//! (Jacob, #1370 dropped the emoji reaction row; Forward is
//! feedback app-01M5MXRZ7609S70P9S9P2RZDDF). One builder for every bubble,
//! and one placement rule: the menu always sits fully inside the window —
//! flipped to the other side of the click near the right or bottom edge,
//! clamped to the edge margin when neither side fits.

use gpui::{
    div, point, prelude::*, px, size, Div, ElementId, FontWeight, MouseButton, Pixels, Point, Size,
    Window,
};

use crate::app::ForwardTarget;
use crate::model::Id;
use crate::ui::avatars;
use crate::ui::glass::{
    self, MENU_CARD_BORDER, MENU_CARD_PADDING, MENU_DIVIDER_HEIGHT, MENU_GAP, MENU_ROW_HEIGHT,
};
use crate::ui::icons::icon;
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, TEXT_SMALL, TEXT_TINY};

pub const MESSAGE_MENU_WIDTH: f32 = 180.0;
/// Space kept between the menu and every window edge.
pub const MENU_EDGE_MARGIN: f32 = 8.0;

/// Forward picker card (same size as the media viewer's).
const PICKER_WIDTH: f32 = 360.0;
const PICKER_HEIGHT: f32 = 420.0;
const PICKER_ROW: f32 = 52.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageMenuItem {
    Copy,
    Reply,
    Forward,
    Delete,
}

impl MessageMenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MessageMenuItem::Copy => "Copy",
            MessageMenuItem::Reply => "Reply",
            MessageMenuItem::Forward => "Forward",
            MessageMenuItem::Delete => "Delete",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            MessageMenuItem::Copy => "copy",
            MessageMenuItem::Reply => "reply",
            MessageMenuItem::Forward => "forward",
            MessageMenuItem::Delete => "trash",
        }
    }

    pub fn danger(self) -> bool {
        matches!(self, MessageMenuItem::Delete)
    }
}

/// The menu's items, in order. One list for every bubble: the set is the
/// contract, not a per-row choice.
pub const ITEMS: [MessageMenuItem; 4] = [
    MessageMenuItem::Copy,
    MessageMenuItem::Reply,
    MessageMenuItem::Forward,
    MessageMenuItem::Delete,
];

/// Divider before the destructive item.
const DIVIDERS: usize = 1;

/// The menu's size, from the same constants `glass::menu_card` /
/// `menu_row` / `menu_divider` lay it out with.
pub fn menu_size() -> Size<Pixels> {
    let rows = ITEMS.len() as f32;
    let dividers = DIVIDERS as f32;
    let children = rows + dividers;
    let height = 2.0 * (MENU_CARD_PADDING + MENU_CARD_BORDER)
        + rows * MENU_ROW_HEIGHT
        + dividers * MENU_DIVIDER_HEIGHT
        + (children - 1.0) * MENU_GAP;
    size(px(MESSAGE_MENU_WIDTH), px(height))
}

/// Top-left of a menu of `size` opened at `anchor` (the click) in a window of
/// `viewport`: at the click when it fits, flipped to the other side of the
/// click when it would cross the right or bottom edge, and clamped to the
/// edge margin when neither side fits.
pub fn place(anchor: Point<Pixels>, size: Size<Pixels>, viewport: Size<Pixels>) -> Point<Pixels> {
    point(
        px(place_axis(
            anchor.x.into(),
            size.width.into(),
            viewport.width.into(),
        )),
        px(place_axis(
            anchor.y.into(),
            size.height.into(),
            viewport.height.into(),
        )),
    )
}

fn place_axis(anchor: f32, extent: f32, limit: f32) -> f32 {
    let margin = MENU_EDGE_MARGIN;
    let start = if anchor + extent + margin <= limit {
        anchor
    } else if anchor - extent >= margin {
        anchor - extent
    } else {
        limit - margin - extent
    };
    start.max(margin)
}

/// Rect of the menu as placed: `(origin, size)`, for the callers (and tests)
/// that check it against the window.
pub fn placed_rect(anchor: Point<Pixels>, viewport: Size<Pixels>) -> (Point<Pixels>, Size<Pixels>) {
    let size = menu_size();
    (place(anchor, size, viewport), size)
}

/// `ARBOS_RIG_MESSAGE_MENU=<x>,<y>`: open the menu for the newest message of
/// the open chat at that window position (screenshot rigs; posted pointer
/// events never reach the app in the EC2 Mac's GUI session).
pub fn rig_anchor() -> Option<Point<Pixels>> {
    let spec = std::env::var("ARBOS_RIG_MESSAGE_MENU").ok()?;
    let (x, y) = spec.split_once(',')?;
    Some(point(
        px(x.trim().parse::<f32>().ok()?),
        px(y.trim().parse::<f32>().ok()?),
    ))
}

/// The open menu, appended straight to the shell's root (taffy places an
/// absolute child relative to its parent; a static wrapper would push it off).
pub fn render(shell: &Shell, window: &Window, cx: &mut Context<Shell>) -> Option<Div> {
    let (chat_id, message_id, at) = shell.message_menu.clone()?;
    let theme = Theme::of(cx).clone();
    let (origin, _) = placed_rect(at, window.viewport_size());
    let mut body = div().flex().flex_col().gap(px(MENU_GAP));
    for item in ITEMS {
        if item.danger() {
            body = body.child(glass::menu_divider(&theme));
        }
        let (chat, msg) = (chat_id.clone(), message_id.clone());
        body = body.child(
            glass::menu_row(
                format!("message-menu-{}", item.label().to_ascii_lowercase()),
                item.glyph(),
                item.label(),
                item.danger(),
                &theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.message_menu = None;
                run(this, item, chat.clone(), msg.clone(), cx);
                cx.notify();
            })),
        );
    }
    Some(
        div()
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(glass::frosted_menu(MESSAGE_MENU_WIDTH, &theme, body)),
    )
}

/// The Forward picker overlay: searchable list of DMs, agents and groups.
pub fn forward_picker(shell: &Shell, theme: &Theme, cx: &mut Context<Shell>) -> impl IntoElement {
    let query = shell.viewer_search.read(cx).text().to_string();
    let targets: Vec<ForwardTarget> = shell.state.read(cx).forward_targets(&query);
    let close = shell.state.clone();
    let close_x = shell.state.clone();
    let search = div()
        .flex()
        .items_center()
        .gap(px(7.0))
        .h(px(30.0))
        .mx(px(12.0))
        .px(px(10.0))
        .rounded(px(8.0))
        .bg(theme.surface_2)
        .cursor_text()
        .child(icon("search", 13.0, theme.text_muted))
        .child(div().flex_1().min_w_0().child(shell.viewer_search.clone()));
    let mut list = div()
        .id("message-forward-list")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(8.0))
        .py(px(6.0));
    if targets.is_empty() {
        list = list.child(
            div()
                .px(px(12.0))
                .py(px(10.0))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(if query.trim().is_empty() {
                    "No chats yet.".to_string()
                } else {
                    format!("No chat matches “{}”.", query.trim())
                }),
        );
    }
    for (i, target) in targets.into_iter().enumerate() {
        let avatar = shell
            .state
            .read(cx)
            .project(&target.space_id)
            .map(|p| avatars::project_hero(p, 36.0, theme))
            .unwrap_or_else(|| avatars::element(&target.space_id, &target.name, None, 36.0, theme));
        let pick = target.clone();
        list = list.child(
            div()
                .id(ElementId::NamedInteger(
                    "message-forward-row".into(),
                    i as u64,
                ))
                .h(px(PICKER_ROW))
                .px(px(8.0))
                .rounded(px(8.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(avatar)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(target.name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(target.subtitle.clone()),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let pick = pick.clone();
                    this.state
                        .update(cx, |s, cx| s.forward_message_to(pick, cx));
                    this.viewer_search.update(cx, |input, cx| input.clear(cx));
                })),
        );
    }
    let footnote = div()
        .px(px(16.0))
        .py(px(10.0))
        .border_t_1()
        .border_color(theme.border)
        .text_size(px(TEXT_TINY))
        .text_color(theme.text_muted)
        .child("Sends it as a new message in that chat.");
    let card = div()
        .id("message-forward")
        .w(px(PICKER_WIDTH))
        .h(px(PICKER_HEIGHT))
        .rounded(px(14.0))
        .bg(theme.surface_1)
        .border_1()
        .border_color(theme.border_strong)
        .flex()
        .flex_col()
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .pl(px(16.0))
                .pr(px(12.0))
                .pt(px(12.0))
                .pb(px(10.0))
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child("Forward to"),
                )
                .child(
                    div()
                        .id("message-forward-close")
                        .size(px(24.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_2))
                        .child(icon("xmark", 11.0, theme.text_muted))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            close_x.update(cx, |s, cx| s.close_message_forward(cx));
                        }),
                ),
        )
        .child(search)
        .child(list)
        .child(footnote);
    div()
        .id("message-forward-backdrop")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(Theme::with_alpha(gpui::black(), 0.35))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            close.update(cx, |s, cx| s.close_message_forward(cx));
        })
        .child(card)
}

fn run(
    shell: &mut Shell,
    item: MessageMenuItem,
    chat_id: Id,
    message_id: Id,
    cx: &mut Context<Shell>,
) {
    match item {
        MessageMenuItem::Copy => {
            shell
                .transcript
                .update(cx, |t, cx| t.copy_message(&message_id, cx));
        }
        MessageMenuItem::Reply => {
            shell
                .state
                .update(cx, |s, cx| s.set_reply_to(chat_id, message_id, cx));
        }
        MessageMenuItem::Forward => {
            shell.viewer_search.update(cx, |input, cx| input.clear(cx));
            shell
                .state
                .update(cx, |s, cx| s.open_message_forward(chat_id, message_id, cx));
        }
        MessageMenuItem::Delete => {
            shell
                .state
                .update(cx, |s, cx| s.delete_message(chat_id, message_id, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: (f32, f32) = (1180.0, 780.0);

    fn inside(origin: Point<Pixels>, size: Size<Pixels>, viewport: Size<Pixels>) -> bool {
        let (x, y): (f32, f32) = (origin.x.into(), origin.y.into());
        let (w, h): (f32, f32) = (size.width.into(), size.height.into());
        let (vw, vh): (f32, f32) = (viewport.width.into(), viewport.height.into());
        x >= 0.0 && y >= 0.0 && x + w <= vw && y + h <= vh
    }

    #[test]
    fn items_are_exactly_copy_reply_forward_delete() {
        let labels: Vec<&str> = ITEMS.iter().map(|i| i.label()).collect();
        assert_eq!(labels, ["Copy", "Reply", "Forward", "Delete"]);
        // No emoji row: every label is plain ASCII text.
        assert!(ITEMS.iter().all(|i| i.label().is_ascii()));
    }

    #[test]
    fn menu_fits_at_a_right_edge_bubble() {
        // Jacob's own bubble hugs the right edge; the click is 12 pt from it.
        let viewport = size(px(WIN.0), px(WIN.1));
        let (origin, size) = placed_rect(point(px(WIN.0 - 12.0), px(300.0)), viewport);
        assert!(inside(origin, size, viewport), "{origin:?} {size:?}");
        // Flipped: the menu ends at the click, not clamped somewhere else.
        assert_eq!(origin.x + size.width, px(WIN.0 - 12.0));
        assert_eq!(origin.y, px(300.0));
    }

    #[test]
    fn menu_fits_at_a_bottom_edge_bubble() {
        let viewport = size(px(WIN.0), px(WIN.1));
        let (origin, size) = placed_rect(point(px(400.0), px(WIN.1 - 20.0)), viewport);
        assert!(inside(origin, size, viewport), "{origin:?} {size:?}");
        assert_eq!(origin.y + size.height, px(WIN.1 - 20.0));
        assert_eq!(origin.x, px(400.0));
    }

    #[test]
    fn menu_fits_in_the_bottom_right_corner() {
        let viewport = size(px(WIN.0), px(WIN.1));
        let (origin, size) = placed_rect(point(px(WIN.0 - 2.0), px(WIN.1 - 2.0)), viewport);
        assert!(inside(origin, size, viewport), "{origin:?} {size:?}");
    }

    #[test]
    fn menu_stays_at_the_click_when_it_fits() {
        let viewport = size(px(WIN.0), px(WIN.1));
        let (origin, _) = placed_rect(point(px(300.0), px(200.0)), viewport);
        assert_eq!(origin, point(px(300.0), px(200.0)));
    }

    #[test]
    fn menu_clamps_to_the_margin_at_the_top_left() {
        let viewport = size(px(WIN.0), px(WIN.1));
        let (origin, size) = placed_rect(point(px(0.0), px(0.0)), viewport);
        assert!(inside(origin, size, viewport));
        assert_eq!(origin, point(px(MENU_EDGE_MARGIN), px(MENU_EDGE_MARGIN)));
    }

    #[test]
    fn menu_clamps_when_the_window_is_short() {
        // A window shorter than twice the menu: neither side of a mid-height
        // click fits, so the menu hugs the bottom margin.
        let viewport = size(px(600.0), px(200.0));
        let (origin, size) = placed_rect(point(px(100.0), px(100.0)), viewport);
        assert!(inside(origin, size, viewport), "{origin:?} {size:?}");
    }
}
