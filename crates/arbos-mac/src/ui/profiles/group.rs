//! A group's pane — a people group chat or a project shared with other
//! humans (redesign 2026-10-20, mockups b–d): big picture, name, two tabs.
//! **Members** starts with "Add members" (people and agents alike), then
//! name + handle rows. **Library** is every image, then every file shared
//! here. The picture takes a crayon on hover, the name an inline field; no
//! actions row, no counts, no presence, no badges.

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, Context, Div, ElementId, Focusable, RenderImage, SharedString, Stateful,
};

use crate::app::{AppState, ProfilePage};
use crate::model::{Person, Project};
use crate::ui::avatars;
use crate::ui::groups;
use crate::ui::icons::icon;
use crate::ui::people;
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, TEXT_BODY, TEXT_SMALL};

use super::{
    card, chats_of, collect, editable_name, editable_picture, empty, file_list, frame, hero,
    list_row, media_grid, tab_bar, HeroClick, HERO_AVATAR,
};

const TABS: [&str; 2] = ["Members", "Library"];

/// Everyone in the space, me first, then the others by handle.
fn members(state: &AppState, space: &Project) -> Vec<Person> {
    let mut out: Vec<Person> = Vec::new();
    let me_handle = state.me_handle();
    let me_id = state.me_id();
    if !me_handle.is_empty() {
        out.push(Person {
            id: me_id,
            handle: me_handle.clone(),
            name: state
                .account
                .as_ref()
                .map(|a| a.name.clone())
                .filter(|n| !n.trim().is_empty()),
            avatar_url: state
                .account
                .as_ref()
                .map(|a| a.avatar_url.clone())
                .unwrap_or_default(),
            status: "member".into(),
            role: "member".into(),
            last_seen_at: None,
            online: true,
            avatar_color_index: None,
        });
    }
    let mut others: Vec<Person> = space
        .people
        .iter()
        .filter(|p| p.handle != me_handle)
        .cloned()
        .collect();
    others.sort_by_key(|p| p.handle.clone());
    out.extend(others);
    out
}

pub(super) fn page(
    space_id: &str,
    shell: &Shell,
    cx: &mut Context<Shell>,
) -> Option<Stateful<Div>> {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let space = state.project(space_id)?.clone();
    let tab = state.profile_tab;
    let me_handle = state.me_handle();
    let people = members(state, &space);
    let images: Vec<Option<Arc<RenderImage>>> =
        people.iter().map(|p| state.avatar(&p.avatar_url)).collect();
    let is_people = space.is_people();
    let renaming = state.profile_editing_name && is_people;
    // A people group: any member names it or gives it a picture (relay
    // `PATCH /chats/{id}`); a shared project keeps its owner's name.
    let editable = space.is_people_group();
    let content = collect(state, &chats_of(state, &space.id));

    // ----- hero: picture with the crayon, name as an inline field ----------------
    let picture = groups::picture(&space, state, HERO_AVATAR, &theme).into_any_element();
    let sid_pic = space.id.clone();
    let pic_click = editable.then(|| {
        let f = cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
            this.pick_group_image(sid_pic.clone(), cx);
        });
        Box::new(f) as HeroClick
    });
    let avatar = editable_picture("profile-avatar", picture, pic_click).into_any_element();
    let sid_name = space.id.clone();
    let name_click = editable.then(|| {
        let f = cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
            this.start_rename_group(sid_name.clone(), window, cx);
        });
        Box::new(f) as HeroClick
    });
    let name = editable_name(
        "profile-name",
        &space.name,
        renaming,
        &shell.agent_name_input,
        name_click,
        &theme,
    )
    .into_any_element();
    let hero = hero(avatar, name, "", theme.text_muted, &theme);

    // ----- tabs -----------------------------------------------------------------
    let tabs = tab_bar(&TABS, tab, &theme, cx);
    let body_tab = match tab {
        0 => {
            let mut list = card(&theme);
            // Add members: people and agents through the one picker.
            let sid = space.id.clone();
            let i_own = !space.is_guest();
            if is_people || i_own {
                list = list.child(
                    div()
                        .id("profile-add-members")
                        .flex()
                        .items_center()
                        .gap(px(12.0))
                        .min_h(px(52.0))
                        .px(px(14.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .text_size(px(TEXT_BODY))
                        .text_color(theme.text)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_2))
                        .child(
                            div()
                                .size(px(36.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon("person-plus", 19.0, theme.text_muted)),
                        )
                        .child("Add members")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let id = sid.clone();
                            this.state.update(cx, |s, cx| {
                                if is_people {
                                    s.select_project(id, cx);
                                } else {
                                    s.invite_target = Some(id);
                                }
                                s.open_new_chat(cx);
                            });
                            let handle = this.people_input.read(cx).focus_handle(cx);
                            window.focus(&handle, cx);
                        })),
                );
            }
            for (i, p) in people.iter().enumerate() {
                let is_me = p.handle == me_handle;
                let title = if is_me {
                    format!("{} (you)", p.display_name())
                } else {
                    p.display_name()
                };
                let handle = p.handle.clone();
                let sid = space.id.clone();
                list = list.child(
                    list_row(
                        ElementId::Name(SharedString::from(format!("profile-member-{}", p.handle))),
                        people::avatar(images[i].clone(), p, 36.0, &theme).into_any_element(),
                        title,
                        format!("@{}", p.handle),
                        theme.text_muted,
                        None,
                        &theme,
                    )
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (handle, sid) = (handle.clone(), sid.clone());
                        this.state.update(cx, |s, cx| {
                            s.open_profile(
                                ProfilePage::User {
                                    handle,
                                    space_id: Some(sid),
                                },
                                cx,
                            )
                        });
                    })),
                );
            }
            // Agents in the chat (people groups) — or the project's own agent.
            if is_people {
                for a in space.agents.iter() {
                    let pid = a.project_id.clone();
                    let sub = if a.owner_handle == me_handle {
                        "Agent · run by you".to_string()
                    } else {
                        format!("Agent · run by @{}", a.owner_handle)
                    };
                    list = list.child(
                        list_row(
                            ElementId::Name(SharedString::from(format!(
                                "profile-agent-{}",
                                a.project_id
                            ))),
                            avatars::agent_member(a, 36.0, &theme).into_any_element(),
                            a.name.clone(),
                            sub,
                            theme.text_muted,
                            None,
                            &theme,
                        )
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_2))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let pid = pid.clone();
                            this.state.update(cx, |s, cx| {
                                s.open_profile(ProfilePage::Agent { project_id: pid }, cx)
                            });
                        })),
                    );
                }
            } else {
                let pid = space.id.clone();
                let sub = if space.is_guest() {
                    format!(
                        "Agent · run by @{}",
                        space.owner_handle.clone().unwrap_or_default()
                    )
                } else {
                    "Agent · run by you".to_string()
                };
                list = list.child(
                    list_row(
                        "profile-agent-self",
                        avatars::project(&space, 36.0, &theme).into_any_element(),
                        space.name.clone(),
                        sub,
                        theme.text_muted,
                        None,
                        &theme,
                    )
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let pid = pid.clone();
                        this.state.update(cx, |s, cx| {
                            s.open_profile(ProfilePage::Agent { project_id: pid }, cx)
                        });
                    })),
                );
            }
            list
        }
        _ => {
            // Library: images first, then files — one place for everything shared.
            let mut lib = div().flex().flex_col();
            if content.media.is_empty() && content.files.is_empty() {
                lib = lib.child(empty("Nothing shared here yet", &theme));
            } else {
                if !content.media.is_empty() {
                    lib = lib
                        .child(section_title(
                            &format!("Images · {}", content.media.len()),
                            &theme,
                        ))
                        .child(media_grid(&content.media, &theme));
                }
                if !content.files.is_empty() {
                    lib = lib
                        .child(section_title(
                            &format!("Files · {}", content.files.len()),
                            &theme,
                        ))
                        .child(file_list(&content.files, "doc", "", &theme));
                }
            }
            lib
        }
    };

    let body = div()
        .flex()
        .flex_col()
        .pb(px(16.0))
        .child(hero)
        .child(tabs)
        .child(body_tab);
    Some(frame(body, None, None, &theme, cx))
}

/// "Images · 23" above a Library section.
fn section_title(text: &str, theme: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .px(px(16.0))
        .pt(px(4.0))
        .pb(px(8.0))
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text_muted)
        .child(text.to_string())
}
