//! Attachment cards (F10, D38): a file the agent produced, blob-backed. Icon by kind, title,
//! size, an inline thumbnail for images, and Download → `~/Downloads/<name>` then reveal.
//! Clicking the picture or the card opens the media viewer (`ui::media_viewer`).

use std::path::Path;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, img, prelude::*, px, AnyElement, App, Entity, FontWeight, Hsla, ObjectFit, RenderImage,
    SharedString,
};

use crate::app::{AppState, FORWARDED_FROM_PREFIX};
use crate::model::{Attachment, AttachmentKind};
use crate::ui::theme::{Appearance, Theme};

/// What a card click does: open the media viewer on this attachment.
pub type OpenFn = Rc<dyn Fn(&mut App)>;

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

/// The card element for one attachment row. `forwarded_from` is the message's
/// "Forwarded from …" line, shown as the card's hint instead of a bubble.
pub fn card(
    row_id: &str,
    attachment: &Attachment,
    preview: Preview,
    forwarded_from: Option<&str>,
    open: Option<OpenFn>,
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
    if let Some(from) = forwarded_from {
        subtitle = format!("{from} · {subtitle}");
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
                cx.stop_propagation();
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
                strip.child(img(image).w_full().h_full().object_fit(ObjectFit::Contain))
            }
            Preview::Loading => strip.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("Loading preview…"),
            ),
            Preview::Unavailable => strip.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("No preview"),
            ),
        };
        let strip = match open.clone() {
            Some(open) => strip.cursor_pointer().on_click(move |_, _, cx| open(cx)),
            None => strip,
        };
        card = card.child(strip);
    }
    // The header opens the viewer too (files have no picture to click); the
    // Download button inside it stops the click.
    let header = match open {
        Some(open) => header
            .id(SharedString::from(format!("{row_id}-open")))
            .cursor_pointer()
            .on_click(move |_, _, cx| open(cx))
            .into_any_element(),
        None => header.into_any_element(),
    };
    card.child(header).into_any_element()
}

/// The "Forwarded from <name>" line of a message, when its text is one.
pub fn forwarded_from(text: &str) -> Option<&str> {
    let t = text.trim();
    (t.starts_with(FORWARDED_FROM_PREFIX) && !t.contains('\n')).then_some(t)
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

/// Hand a fetched file to macOS: Quick Look (`qlmanage -p`) for documents, the
/// default app for the rest (QuickTime Player for video). Elsewhere `xdg-open`.
pub fn open_with_system(path: &Path, quick_look: bool) {
    #[cfg(target_os = "macos")]
    let spawned = if quick_look {
        Command::new("qlmanage")
            .arg("-p")
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    } else {
        Command::new("open").arg(path).spawn()
    };
    #[cfg(not(target_os = "macos"))]
    let spawned = {
        let _ = quick_look;
        Command::new("xdg-open")
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    if let Err(err) = spawned {
        eprintln!("open {}: {err}", path.display());
    }
}
