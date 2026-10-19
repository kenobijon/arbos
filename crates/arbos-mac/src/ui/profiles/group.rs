//! A group's page — a people group chat or a project shared with other
//! humans: avatar, name, "N members", mute / search / leave / more, then
//! Members (with Add Members, presence, owner badges) / Media / Files / Voice
//! / Links from the chat docs.

use std::sync::Arc;

use gpui::{div, prelude::*, px, Context, Div, ElementId, Focusable, RenderImage, SharedString};

use crate::app::{AppState, ProfilePage};
use crate::model::{Person, Project};
use crate::ui::avatars;
use crate::ui::glass;
use crate::ui::icons::icon;
use crate::ui::people;
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, TEXT_BODY};

use super::{
    action_button, action_row, badge, card, chats_of, collect, empty, file_list, frame, hero,
    hero_name, link_list, list_row, media_grid, more_menu, tab_bar, Action,
};

const TABS: [&str; 5] = ["Members", "Media", "Files", "Voice", "Links"];

/// Everyone in the space, me included, owner first.
fn members(state: &AppState, space: &Project) -> Vec<Person> {
    let mut out: Vec<Person> = Vec::new();
    let me_handle = state.me_handle();
    let me_id = state.me_id();
    let me = (!me_handle.is_empty()).then(|| Person {
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
        // In my own project I am the owner; in a people chat the relay's
        // member list says (it omits me, so fall back on the space).
        role: if space.is_people() {
            if space.owner_handle.as_deref().is_none_or(|o| o == me_handle) {
                "owner".into()
            } else {
                "member".into()
            }
        } else if space.shared {
            "member".into()
        } else {
            "owner".into()
        },
        last_seen_at: None,
        online: true,
        avatar_color_index: None,
    });
    out.extend(space.people.iter().cloned());
    if let Some(me) = me {
        if !out.iter().any(|p| p.handle == me_handle) {
            out.push(me);
        }
    }
    out.sort_by_key(|p| (p.role != "owner", !p.online, p.handle.clone()));
    out
}

pub(super) fn page(space_id: &str, shell: &Shell, cx: &mut Context<Shell>) -> Option<Div> {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let space = state.project(space_id)?.clone();
    let tab = state.profile_tab;
    let more_open = state.profile_more_open;
    let muted = state.is_muted(&space.id);
    let me_handle = state.me_handle();
    let people = members(state, &space);
    let images: Vec<Option<Arc<RenderImage>>> =
        people.iter().map(|p| state.avatar(&p.avatar_url)).collect();
    let statuses: Vec<(String, bool)> = people
        .iter()
        .map(|p| (state.presence_text(p), state.is_online(p)))
        .collect();
    let i_own = people
        .iter()
        .any(|p| p.handle == me_handle && p.role == "owner");
    let human_count = people.len();
    let agent_count = if space.is_people() {
        space.agents.len()
    } else {
        1
    };
    let count_text = format!(
        "{human_count} member{}{}",
        if human_count == 1 { "" } else { "s" },
        match agent_count {
            0 => String::new(),
            1 => " · 1 agent".to_string(),
            n => format!(" · {n} agents"),
        }
    );
    let content = collect(state, &chats_of(state, &space.id));

    // ----- hero: stacked people for a group, the tree for a shared project ----
    let avatar = if space.is_people() {
        let stack: Vec<(Person, Option<Arc<RenderImage>>)> = people
            .iter()
            .cloned()
            .zip(images.iter().cloned())
            .take(3)
            .collect();
        div()
            .size(px(112.0))
            .flex()
            .items_center()
            .justify_center()
            .child(people::avatar_stack(&stack, 72.0, 3, &theme))
            .into_any_element()
    } else {
        avatars::project(&space, 112.0, &theme).into_any_element()
    };
    let hero = hero(
        avatar,
        hero_name(&space.name, &theme).into_any_element(),
        &count_text,
        theme.text_muted,
        &theme,
    );

    // ----- actions --------------------------------------------------------------
    let sid = space.id.clone();
    let sid_leave = space.id.clone();
    let name_q = space.name.clone();
    let is_people = space.is_people();
    let can_leave = is_people || space.is_guest();
    let actions = action_row(vec![
        action_button(
            &Action::new(
                "profile-mute",
                if muted { "bell-slash" } else { "bell" },
                if muted { "unmute" } else { "mute" },
            ),
            &theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let id = sid.clone();
            this.state.update(cx, |s, cx| s.toggle_mute(id, cx));
        })),
        action_button(&Action::new("profile-search", "search", "search"), &theme).on_click(
            cx.listener(move |this, _, window, cx| {
                let q = name_q.clone();
                this.state.update(cx, |s, cx| {
                    s.close_profile(cx);
                });
                this.search_input
                    .update(cx, |input, cx| input.set_text(q, cx));
                let handle = this.search_input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }),
        ),
        {
            let mut a = Action::new("profile-leave", "leave", "leave");
            a.enabled = can_leave;
            action_button(&a, &theme).on_click(cx.listener(move |this, _, _, cx| {
                if !can_leave {
                    return;
                }
                let id = sid_leave.clone();
                this.state.update(cx, |s, cx| {
                    s.close_profile(cx);
                    if is_people {
                        s.leave_chat(id, cx);
                    } else {
                        let me = s.me_id();
                        s.remove_project_member(id, me, cx);
                    }
                });
            }))
        },
        action_button(&Action::new("profile-more", "ellipsis", "more"), &theme).on_click(
            cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_profile_more(cx));
            }),
        ),
    ]);

    let more = more_open.then(|| {
        let sid = space.id.clone();
        let mut rows = vec![glass::menu_row(
            "profile-more-open",
            "chevron-right",
            "Open chat",
            false,
            &theme,
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            let id = sid.clone();
            this.state.update(cx, |s, cx| {
                s.close_profile(cx);
                s.select_project(id, cx);
            });
            crate::ui::composer::Composer::focus(&this.composer, window, cx);
        }))];
        if !space.is_people() {
            let pid = space.id.clone();
            rows.push(
                glass::menu_row(
                    "profile-more-agent",
                    "sparkles",
                    "Agent profile",
                    false,
                    &theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let pid = pid.clone();
                    this.state.update(cx, |s, cx| {
                        s.open_profile(ProfilePage::Agent { project_id: pid }, cx)
                    });
                })),
            );
        }
        more_menu(rows, &theme)
    });

    // ----- tabs -----------------------------------------------------------------
    let tabs = tab_bar(&TABS, tab, &theme, cx);
    let body_tab = match tab {
        0 => {
            let mut list = card(&theme);
            // Add Members: the invite flow (people chat: add person; project: invite guest).
            let sid = space.id.clone();
            let add_label = if is_people {
                "Add Members"
            } else {
                "Invite to project"
            };
            let can_add = i_own || is_people;
            if can_add {
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
                        .text_color(theme.primary)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_2))
                        .child(
                            div()
                                .size(px(40.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon("person-plus", 20.0, theme.primary)),
                        )
                        .child(add_label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let id = sid.clone();
                            this.state.update(cx, |s, cx| {
                                s.close_profile(cx);
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
                let (status, online) = statuses[i].clone();
                let is_me = p.handle == me_handle;
                let trailing = (p.role == "owner")
                    .then(|| badge("owner", super::owner_purple()).into_any_element());
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
                        people::avatar(images[i].clone(), p, 40.0, &theme).into_any_element(),
                        title,
                        status,
                        if online {
                            super::presence_blue()
                        } else {
                            theme.text_muted
                        },
                        trailing,
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
                    let sub = format!("agent · owned by @{}", a.owner_handle);
                    list = list.child(
                        list_row(
                            ElementId::Name(SharedString::from(format!(
                                "profile-agent-{}",
                                a.project_id
                            ))),
                            avatars::agent_member(a, 40.0, &theme).into_any_element(),
                            a.name.clone(),
                            sub,
                            theme.text_muted,
                            Some(badge("agent", theme.text_muted).into_any_element()),
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
                list = list.child(
                    list_row(
                        "profile-agent-self",
                        avatars::project(&space, 40.0, &theme).into_any_element(),
                        space.name.clone(),
                        "the project's agent",
                        theme.text_muted,
                        Some(badge("agent", theme.text_muted).into_any_element()),
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
        1 => media_grid(&content.media, &theme),
        2 => file_list(&content.files, "doc", "No files shared yet", &theme),
        3 => file_list(&content.voice, "waveform", "No voice messages yet", &theme),
        _ => link_list(&content.links, &theme, cx),
    };
    let _ = empty;

    let body = div()
        .flex()
        .flex_col()
        .pb(px(16.0))
        .child(hero)
        .child(actions)
        .children(more)
        .child(tabs)
        .child(body_tab);
    Some(frame(body, None, &theme, cx))
}
