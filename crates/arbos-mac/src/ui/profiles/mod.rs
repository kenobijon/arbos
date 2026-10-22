//! Profile pages in the right pane (three-pane rule, 2026-10-20): a user, a
//! group (people chat or shared project) or an agent. One pane beside the
//! chat — it never covers it — closed by the X at its top right, the same
//! skeleton for all three: hero (picture, name), tabs, then lists or
//! settings. Pictures and names are editable only on what you own.
//!
//! Everything shown is derived from state the app already holds: members from
//! the space, media / files / voice / links from the loaded chat docs,
//! presence from the room frames.

mod agent;
mod group;
pub(crate) mod kit;
mod me;
mod server;
mod user;

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, AnyElement, Context, Div, ElementId, FontWeight, Hsla, RenderImage,
    ScrollHandle, SharedString, Stateful,
};

use crate::app::{AppState, ProfilePage};
use crate::model::{now_ms, Attachment, AttachmentKind, Chat, Message, Part};
use crate::ui::glass;
use crate::ui::icons::icon;
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, TEXT_BODY, TEXT_SMALL, TEXT_TINY};

/// The right pane's width (mockups b, d, g: a quarter of the window).
pub const SHEET_WIDTH: f32 = 340.0;
/// Mocks b/d/g: the picture is about a fifth of the pane's width.
pub(super) const HERO_AVATAR: f32 = 72.0;

/// The open page, if any — rendered as the right pane.
pub fn pane(page: &ProfilePage, shell: &Shell, cx: &mut Context<Shell>) -> Option<Stateful<Div>> {
    match page {
        ProfilePage::User { handle, space_id } => {
            user::page(handle, space_id.as_deref(), shell, cx)
        }
        ProfilePage::Group { space_id } => group::page(space_id, shell, cx),
        ProfilePage::Agent { project_id } => agent::page(project_id, shell, cx),
        ProfilePage::Me => Some(me::page(shell, cx)),
        ProfilePage::Server { server_id } => server::page(server_id, shell, cx),
    }
}

// ----- scaffold ----------------------------------------------------------------

/// The right pane: an X at the top right (plus the page's `right` control),
/// then the scrolling body. It sits beside the chat as a flex sibling, so it
/// never covers it; `scroll` tracks the body's scroll when a page needs to
/// move it. Clicks inside stay inside (pickers, menus, fields).
pub(super) fn frame(
    body: Div,
    right: Option<Stateful<Div>>,
    scroll: Option<&ScrollHandle>,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Stateful<Div> {
    pane_frame(SHEET_WIDTH, None, body, right, scroll, theme, cx)
}

/// A page pushed from another (a sender tapped in a group): the header leads with `‹`
/// back to the page underneath and `title`.
pub(super) fn pushed_frame(
    title: &str,
    body: Div,
    right: Option<Stateful<Div>>,
    scroll: Option<&ScrollHandle>,
    shell: &Shell,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Stateful<Div> {
    if shell.state.read(cx).profile_stack.is_empty() {
        return pane_frame(SHEET_WIDTH, None, body, right, scroll, theme, cx);
    }
    let back = glass::circle("profile-back", 30.0, theme)
        .child(icon("chevron-left", 14.0, theme.text))
        .on_click(cx.listener(|this, _, _, cx| {
            this.state.update(cx, |s, cx| s.back_profile(cx));
        }));
    pane_frame(
        SHEET_WIDTH,
        Some((title, back)),
        body,
        right,
        scroll,
        theme,
        cx,
    )
}

/// The pane with a pushed page: `‹` back and the page title lead the header.
pub(crate) fn pane_frame(
    width: f32,
    back: Option<(&str, Stateful<Div>)>,
    body: Div,
    right: Option<Stateful<Div>>,
    scroll: Option<&ScrollHandle>,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Stateful<Div> {
    let close = glass::circle("profile-close", 30.0, theme)
        .child(icon("xmark", 13.0, theme.text))
        .on_click(cx.listener(|this, _, window, cx| this.close_profile(window, cx)));
    // An opaque bar the body scrolls under; once scrolled it shows its lower
    // edge, so content passing beneath reads as behind a bar, not clipped by
    // nothing (Jacob, Oct 21; [feedback mac-collapsed-toggle-inline]).
    let scrolled = scroll.is_some_and(|h| f32::from(h.offset().y) < -0.5);
    let mut header = div()
        .h(px(crate::ui::theme::HEADER_HEIGHT))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(10.0))
        .bg(theme.card())
        .when(scrolled, |el| el.border_b_1().border_color(theme.border));
    if let Some((title, back)) = back {
        header = header.child(back).child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(TEXT_BODY))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(title.to_string()),
        );
    } else {
        header = header.child(div().flex_1());
    }
    let header = header.children(right).child(close);
    div()
        .id("profile-pane")
        .w(px(width))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(theme.border)
        .bg(theme.pane())
        .child(header)
        .child(
            body.id("profile-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .when_some(scroll, |el, h| el.track_scroll(h))
                // A wheel tick re-renders the pane so the bar's edge follows the
                // scroll position (the handle alone does not notify).
                .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify())),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                // The root would refocus the composer over the pane's fields.
                cx.stop_propagation();
                this.close_popovers(cx);
            }),
        )
}

/// Big avatar, name, status line.
pub(super) fn hero(
    avatar: AnyElement,
    name: AnyElement,
    status: &str,
    status_color: Hsla,
    _theme: &Theme,
) -> Div {
    div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.0))
        .pt(px(4.0))
        .pb(px(18.0))
        .child(div().size(px(HERO_AVATAR)).child(avatar))
        .child(
            div()
                .pt(px(8.0))
                .w_full()
                .max_w(px(SHEET_WIDTH - 80.0))
                .flex()
                .justify_center()
                .child(name),
        )
        .child(
            div()
                .text_size(px(TEXT_SMALL))
                .text_color(status_color)
                .child(status.to_string()),
        )
}

/// A click handler a page hands the shared hero (`cx.listener(..)`, boxed).
pub(super) type HeroClick = Box<dyn Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App)>;

/// The hero picture of something you own: hover shows a crayon over it,
/// click runs `on_click` (mockup c). `None` = read-only, just the picture.
pub(super) fn editable_picture(
    id: &'static str,
    picture: AnyElement,
    on_click: Option<HeroClick>,
) -> Stateful<Div> {
    let editable = on_click.is_some();
    div()
        .id(id)
        .group("hero-picture")
        .relative()
        .size(px(HERO_AVATAR))
        .flex_shrink_0()
        .child(picture)
        .when(editable, |el| {
            el.cursor_pointer().child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(Theme::with_alpha(gpui::black(), 0.45))
                    .opacity(0.0)
                    .group_hover("hero-picture", |s| s.opacity(1.0))
                    .child(icon("pencil", 30.0, gpui::white())),
            )
        })
        .when_some(on_click, |el, f| el.on_click(move |e, w, cx| f(e, w, cx)))
}

/// The hero name of something you own: hover shows it as a field, click
/// edits it in place (`editing` swaps in the shared name input). `None` for
/// `on_click` = read-only text.
pub(super) fn editable_name(
    id: &'static str,
    name: &str,
    editing: bool,
    input: &gpui::Entity<crate::ui::composer::ComposerInput>,
    on_click: Option<HeroClick>,
    theme: &Theme,
) -> Stateful<Div> {
    let field = div()
        .id(id)
        .h(px(36.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .border_1()
        .text_size(px(TEXT_BODY + 6.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text);
    if editing {
        return field
            .w(px(SHEET_WIDTH - 64.0))
            .bg(theme.field)
            .border_color(theme.primary)
            .child(input.clone());
    }
    let editable = on_click.is_some();
    field
        .max_w(px(SHEET_WIDTH - 64.0))
        .border_color(gpui::transparent_black())
        .when(editable, |el| {
            el.cursor_text()
                .hover(|s| s.bg(theme.field).border_color(theme.field_border))
        })
        .child(div().truncate().child(name.to_string()))
        .when_some(on_click, |el, f| el.on_click(move |e, w, cx| f(e, w, cx)))
}

pub(super) fn card(theme: &Theme) -> Div {
    kit::card(theme)
}

/// Segmented tab bar; `tabs[i]` is selected when `i == current`.
pub(super) fn tab_bar(
    tabs: &[&'static str],
    current: usize,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Div {
    // Mocks b/g: two bare labels, the current one on a capsule, centred —
    // no track, no border; 18 pt down to the first card.
    let mut bar = div()
        .flex_shrink_0()
        .mx(px(16.0))
        .mb(px(18.0))
        .flex()
        .justify_center()
        .gap(px(4.0));
    for (i, name) in tabs.iter().enumerate() {
        let on = i == current;
        bar = bar.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "profile-tab-{name}"
                ))))
                .h(px(30.0))
                .px(px(14.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(TEXT_BODY))
                .font_weight(if on {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(if on { theme.text } else { theme.text_muted })
                .bg(if on {
                    theme.surface_2
                } else {
                    gpui::transparent_black()
                })
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(name.to_string())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.set_profile_tab(i, cx));
                })),
        );
    }
    bar
}

/// Centered grey line for an empty tab.
pub(super) fn empty(text: &str, theme: &Theme) -> Div {
    kit::empty(text, theme)
}

/// A list row: 36 pt picture, title, subtitle, optional trailing element —
/// the kit's row, so members, groups and files share one height and inset.
pub(super) fn list_row(
    id: impl Into<ElementId>,
    leading: AnyElement,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    _subtitle_color: Hsla,
    trailing: Option<AnyElement>,
    theme: &Theme,
) -> Stateful<Div> {
    kit::row(
        id,
        kit::Leading::Avatar(leading),
        title,
        Some(subtitle.into()),
        None,
        trailing,
        false,
        theme,
    )
}

// ----- content from the chat docs ----------------------------------------------

/// One attachment found in a chat, with where it came from.
pub(super) struct Found {
    pub attachment: Attachment,
    /// Who shared it ("Mira", "@sam", "You", the agent's name).
    pub by: String,
    pub when: u64,
    pub thumbnail: Option<Arc<RenderImage>>,
}

/// A link found in a message.
pub(super) struct Link {
    pub url: String,
    pub by: String,
    pub chat: String,
    pub when: u64,
}

pub(super) struct Content {
    pub media: Vec<Found>,
    pub files: Vec<Found>,
    pub voice: Vec<Found>,
    pub links: Vec<Link>,
}

/// Everything shareable in `chats`, newest first. Only loaded docs count.
pub(super) fn collect(state: &AppState, chats: &[&Chat]) -> Content {
    let mut out = Content {
        media: Vec::new(),
        files: Vec::new(),
        voice: Vec::new(),
        links: Vec::new(),
    };
    for chat in chats {
        for m in state.messages_of(&chat.id) {
            let by = sender_label(state, m);
            for part in &m.parts {
                match part {
                    Part::Attachment { attachment, .. } => {
                        let thumbnail = attachment
                            .blob_ref
                            .as_ref()
                            .and_then(|k| state.thumbnails.get(k).cloned());
                        let found = Found {
                            attachment: attachment.clone(),
                            by: sender_name(state, m),
                            when: m.created_at,
                            thumbnail,
                        };
                        match attachment.kind() {
                            AttachmentKind::Image | AttachmentKind::Video => out.media.push(found),
                            AttachmentKind::Audio => out.voice.push(found),
                            _ => out.files.push(found),
                        }
                    }
                    Part::Text { text, .. } => {
                        for url in urls_in(text) {
                            out.links.push(Link {
                                url,
                                by: by.clone(),
                                chat: chat.title.clone(),
                                when: m.created_at,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out.media.sort_by_key(|a| std::cmp::Reverse(a.when));
    out.files.sort_by_key(|a| std::cmp::Reverse(a.when));
    out.voice.sort_by_key(|a| std::cmp::Reverse(a.when));
    out.links.sort_by_key(|a| std::cmp::Reverse(a.when));
    out.links.dedup_by(|a, b| a.url == b.url);
    out
}

/// The sender's name for a file subline ("Mira"); the handle only when there is no name.
fn sender_name(state: &AppState, m: &Message) -> String {
    if let Some(n) = m.sender_name.as_deref().filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    if let Some(p) = m
        .sender_handle
        .as_deref()
        .filter(|h| !h.is_empty())
        .and_then(|h| state.person_by_handle(h))
    {
        return p.display_name();
    }
    sender_label(state, m)
}

fn sender_label(state: &AppState, m: &Message) -> String {
    if let Some(h) = m.sender_handle.as_deref().filter(|h| !h.is_empty()) {
        return format!("@{h}");
    }
    if let Some(n) = m.sender_name.as_deref().filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    if state.is_mine(m) {
        return "You".into();
    }
    m.sender
        .strip_prefix("agent:")
        .or_else(|| m.sender.strip_prefix("project:"))
        .and_then(|pid| state.project(pid))
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Agent".into())
}

/// `http(s)://…` tokens, trailing punctuation trimmed, in order of appearance.
pub(super) fn urls_in(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '(' || c == ')')
        .filter(|t| t.starts_with("http://") || t.starts_with("https://"))
        .map(|t| {
            t.trim_end_matches(['.', ',', ';', ':', '!', '?', '"', '\''])
                .to_string()
        })
        .filter(|t| t.len() > 8)
        .collect()
}

/// "today", "yesterday", "3 days ago"…
pub(super) fn when_text(at: u64) -> String {
    let ago = now_ms().saturating_sub(at);
    if ago < 86_400_000 {
        "Today".into()
    } else {
        crate::app::ago_text(ago)
    }
}

fn size_text(bytes: Option<u64>) -> Option<String> {
    let b = bytes?;
    Some(if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{} KB", b / 1024)
    } else {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    })
}

/// Gutter between media tiles.
const MEDIA_GAP: f32 = 3.0;

/// Media tab: three square tiles per row (aspect-fill crop), small even
/// gutters; a photo / video glyph while a thumbnail loads. The cell is
/// floored against the pane's inner width (the 1 pt left border counts) —
/// at exactly a third the last tile used to wrap and leave two per row.
pub(super) fn media_grid(items: &[Found], theme: &Theme) -> Div {
    if items.is_empty() {
        return empty("No photos or videos yet", theme);
    }
    let cell = ((SHEET_WIDTH - 1.0 - 2.0 * kit::INSET - 2.0 * MEDIA_GAP) / 3.0).floor();
    let mut grid = div()
        .flex_shrink_0()
        .flex()
        .flex_wrap()
        .gap(px(MEDIA_GAP))
        .px(px(kit::INSET))
        .pb(px(16.0));
    for (i, f) in items.iter().enumerate() {
        let tile = div()
            .id(ElementId::Name(SharedString::from(format!(
                "profile-media-{i}"
            ))))
            .size(px(cell))
            .rounded(px(8.0))
            .overflow_hidden()
            .bg(theme.surface_1)
            .flex()
            .items_center()
            .justify_center();
        grid = grid.child(match &f.thumbnail {
            // The tile's overflow clip is rectangular, so the picture rounds itself.
            Some(img) => tile.child(
                gpui::img(img.clone())
                    .size(px(cell))
                    .rounded(px(8.0))
                    .object_fit(gpui::ObjectFit::Cover),
            ),
            None => tile.child(icon(
                if f.attachment.kind() == AttachmentKind::Video {
                    "video"
                } else {
                    "photo"
                },
                22.0,
                theme.text_faint,
            )),
        });
    }
    grid
}

/// Files / Voice tabs: one row per attachment.
pub(super) fn file_list(items: &[Found], glyph: &'static str, none: &str, theme: &Theme) -> Div {
    if items.is_empty() {
        return empty(none, theme);
    }
    let mut list = card(theme);
    for (i, f) in items.iter().enumerate() {
        // Redesign mock d: the file name, then "212 kB · Mira · Today" — size,
        // who shared it, when.
        let title = if f.attachment.name.is_empty() {
            f.attachment.title.clone().unwrap_or_default()
        } else {
            f.attachment.name.clone()
        };
        let mut sub = vec![f.by.clone(), when_text(f.when)];
        if let Some(size) = size_text(f.attachment.bytes) {
            sub.insert(0, size);
        }
        // The tile names the type (PDF, XLS, ZIP …) when the extension says so.
        let ext = f
            .attachment
            .name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_uppercase())
            .filter(|e| (2..=4).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()));
        let tile = div()
            .size(px(kit::AVATAR))
            .rounded(px(10.0))
            .bg(theme.surface_2)
            .flex()
            .items_center()
            .justify_center();
        let tile = match ext {
            Some(e) => tile
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.text_muted)
                .child(e),
            None => tile.child(icon(glyph, 18.0, theme.text_muted)),
        };
        list = list.child(list_row(
            ElementId::Name(SharedString::from(format!("profile-file-{i}"))),
            tile.into_any_element(),
            title,
            sub.join(" · "),
            theme.text_muted,
            None,
            theme,
        ));
    }
    list
}

/// Host for a link row (no scheme, path, or leading `www.`).
fn link_host(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_string()
}

/// The site's own favicon — fetched through the avatar pipeline (ICO/PNG/JPEG).
fn favicon_url(host: &str) -> Option<String> {
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
    {
        return None;
    }
    Some(format!("https://{host}/favicon.ico"))
}

/// Links tab: the site's favicon when we have it, else a letter tile; URL; who shared it.
pub(super) fn link_list(
    items: &[Link],
    shell: &Shell,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Div {
    if items.is_empty() {
        return empty("No links shared yet", theme);
    }
    let wants: Vec<String> = items
        .iter()
        .filter_map(|l| favicon_url(&link_host(&l.url)))
        .collect();
    shell.state.update(cx, |s, _| {
        for url in wants {
            s.want_avatar(url);
        }
    });
    let state = shell.state.read(cx);
    let mut list = card(theme);
    for (i, l) in items.iter().enumerate() {
        let host = link_host(&l.url);
        let letter = host
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "#".into());
        let favicon = favicon_url(&host).and_then(|u| state.avatar(&u));
        let leading = match favicon {
            Some(img) => div()
                .size(px(kit::AVATAR))
                .rounded(px(10.0))
                .overflow_hidden()
                .bg(theme.surface_2)
                .child(
                    gpui::img(img)
                        .size(px(kit::AVATAR))
                        .rounded(px(10.0))
                        .object_fit(gpui::ObjectFit::Cover),
                )
                .into_any_element(),
            None => div()
                .size(px(kit::AVATAR))
                .rounded(px(10.0))
                .bg(crate::ui::groups::sender_color(&host, theme))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(TEXT_BODY + 2.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(letter)
                .into_any_element(),
        };
        let url = l.url.clone();
        list = list.child(
            list_row(
                ElementId::Name(SharedString::from(format!("profile-link-{i}"))),
                leading,
                host,
                format!("{} · {} · {}", l.by, l.chat, when_text(l.when)),
                theme.text_muted,
                Some(
                    div()
                        .max_w(px(150.0))
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.primary)
                        .truncate()
                        .child(l.url.clone())
                        .into_any_element(),
                ),
                theme,
            )
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))),
        );
    }
    list
}

/// The chats of a space, for content tabs (one per agent unless multi-chat is on).
pub(super) fn chats_of<'a>(state: &'a AppState, space_id: &str) -> Vec<&'a Chat> {
    state.visible_tabs(space_id)
}
