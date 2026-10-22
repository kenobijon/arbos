//! The iPhone app's glass controls, in GPUI: the glass circle (header
//! buttons), the glass pill (header title), grouped cards and rows for the
//! sheets, menu rows, and the centered sheet scaffold every modal uses.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, Div, ElementId, FontWeight, InteractiveElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, Pixels, Point, SharedString, Stateful, WindowControlArea,
};

use crate::ui::frost::{frosted, MENU_BLUR, PANEL_BLUR};
use crate::ui::icons::icon;
use crate::ui::theme::{Theme, PANEL_RADIUS, TEXT_BODY, TEXT_SMALL, TEXT_TINY};

/// Drag this region to move the window.
///
/// macOS windows open with `app_owns_titlebar_drag`, so AppKit will not move
/// the window itself. Mark the title-strip (and any other chrome the user
/// should pull) with this; a left-drag calls [`gpui::Window::start_window_move`].
/// Interactive children should `cx.stop_propagation()` on mouse-down so a
/// click on a button does not start a move.
/// How far the pointer travels (pt) after a press before the window moves.
/// A press that stays put is a click for whatever is under it (a bubble in
/// the transcript's top fade), so the drag catchment can overlap content.
pub const WINDOW_DRAG_SLOP: f32 = 4.0;

pub trait WindowDrag: InteractiveElement + Sized {
    fn window_drag(self) -> Self {
        // The press position while a move is pending; None otherwise.
        let pending: Rc<Cell<Option<Point<Pixels>>>> = Rc::new(Cell::new(None));
        self.window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, {
                let pending = pending.clone();
                move |event: &MouseDownEvent, _, _| pending.set(Some(event.position))
            })
            .on_mouse_up(MouseButton::Left, {
                let pending = pending.clone();
                move |_, _, _| pending.set(None)
            })
            .on_mouse_up_out(MouseButton::Left, {
                let pending = pending.clone();
                move |_, _, _| pending.set(None)
            })
            .on_mouse_down_out({
                let pending = pending.clone();
                move |_, _, _| pending.set(None)
            })
            .on_mouse_move({
                let pending = pending.clone();
                move |event: &MouseMoveEvent, window, _| {
                    if let Some(start) = pending.get() {
                        let dx: f32 = (event.position.x - start.x).into();
                        let dy: f32 = (event.position.y - start.y).into();
                        if dx * dx + dy * dy >= WINDOW_DRAG_SLOP * WINDOW_DRAG_SLOP {
                            pending.set(None);
                            window.start_window_move();
                        }
                    }
                }
            })
    }
}

impl<E: InteractiveElement> WindowDrag for E {}

/// A glass circle: field fill at 85 %, hairline rim. Content centred.
pub fn circle(id: impl Into<ElementId>, size: f32, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme.glass_fill())
        .border_1()
        .border_color(theme.field_border)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
}

/// Filled primary button (white on dark): the iPhone CTA.
pub fn primary_button(id: impl Into<ElementId>, label: &str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(38.0))
        .px(px(18.0))
        .rounded_full()
        .bg(theme.primary)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(TEXT_SMALL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.on_primary)
        .cursor_pointer()
        .hover(|s| s.opacity(0.88))
        .child(label.to_string())
}

/// Quiet pill button (field fill, rim): Cancel / Deny / secondary actions.
pub fn quiet_button(id: impl Into<ElementId>, label: &str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(38.0))
        .px(px(18.0))
        .rounded_full()
        .bg(theme.field)
        .border_1()
        .border_color(theme.field_border)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(TEXT_SMALL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .child(label.to_string())
}

/// Small capsule chip ("On Arbos", "owner", "3 agents").
pub fn chip(text: impl Into<SharedString>, color: gpui::Hsla) -> Div {
    div()
        .px(px(8.0))
        .py(px(2.0))
        .rounded_full()
        .bg(Theme::with_alpha(color, 0.16))
        .text_size(px(TEXT_TINY))
        .text_color(color)
        .flex_shrink_0()
        .whitespace_nowrap()
        .child(text.into())
}

/// Section label above a grouped card ("ACCOUNT").
pub fn section_label(text: &str, theme: &Theme) -> Div {
    div()
        .px(px(16.0))
        .pb(px(6.0))
        .text_size(px(TEXT_TINY))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text_muted)
        .child(text.to_uppercase())
}

/// A grouped card: rows separated by hairlines, 14 px corners.
///
/// `flex_shrink_0` is required: `overflow_hidden` makes the automatic
/// minimum height zero, so inside a scrolling flex column the card would
/// otherwise shrink and clip its rows (Settings, New Message, profiles).
pub fn group(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .rounded(px(PANEL_RADIUS))
        .bg(theme.surface_1)
        .overflow_hidden()
}

/// One row in a grouped card: label left, value right. Not interactive.
pub fn row(label: impl Into<SharedString>, theme: &Theme) -> Div {
    // The pane kit's metrics (profiles::kit): 48 pt rows, 16 pt inset. The
    // hairline hangs 1 pt below the row like `kit::row`'s: between rows it
    // lands on the next row's first pixel, under the last row the card's clip
    // swallows it. A bottom border inside the row drew a stray line under a
    // one-row card (Settings › Notifications, Jacob Oct 21).
    div()
        .relative()
        .flex()
        .items_center()
        .gap(px(12.0))
        .min_h(px(48.0))
        .px(px(16.0))
        .py(px(6.0))
        .text_size(px(TEXT_BODY))
        .text_color(theme.text)
        .child(div().flex_1().min_w_0().child(label.into()))
        .child(
            div()
                .absolute()
                .left(px(16.0))
                .right(px(0.0))
                .bottom(px(-1.0))
                .h(px(1.0))
                .bg(theme.border),
        )
}

/// Apple-style icon tile for settings rows: 30 px rounded square, muted
/// monochrome fill, white glyph (D80).
pub fn icon_tile(glyph: &str, theme: &Theme) -> Div {
    div()
        .size(px(30.0))
        .flex_shrink_0()
        .rounded(px(7.5))
        .bg(match theme.appearance {
            crate::ui::theme::Appearance::Dark => gpui::Hsla::from(gpui::rgb(0x5c5c60)),
            crate::ui::theme::Appearance::Light => gpui::Hsla::from(gpui::rgb(0x6e6e73)),
        })
        .flex()
        .items_center()
        .justify_center()
        .child(icon(glyph, 15.0, gpui::white()))
}

/// Segmented choice inside a row: `[Dark] Light`.
pub fn segment(id: impl Into<ElementId>, label: &str, on: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.0))
        .px(px(12.0))
        .rounded_full()
        .flex()
        .items_center()
        .text_size(px(TEXT_SMALL))
        .font_weight(if on {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .bg(if on {
            theme.bubble
        } else {
            gpui::transparent_black()
        })
        // The on-segment sits on the user-bubble colour (black in light), so it
        // takes the bubble's text colour, not the page text colour.
        .text_color(if on {
            theme.bubble_text()
        } else {
            theme.text_muted
        })
        .cursor_pointer()
        .when(!on, |el| el.hover(|s| s.text_color(theme.text)))
        .child(label.to_string())
}

/// Menu row (the "…" menu): glyph, label, optional trailing value.
pub fn menu_row(
    id: impl Into<ElementId>,
    glyph: &str,
    label: impl Into<SharedString>,
    danger: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let color = if danger { theme.danger } else { theme.text };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(10.0))
        .h(px(36.0))
        .px(px(12.0))
        .rounded(px(9.0))
        .text_size(px(TEXT_SMALL))
        .text_color(color)
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .child(icon(glyph, 15.0, color))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
}

/// Hitbox behaviour for every floating popover / menu / sheet: block clicks
/// **and** wheel/trackpad scroll into the view behind it. GPUI's
/// [`InteractiveElement::occlude`] (`HitboxBehavior::BlockMouse`) — not
/// `block_mouse_except_scroll`, which would still let the transcript scroll.
fn floating_layer_hitbox_behavior() -> gpui::HitboxBehavior {
    gpui::HitboxBehavior::BlockMouse
}

/// Wrap a floating popover so clicks and wheel/trackpad events inside it
/// never reach the view behind (e.g. the chat transcript) or the root's
/// "click elsewhere closes popovers" handler.
pub fn popover(child: impl IntoElement) -> Div {
    debug_assert_eq!(
        floating_layer_hitbox_behavior(),
        gpui::HitboxBehavior::BlockMouse
    );
    div()
        // Occlude: GPUI otherwise delivers scroll to every hitbox under the
        // pointer, so a wheel over this menu would also move the transcript.
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .child(child)
}

/// Shared vertical scroll area for inside a popover, menu, or sheet. Prefer
/// this over a bare `overflow_y_scroll` so floating layers share one
/// container. Pair with [`popover`] / [`frosted_menu`] / [`sheet`] (they
/// occlude so the wheel never chains into the view behind).
pub fn contained_y_scroll(id: impl Into<ElementId>) -> Stateful<Div> {
    div().id(id).flex().flex_col().overflow_y_scroll()
}

pub fn menu_divider(theme: &Theme) -> Div {
    div().h(px(1.0)).my(px(4.0)).bg(theme.border)
}

/// A dropdown card under a header control: frosted, rounded, shadowed.
pub fn menu_card(width: f32, theme: &Theme) -> Div {
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(1.0))
        .p(px(6.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
}

/// Frosted dropdown used by the tab "+" menu and sidebar row menus. Occludes
/// so a wheel over the menu never scrolls the list or transcript behind it,
/// even when the caller did not wrap with [`popover`].
pub fn frosted_menu(width: f32, theme: &Theme, body: Div) -> impl IntoElement {
    div()
        .occlude()
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .child(frosted(
            PANEL_RADIUS,
            MENU_BLUR,
            menu_card(width, theme).child(body),
        ))
}

/// Centered sheet with a dimmed backdrop: the iPhone's `.sheet`. `header`
/// is the title row (left control, centred title, right control).
///
/// A mouse-down inside the card stays inside it: the root's "click elsewhere"
/// handler never sees it (so a field in the sheet keeps the keyboard), and
/// neither does the returned backdrop — a sheet that closes on a click
/// outside puts its handler on the backdrop.
pub fn sheet(width: f32, theme: &Theme, header: Div, body: impl IntoElement) -> Div {
    let card = div()
        .w(px(width))
        .max_w_full()
        .max_h_full()
        .flex()
        .flex_col()
        .rounded(px(PANEL_RADIUS + 6.0))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(header)
        .child(body);
    // The backdrop is modal: nothing under it is hovered, clicked, or
    // scrolled (GPUI delivers mouse/wheel events to every hitbox under the
    // pointer otherwise — same containment as [`popover`]).
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
        .child(frosted(PANEL_RADIUS + 6.0, PANEL_BLUR, card))
}

/// Sheet header: `[left] Title [right]`.
pub fn sheet_header(
    title: &str,
    left: Option<Stateful<Div>>,
    right: Option<Stateful<Div>>,
    theme: &Theme,
) -> Div {
    div()
        .relative()
        .h(px(60.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .px(px(14.0))
        // The centred title paints first so the corner buttons stay clickable.
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(TEXT_BODY + 1.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(title.to_string()),
        )
        .child(div().min_w(px(72.0)).children(left))
        .child(div().min_w(px(72.0)).flex().justify_end().children(right))
}

/// Text button for sheet headers ("Cancel", "Done"): a glass pill.
pub fn sheet_button(id: impl Into<ElementId>, label: &str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(34.0))
        .px(px(14.0))
        .rounded_full()
        .bg(theme.glass_fill())
        .border_1()
        .border_color(theme.field_border)
        .flex()
        .items_center()
        .text_size(px(TEXT_SMALL))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text)
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .child(label.to_string())
}

/// A macOS-style switch (read-only picture; the row around it is the control).
pub fn switch(on: bool, theme: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .w(px(38.0))
        .h(px(22.0))
        .rounded_full()
        .bg(if on {
            theme.online
        } else {
            theme.border_strong
        })
        .flex()
        .items_center()
        .px(px(2.0))
        .when(on, |el| el.justify_end())
        .child(
            div()
                .size(px(18.0))
                .rounded_full()
                .bg(gpui::white())
                .shadow_sm(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Front-to-back layers under the pointer. A layer with `contains_scroll`
    /// (popover / menu / sheet via occlude) consumes the wheel; layers behind
    /// it are not scroll targets — the same rule GPUI's hit-test applies for
    /// `BlockMouse`.
    fn scroll_hit_layers<'a>(
        front_to_back: impl IntoIterator<Item = (bool /* contains_scroll */, &'a str)>,
    ) -> Vec<&'a str> {
        let mut hit = Vec::new();
        for (contains_scroll, name) in front_to_back {
            hit.push(name);
            if contains_scroll {
                break;
            }
        }
        hit
    }

    #[test]
    fn floating_layer_blocks_scroll_not_only_clicks() {
        // `BlockMouseExceptScroll` would still let the transcript move under
        // the + tab menu; popovers must use full `BlockMouse` (occlude).
        assert_eq!(
            floating_layer_hitbox_behavior(),
            gpui::HitboxBehavior::BlockMouse
        );
        assert_ne!(
            floating_layer_hitbox_behavior(),
            gpui::HitboxBehavior::BlockMouseExceptScroll
        );
    }

    #[test]
    fn wheel_over_popover_does_not_move_transcript() {
        // Regression for popover-scroll-bleed-2026-10-20: a wheel over the
        // agent chat's + tab menu (New chat + Previous chats) must not scroll
        // the transcript behind it.
        let hit = scroll_hit_layers([(true, "popover"), (false, "transcript")]);
        assert_eq!(hit, vec!["popover"]);
        assert!(!hit.contains(&"transcript"));
    }

    #[test]
    fn wheel_over_menu_does_not_chain_into_underlying_scroll() {
        let hit = scroll_hit_layers([(true, "frosted_menu"), (false, "sidebar_list")]);
        assert_eq!(hit, vec!["frosted_menu"]);
    }
}
