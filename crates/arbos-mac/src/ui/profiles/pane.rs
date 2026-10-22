//! The one profile pane (D93, Jacob 2026-10-22): people, own profile, agents,
//! groups, servers and channels are all rendered by [`render`] from a
//! [`ProfilePane`] spec. The spec says *which* tabs exist and *which* rows sit
//! under each; this module owns the look — hero, tab pill, cards, spacing,
//! footer, the frame (×, ‹ back from the stack, Done) — so every entity looks
//! the same and the iPhone's `ProfilePane` mirrors it one to one. Rows come
//! from `kit` (`row`, `nav_row`, `value_row`, `toggle_row`, `text_row`,
//! `danger_row`, `confirm_row`), never laid out by hand in a page.
//! API: `docs/group-chat-redesign.md` § "Shared profile pane".

use gpui::{
    div, prelude::*, px, AnyElement, Bounds, Context, Div, Hsla, Pixels, SharedString, Stateful,
};

use crate::app::{AppState, ProfilePage};
use crate::ui::icons::icon;
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, TEXT_TINY};

use super::{
    file_list, hero, kit, link_list, media_grid, pushed_frame, tab_bar, Found, Link, HERO_AVATAR,
};

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
    /// Mac only: inline controls under the subtitle while the name is being
    /// edited (the own profile's handle field with Cancel / Save). The iPhone
    /// edits in a sheet, so its hero has no twin field.
    pub editor: Option<AnyElement>,
}

pub enum Picture {
    /// A photo, initials disc or leaf — `editable_picture(..)` when owned.
    Element(AnyElement),
    /// A glyph tile: "#" for channels, "archive"… (the channel page, bc-55453a88).
    Glyph(&'static str),
}

// Every variant is part of the published API; a page uses the ones its tabs
// need (D93).
#[allow(dead_code)]
pub enum Section {
    /// Small caps title over a card ("Images · 4", "Archived").
    Title(SharedString),
    /// Rows from `kit`; separators sit between rows only (`kit::section`).
    Card(Vec<AnyElement>),
    /// A card whose row `open` has just unfolded a picker under it (Model,
    /// Reasoning): the pane scrolls so that row is in view, once.
    Reveal {
        rows: Vec<AnyElement>,
        open: usize,
    },
    /// Media thumbnails, three square tiles per row.
    Grid(Vec<Found>),
    /// The Library's file rows: name, then "212 kB · Mira · Today".
    Files(Vec<Found>),
    /// The Library's link rows with favicons.
    Links(Vec<Link>),
    /// A picker card: a `kit` row that opens and closes it, the swatch grid
    /// under it (leaves, colours), a small note under the grid.
    Picker {
        header: AnyElement,
        swatches: AnyElement,
        note: SharedString,
    },
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

/// Which child of `kit::section` is row `open` (separators sit between rows).
fn child_index_of_row(rows: usize, open: usize) -> Option<usize> {
    kit::section_parts(rows)
        .iter()
        .position(|p| *p == kit::SectionPart::Row(open))
}

fn section(part: Section, shell: &Shell, theme: &Theme, cx: &mut Context<Shell>) -> AnyElement {
    match part {
        Section::Title(t) => kit::section_title(t, theme).into_any_element(),
        Section::Card(rows) => kit::section(rows, kit::INSET, theme).into_any_element(),
        Section::Reveal { rows, open } => {
            let child = child_index_of_row(rows.len(), open);
            let card = kit::section(rows, kit::INSET, theme);
            match child {
                Some(ix) => {
                    let reveal =
                        cx.listener(move |this, bounds: &Vec<Bounds<Pixels>>, window, cx| {
                            this.reveal_pane_row(bounds.get(ix), window, cx);
                        });
                    card.on_children_prepainted(move |bounds, window, cx| {
                        reveal(&bounds, window, cx)
                    })
                    .into_any_element()
                }
                None => card.into_any_element(),
            }
        }
        Section::Grid(items) => media_grid(&items, theme).into_any_element(),
        Section::Files(items) => file_list(&items, "doc", "", theme).into_any_element(),
        Section::Links(items) => link_list(&items, shell, theme, cx).into_any_element(),
        Section::Picker {
            header,
            swatches,
            note,
        } => kit::card(theme)
            .child(header)
            .child(div().pt(px(14.0)).child(swatches))
            .child(
                div()
                    .px(px(kit::INSET))
                    .pb(px(12.0))
                    .text_size(px(TEXT_TINY))
                    .text_color(theme.text_muted)
                    .child(note),
            )
            .into_any_element(),
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
    let mut pinned = div().flex().flex_col().child(
        hero(
            picture,
            name_el,
            &subtitle,
            pane.hero.subtitle_color,
            &theme,
        )
        .children(pane.hero.editor),
    );
    if pane.tabs.len() > 1 {
        pinned = pinned.child(tab_bar(pane.tabs, pane.selected, &theme, cx));
    }
    let mut body = div().flex().flex_col().pb(px(16.0));
    for part in pane.body {
        body = body.child(section(part, shell, &theme, cx));
    }
    if !pane.footer.is_empty() {
        body = body.child(div().h(px(14.0)));
        for part in pane.footer {
            body = body.child(section(part, shell, &theme, cx));
        }
    }
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

#[cfg(test)]
mod tests {
    use super::child_index_of_row;

    /// `kit::section` interleaves a separator between rows, so row `i` is
    /// child `2i`; a row past the end is no child at all.
    #[test]
    fn reveal_row_maps_to_section_child() {
        assert_eq!(child_index_of_row(2, 0), Some(0));
        assert_eq!(child_index_of_row(2, 1), Some(2));
        assert_eq!(child_index_of_row(3, 2), Some(4));
        assert_eq!(child_index_of_row(2, 2), None);
        assert_eq!(child_index_of_row(0, 0), None);
    }
}
