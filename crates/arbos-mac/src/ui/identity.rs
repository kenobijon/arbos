//! The one avatar + name layout (Jacob, 2026-10-22: "names need to be pulled
//! back in"): a fixed picture slot, one gap, then the text, which truncates.
//! The header pill (every chat kind), the sidebar rows and the kit's people /
//! agent / group rows all draw through [`row`], so the picture can never be
//! squeezed under a long name and the gap is measured in exactly one place.
//!
//! Gaps are Grok Bot's, measured on the real Mac app (ink to ink, 2 px/pt):
//! the chat title's blob → name is 5 pt (ref 57); a sidebar / members row's
//! blob → name is 11–12 pt (refs 45, 39). Text starts about 1 pt of side
//! bearing after its box, so the box gaps below land on those.

use gpui::{div, prelude::*, px, AnyElement, Div};

/// Picture → name in the chat header pill (Grok: 5 pt ink).
pub const HEADER_GAP: f32 = 4.0;
/// Picture → name in a list row — sidebar, members, people (Grok: 11–12 pt ink).
pub const ROW_GAP: f32 = 10.0;

/// A fixed `size` square for the picture. Whatever the picture does — a leaf's
/// hover gust, a photo, a stacked badge hanging off the corner — the slot keeps
/// its width, so the name's left edge is `slot right + gap` at every width.
pub fn slot(size: f32, picture: impl IntoElement) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .debug_selector(|| "identity-picture".into())
        .child(picture)
}

/// Picture slot, `gap`, then `text` (which shrinks and truncates; the slot
/// never does). The caller adds id, hover, padding and trailing children.
pub fn row(size: f32, gap: f32, picture: impl IntoElement, text: AnyElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(gap))
        .min_w_0()
        .child(slot(size, picture))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .debug_selector(|| "identity-text".into())
                .child(text),
        )
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, Pixels, TestAppContext};

    use super::*;
    use crate::ui::avatars;

    /// Three picture kinds: a leaf disc (the hover kind, as in the header and
    /// rows), an initials disc, and a photo-shaped box with a badge hanging off
    /// its corner (the shared-project row).
    #[derive(Clone, Copy)]
    enum Picture {
        Leaf,
        Initials,
        PhotoWithBadge,
    }

    struct Row {
        width: f32,
        size: f32,
        gap: f32,
        picture: Picture,
        name: &'static str,
        subtitle: Option<&'static str>,
    }

    impl gpui::Render for Row {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let size = self.size;
            let picture: AnyElement = match self.picture {
                Picture::Leaf => {
                    avatars::leaf_element(avatars::LeafPick { leaf: 3, color: 4 }, size)
                        .into_any_element()
                }
                Picture::Initials => div()
                    .size(px(size))
                    .flex_shrink_0()
                    .rounded_full()
                    .child("LI")
                    .into_any_element(),
                Picture::PhotoWithBadge => div()
                    .relative()
                    .size(px(size))
                    .flex_shrink_0()
                    .child(
                        div()
                            .absolute()
                            .right(px(-4.0))
                            .bottom(px(-2.0))
                            .size(px(16.0)),
                    )
                    .into_any_element(),
            };
            let mut text = div().flex().flex_col().min_w_0();
            text = text.child(div().truncate().child(self.name));
            if let Some(sub) = self.subtitle {
                text = text.child(div().truncate().child(sub));
            }
            div()
                .w(px(self.width))
                .h(px(60.0))
                .flex()
                .child(row(size, self.gap, picture, text.into_any_element()).pr(px(4.0)))
        }
    }

    fn bounds(cx: &mut TestAppContext, r: Row) -> (Bounds<Pixels>, Bounds<Pixels>) {
        let (_view, vcx) = cx.add_window_view(|_, _| r);
        vcx.run_until_parked();
        (
            vcx.debug_bounds("identity-picture").expect("picture drawn"),
            vcx.debug_bounds("identity-text").expect("text drawn"),
        )
    }

    const LONG: &str = "Lium Lium Lium Lium Lium Lium Lium Lium Lium Lium Lium Lium";
    const LONG_SUB: &str =
        "Arbos, two agents and a very long subtitle that never ends and keeps going";

    #[gpui::test]
    fn the_name_starts_at_the_slot_edge_plus_the_gap_at_every_width(cx: &mut TestAppContext) {
        for picture in [Picture::Leaf, Picture::Initials, Picture::PhotoWithBadge] {
            for (size, gap) in [(22.0, HEADER_GAP), (36.0, ROW_GAP)] {
                for width in [60.0, 120.0, 200.0, 320.0, 600.0] {
                    for (name, subtitle) in [("Lium", None), (LONG, Some(LONG_SUB))] {
                        let (pic, text) = bounds(
                            cx,
                            Row {
                                width,
                                size,
                                gap,
                                picture,
                                name,
                                subtitle,
                            },
                        );
                        let pic_w: f32 = pic.size.width.into();
                        let pic_right: f32 = pic.right().into();
                        let text_x: f32 = text.origin.x.into();
                        assert_eq!(pic_w, size, "slot keeps its size at width {width}");
                        assert!(
                            (text_x - (pic_right + gap)).abs() <= 0.5,
                            "name x {text_x} vs slot right {pic_right} + gap {gap} at width {width} ({name:?})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_gaps_are_grok_bots() {
        let (header, row) = (
            std::hint::black_box(HEADER_GAP),
            std::hint::black_box(ROW_GAP),
        );
        // Ref 57: 5 pt ink; ref 45 / 39: 11–12 pt ink; ≈ 1 pt of side bearing.
        assert!((4.0..=5.0).contains(&header), "header gap {header}");
        assert!((10.0..=11.0).contains(&row), "row gap {row}");
    }
}
