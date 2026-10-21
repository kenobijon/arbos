//! Attachment cards (F10, D38): a file the agent produced, blob-backed. Icon by kind, title,
//! size, an inline thumbnail for images, and Download → `~/Downloads/<name>` then reveal.
//! Click an image strip to open an expanded in-app preview (no download required).

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use gpui::{
    div, img, prelude::*, px, AnyElement, Entity, FontWeight, Hsla, MouseButton, ObjectFit,
    RenderImage, SharedString,
};

use crate::app::AppState;
use crate::model::{Attachment, AttachmentKind};
use crate::ui::icons::icon;
use crate::ui::theme::{Appearance, Theme};

pub const CARD_WIDTH: f32 = 360.0;
/// Fixed, so a row's height does not change when the thumbnail arrives.
pub const THUMB_HEIGHT: f32 = 200.0;

/// Glyph and tint for the kind badge.
pub fn badge(kind: AttachmentKind, theme: &Theme) -> (&'static str, Hsla) {
    let tint = |h: f32| Hsla {
        h,
        s: 0.55,
        l: match theme.appearance {
            Appearance::Dark => 0.62,
            Appearance::Light => 0.42,
        },
        a: 1.0,
    };
    match kind {
        AttachmentKind::Image => ("▧", tint(0.78)),
        AttachmentKind::Video => ("▶", tint(0.95)),
        AttachmentKind::Slides => ("▣", tint(0.07)),
        AttachmentKind::Doc => ("≣", tint(0.60)),
        AttachmentKind::Sheet => ("▦", tint(0.38)),
        AttachmentKind::Pdf => ("▤", tint(0.0)),
        AttachmentKind::Audio => ("♪", tint(0.50)),
        AttachmentKind::File => ("⬚", theme.text_muted),
    }
}

/// What the image strip of a card shows.
pub enum Preview {
    /// The thumbnail bytes are on their way.
    Loading,
    Ready(Arc<RenderImage>),
    /// No blob, or the fetch failed: the file is still listed and downloadable.
    Unavailable,
}

/// The card element for one attachment row.
pub fn card(
    row_id: &str,
    attachment: &Attachment,
    preview: Preview,
    state: Entity<AppState>,
    theme: &Theme,
) -> AnyElement {
    let kind = attachment.kind();
    let (glyph, tint) = badge(kind, theme);
    let key = attachment.blob_ref.clone();
    let name = attachment.name.clone();
    let title = attachment.title().to_owned();
    let size = attachment.size_label();
    let mut subtitle = kind.label().to_owned();
    if title != name {
        subtitle.push_str(" · ");
        subtitle.push_str(&name);
    }
    if !size.is_empty() {
        subtitle.push_str(" · ");
        subtitle.push_str(&size);
    }

    let download = {
        let state = state.clone();
        let key = key.clone();
        let name = name.clone();
        div()
            .id(SharedString::from(format!("{row_id}-download")))
            .flex_shrink_0()
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .text_size(px(11.5))
            .text_color(if key.is_some() {
                theme.accent
            } else {
                theme.text_faint
            })
            .when(key.is_some(), |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(theme.accent_soft).border_color(theme.border_strong))
            })
            .child("Download")
            .on_click(move |_, _, cx| {
                if let Some(key) = key.clone() {
                    let name = name.clone();
                    state.update(cx, |s, cx| s.download_attachment(key, name, cx));
                }
            })
    };

    let header = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .py(px(8.0))
        .child(
            div()
                .flex_shrink_0()
                .size(px(30.0))
                .rounded(px(7.0))
                .bg(Theme::with_alpha(tint, 0.16))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(15.0))
                .text_color(tint)
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_muted)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(subtitle),
                ),
        )
        .child(download);

    let mut card = div()
        .id(SharedString::from(row_id.to_owned()))
        .w(px(CARD_WIDTH))
        .max_w_full()
        .flex()
        .flex_col()
        .rounded(px(10.0))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border)
        .overflow_hidden();

    if kind == AttachmentKind::Image {
        let strip = div()
            .id(SharedString::from(format!("{row_id}-preview")))
            .w_full()
            .h(px(THUMB_HEIGHT))
            .bg(Theme::with_alpha(tint, 0.08))
            .flex()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_color(theme.border);
        let strip = match preview {
            Preview::Ready(image) => {
                let state = state.clone();
                let key = key.clone();
                let open = image.clone();
                strip
                    .cursor_pointer()
                    .child(img(image).w_full().h_full().object_fit(ObjectFit::Contain))
                    .on_click(move |_, _, cx| {
                        if let Some(key) = key.clone() {
                            state.update(cx, |s, cx| s.preview_attachment_image(key, cx));
                        } else {
                            state.update(cx, |s, cx| s.open_image_preview(open.clone(), cx));
                        }
                    })
            }
            Preview::Loading => strip.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("Loading preview…"),
            ),
            Preview::Unavailable => {
                let strip = strip.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child("No preview"),
                );
                if let Some(key) = key.clone() {
                    let state = state.clone();
                    strip.cursor_pointer().on_click(move |_, _, cx| {
                        state.update(cx, |s, cx| s.preview_attachment_image(key.clone(), cx));
                    })
                } else {
                    strip
                }
            }
        };
        card = card.child(strip);
    }
    card.child(header).into_any_element()
}

/// Full-window image lightbox: dimmed backdrop, contained picture, × to dismiss.
/// `image` is `None` while a missing thumbnail is still fetching.
pub fn preview_overlay(
    image: Option<Arc<RenderImage>>,
    state: Entity<AppState>,
    theme: &Theme,
) -> impl IntoElement {
    let close = state.clone();
    let close_btn = state;
    let body: AnyElement = match image {
        Some(image) => div()
            .id("image-preview-frame")
            .w_full()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(img(image).w_full().h_full().object_fit(ObjectFit::Contain))
            .into_any_element(),
        None => div()
            .id("image-preview-loading")
            .text_size(px(13.0))
            .text_color(theme.text_muted)
            .child("Loading preview…")
            .into_any_element(),
    };
    div()
        .id("image-preview")
        .absolute()
        .inset_0()
        .occlude()
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .flex()
        .items_center()
        .justify_center()
        .p(px(28.0))
        .bg(Theme::with_alpha(theme.bg, 0.82))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            close.update(cx, |s, cx| s.close_image_preview(cx));
        })
        .child(
            div()
                .id("image-preview-body")
                .relative()
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(body)
                .child(
                    div()
                        .id("image-preview-close")
                        .absolute()
                        .top(px(4.0))
                        .right(px(4.0))
                        .size(px(36.0))
                        .rounded_full()
                        .bg(Theme::with_alpha(theme.surface_2, 0.92))
                        .border_1()
                        .border_color(theme.border_strong)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_1))
                        .child(icon("xmark", 14.0, theme.text))
                        .on_click(move |_, _, cx| {
                            close_btn.update(cx, |s, cx| s.close_image_preview(cx));
                        }),
                ),
        )
}

/// Reveal the saved file: Finder on macOS, the file manager (or the default app) elsewhere.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let spawned = Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(not(target_os = "macos"))]
    let spawned = Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(err) = spawned {
        eprintln!("reveal {}: {err}", path.display());
    }
}
