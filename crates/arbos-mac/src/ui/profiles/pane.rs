//! The one profile pane (D93, Jacob 2026-10-22): people, own profile, agents,
//! groups, servers and channels are all rendered by [`render`] from a
//! [`ProfilePane`] spec. The spec says *which* tabs exist and *which* rows sit
//! under each; this module owns the look — hero, tab pill, cards, spacing,
//! footer, the frame (×, ‹ back from the stack, Done) — so every entity looks
//! the same and the iPhone's `ProfilePane` mirrors it one to one. Rows come
//! from `kit` (`row`, `nav_row`, `value_row`, `toggle_row`, `text_row`,
//! `danger_row`, `confirm_row`), never laid out by hand in a page.
//! API: `docs/group-chat-redesign.md` § "Shared profile pane".

use gpui::{div, prelude::*, px, AnyElement, Context, Div, Hsla, SharedString, Stateful};

use crate::app::{AppState, ProfilePage};
use crate::ui::glass;
use crate::ui::icons::icon;
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, TEXT_BODY};

use super::{hero, kit, media_grid, pushed_frame, tab_bar, Found, HERO_AVATAR};

/// Everything a page declares. The component renders it.
pub struct ProfilePane {
    /// Identity: the navigation stack, [`is_open`], element ids.
    pub page: ProfilePage,
    pub hero: Hero,
    /// Same labels and order as the iPhone page; zero or one tab shows no pill.
    pub tabs: &'static [&'static str],
    /// `state.profile_tab` (the stack restores it when a pushed page pops).
    pub selected: usize,
    /// The selected tab's content, top to bottom.
    pub body: Vec<Section>,
    /// Pinned after the body: Block user, Leave group, Sign out…
    pub footer: Vec<Section>,
    /// Optional control beside the × (Done, Save).
    pub done: Option<Stateful<Div>>,
}

pub struct Hero {
    pub picture: Picture,
    /// `name(text)` for a read-only name, `editable_name(..)` when owned.
    pub name: AnyElement,
    /// Glyph after the name ("lock" on a private channel).
    pub badge: Option<&'static str>,
    pub subtitle: Option<SharedString>,
    pub subtitle_color: Hsla,
}

pub enum Picture {
    /// A photo, initials disc or leaf — `editable_picture(..)` when owned.
    Element(AnyElement),
    /// A glyph tile: "#" for channels, "archive"… (the channel page, bc-55453a88).
    Glyph(&'static str),
}

// Variants the person page does not use yet are consumed by the group, agent,
// server and channel migrations (D93); they stay part of the published API.
#[allow(dead_code)]
pub enum Section {
    /// Small caps title over a card ("Images · 4", "Archived").
    Title(SharedString),
    /// Rows from `kit`; separators sit between rows only (`kit::section`).
    Card(Vec<AnyElement>),
    /// Media thumbnails, three square tiles per row.
    Grid(Vec<Found>),
    /// Centred muted line for an empty tab.
    Empty(SharedString),
    Footnote(SharedString),
    /// Escape hatch — say why on the component's issue.
    Custom(AnyElement),
}

/// The read-only hero name.
#[allow(dead_code)]
pub fn name(text: impl Into<SharedString>, theme: &Theme) -> AnyElement {
    div()
        .text_size(px(22.0))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme.text)
        .truncate()
        .child(text.into())
        .into_any_element()
}

/// The page's frame title, for the pushed bar.
fn title_of(page: &ProfilePage, state: &AppState) -> String {
    match page {
        ProfilePage::User { handle, .. } => state
            .person_by_handle(handle)
            .map(|p| p.display_name())
            .unwrap_or_else(|| format!("@{handle}")),
        ProfilePage::Group { space_id }
        | ProfilePage::Agent {
            project_id: space_id,
        } => state
            .projects
            .iter()
            .find(|p| &p.id == space_id)
            .map(|p| p.name.clone())
            .unwrap_or_default(),
        ProfilePage::Me => "You".to_string(),
        ProfilePage::Server { server_id } => state
            .servers
            .iter()
            .find(|s| &s.id == server_id)
            .map(|s| s.name.clone())
            .unwrap_or_default(),
        ProfilePage::Channel {
            server_id,
            channel_id,
        } => state
            .server(server_id)
            .and_then(|s| s.channel(channel_id))
            .map(|c| c.name.clone())
            .unwrap_or_default(),
    }
}

/// Is this page the one open in the right pane (the header pill / server
/// header ring)?
pub fn is_open(state: &AppState, page: &ProfilePage) -> bool {
    state.profile.as_ref() == Some(page)
}

fn glyph_tile(glyph: &'static str, theme: &Theme) -> AnyElement {
    div()
        .size(px(HERO_AVATAR))
        .rounded(px(HERO_AVATAR * 0.28))
        .bg(theme.surface_2)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(glyph, HERO_AVATAR * 0.45, theme.text))
        .into_any_element()
}

fn section(part: Section, theme: &Theme) -> AnyElement {
    match part {
        Section::Title(t) => kit::section_title(t, theme).into_any_element(),
        Section::Card(rows) => kit::section(rows, kit::INSET, theme).into_any_element(),
        Section::Grid(items) => media_grid(&items, theme).into_any_element(),
        Section::Empty(t) => kit::empty(&t, theme).into_any_element(),
        Section::Footnote(t) => kit::footnote(t, theme).into_any_element(),
        Section::Custom(el) => el,
    }
}

/// Hero + pill + sections inside the frame (×, ‹ when the stack is non-empty,
/// Done, the shared scroll handle, the opaque top bar).
pub fn render(pane: ProfilePane, shell: &Shell, cx: &mut Context<Shell>) -> Stateful<Div> {
    let theme = Theme::of(cx).clone();
    let title = title_of(&pane.page, shell.state.read(cx));
    let picture = match pane.hero.picture {
        Picture::Element(el) => el,
        Picture::Glyph(glyph) => glyph_tile(glyph, &theme),
    };
    let name_el = match pane.hero.badge {
        Some(glyph) => div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(pane.hero.name)
            .child(icon(glyph, 14.0, theme.text_muted))
            .into_any_element(),
        None => pane.hero.name,
    };
    let subtitle = pane.hero.subtitle.unwrap_or_default();
    // Hero + tab pill pin under the top bar; the sections scroll beneath them
    // and fade out at the join (Jacob, Oct 22: sticky header, no hard cut).
    let mut pinned = div().flex().flex_col().child(hero(
        picture,
        name_el,
        &subtitle,
        pane.hero.subtitle_color,
        &theme,
    ));
    if pane.tabs.len() > 1 {
        pinned = pinned.child(tab_bar(pane.tabs, pane.selected, &theme, cx));
    }
    let mut body = div().flex().flex_col().pb(px(16.0));
    for part in pane.body {
        body = body.child(section(part, &theme));
    }
    if !pane.footer.is_empty() {
        body = body.child(div().h(px(14.0)));
        for part in pane.footer {
            body = body.child(section(part, &theme));
        }
    }
    let _ = TEXT_BODY;
    let _ = glass::group;
    pushed_frame(
        &title,
        Some(pinned),
        body,
        pane.done,
        Some(&shell.profile_scroll),
        shell,
        &theme,
        cx,
    )
}
