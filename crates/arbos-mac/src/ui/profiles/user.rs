//! A person's page: avatar, name, presence, call / mute / search /
//! more, the info card (@handle, Send Message, Block), and the Links /
//! Groups tabs built from the chats and projects we share with them.

use gpui::{div, prelude::*, px, ClipboardItem, Context, Div, ElementId, Focusable, SharedString};

use crate::app::{CallTarget, ProfilePage};
use crate::model::{Person, Project};
use crate::ui::avatars;
use crate::ui::glass;
use crate::ui::icons::icon;
use crate::ui::people;
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, TEXT_SMALL};

use super::{
    action_button, action_row, card, chats_of, collect, empty, frame, hero, hero_name, info_action,
    info_row, link_list, list_row, more_menu, tab_bar, Action,
};

const TABS: [&str; 2] = ["Links", "Groups"];

pub(super) fn page(
    handle: &str,
    space_id: Option<&str>,
    shell: &Shell,
    cx: &mut Context<Shell>,
) -> Option<Div> {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let person: Person = state.person_by_handle(handle).unwrap_or_else(|| Person {
        handle: handle.to_string(),
        ..Default::default()
    });
    let me = state.me_handle() == person.handle;
    let image = state.avatar(&person.avatar_url);
    let status = if me {
        "that's you".to_string()
    } else {
        state.presence_text(&person)
    };
    let online = state.is_online(&person);
    let tab = state.profile_tab;
    let more_open = state.profile_more_open;

    // Spaces we share: the DM, group chats, shared projects.
    let shared: Vec<Project> = state
        .projects
        .iter()
        .filter(|p| p.people.iter().any(|x| x.handle == person.handle))
        .cloned()
        .collect();
    let dm = shared
        .iter()
        .find(|p| p.person().is_some_and(|x| x.handle == person.handle) && !p.is_people_group());
    // Mute applies to the DM when there is one, else to the chat this opened from.
    let mute_space = dm
        .map(|p| p.id.clone())
        .or_else(|| space_id.map(str::to_string));
    let muted = mute_space.as_deref().is_some_and(|id| state.is_muted(id));
    // Group tab: everything but the DM.
    let groups: Vec<&Project> = shared
        .iter()
        .filter(|p| !(p.kind == crate::model::SpaceKind::Dm))
        .collect();
    let group_images: Vec<Vec<(Person, Option<std::sync::Arc<gpui::RenderImage>>)>> = groups
        .iter()
        .map(|p| {
            p.people
                .iter()
                .take(3)
                .map(|x| (x.clone(), state.avatar(&x.avatar_url)))
                .collect()
        })
        .collect();

    // Links this person shared, across every chat we hold.
    let all_chats: Vec<&crate::model::Chat> =
        shared.iter().flat_map(|p| chats_of(state, &p.id)).collect();
    let content = collect(state, &all_chats);
    let any_chats = !all_chats.is_empty();
    drop(all_chats);
    let by = format!("@{}", person.handle);
    let links: Vec<super::Link> = content.links.into_iter().filter(|l| l.by == by).collect();

    let can_remove = space_id
        .and_then(|id| state.project(id))
        .filter(|p| p.is_people_group() || (!p.is_people() && !p.is_guest()))
        .map(|p| (p.id.clone(), p.is_people()))
        .filter(|_| !me);
    let handle_s = person.handle.clone();
    let user_id = person.id.clone();

    // ----- hero ----------------------------------------------------------------
    let avatar = div()
        .relative()
        .size(px(112.0))
        .child(people::avatar(image, &person, 112.0, &theme))
        .when(online, |el| {
            el.child(
                div()
                    .absolute()
                    .right(px(4.0))
                    .bottom(px(4.0))
                    .size(px(22.0))
                    .rounded_full()
                    .bg(theme.card())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().size(px(14.0)).rounded_full().bg(theme.online)),
            )
        })
        .into_any_element();
    let hero = hero(
        avatar,
        hero_name(&person.display_name(), &theme).into_any_element(),
        &status,
        if online {
            super::presence_blue()
        } else {
            theme.text_muted
        },
        &theme,
    );

    // ----- actions --------------------------------------------------------------
    let h1 = handle_s.clone();
    let mute_target = mute_space.clone();
    let h3 = handle_s.clone();
    // Calls start audio-only; camera is toggled on the call screen (same as iPhone).
    let actions = action_row(vec![
        action_button(&Action::new("profile-call", "phone", "call"), &theme).on_click(cx.listener(
            move |this, _, _, cx| {
                let h = h1.clone();
                this.state
                    .update(cx, |s, cx| s.start_call(CallTarget::User(h), false, cx));
            },
        )),
        {
            let a = Action::new(
                "profile-mute",
                if muted { "bell-slash" } else { "bell" },
                if muted { "unmute" } else { "mute" },
            );
            let mut b = action_button(&a, &theme);
            if mute_target.is_none() {
                b = b.opacity(0.5);
            }
            b.on_click(cx.listener(move |this, _, _, cx| {
                if let Some(id) = mute_target.clone() {
                    this.state.update(cx, |s, cx| s.toggle_mute(id, cx));
                }
            }))
        },
        action_button(&Action::new("profile-search", "search", "search"), &theme).on_click(
            cx.listener(move |this, _, window, cx| {
                let q = format!("@{h3}");
                this.state.update(cx, |s, cx| {
                    s.close_profile(cx);
                });
                this.search_input
                    .update(cx, |input, cx| input.set_text(q, cx));
                let handle = this.search_input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }),
        ),
        action_button(&Action::new("profile-more", "ellipsis", "more"), &theme).on_click(
            cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_profile_more(cx));
            }),
        ),
    ]);

    let more = more_open.then(|| {
        let h = handle_s.clone();
        let h_gh = handle_s.clone();
        let mut rows = vec![
            glass::menu_row("profile-more-copy", "copy", "Copy @handle", false, &theme).on_click(
                cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(format!("@{h}")));
                    this.state.update(cx, |s, cx| {
                        s.push_notice("Copied");
                        s.toggle_profile_more(cx);
                    });
                }),
            ),
            glass::menu_row(
                "profile-more-github",
                "globe",
                "Open on GitHub",
                false,
                &theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.open_url(&format!("https://github.com/{h_gh}"));
                this.state.update(cx, |s, cx| s.toggle_profile_more(cx));
            })),
        ];
        if let Some((space, is_people)) = can_remove.clone() {
            let uid = user_id.clone();
            rows.push(
                glass::menu_row(
                    "profile-more-remove",
                    "xmark",
                    "Remove from chat",
                    true,
                    &theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (space, uid) = (space.clone(), uid.clone());
                    this.state.update(cx, |s, cx| {
                        if is_people {
                            s.remove_member(space, uid, cx);
                        } else {
                            s.remove_project_member(space, uid, cx);
                        }
                        s.close_profile(cx);
                    });
                })),
            );
        }
        more_menu(rows, &theme)
    });

    // ----- info card -------------------------------------------------------------
    let h_copy = handle_s.clone();
    let h_dm = handle_s.clone();
    let h_block = handle_s.clone();
    let mut info = card(&theme).child(
        div()
            .id("profile-handle")
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(info_row("username", format!("@{}", person.handle), &theme))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(format!("@{h_copy}")));
                this.state.update(cx, |s, cx| {
                    s.push_notice("Copied");
                    cx.notify();
                });
            })),
    );
    if !me {
        info = info
            .child(
                info_action("profile-dm", "Send Message", theme.primary, None, &theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        let h = h_dm.clone();
                        this.state.update(cx, |s, cx| {
                            s.close_profile(cx);
                            s.start_dm(h, cx);
                        });
                    }),
                ),
            )
            .child(
                info_action("profile-block", "Block User", theme.danger, None, &theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        let h = h_block.clone();
                        this.state.update(cx, |s, cx| {
                            s.block_user(h, cx);
                            s.close_profile(cx);
                        });
                    }),
                ),
            );
    }

    // ----- tabs -----------------------------------------------------------------
    let tabs = tab_bar(&TABS, tab, &theme, cx);
    let body_tab = match tab {
        0 => link_list(&links, &theme, cx),
        _ => {
            if groups.is_empty() {
                empty("No groups in common yet", &theme)
            } else {
                let mut list = card(&theme);
                for (i, p) in groups.iter().enumerate() {
                    let leading = if p.is_people() {
                        people::avatar_stack(&group_images[i], 36.0, 3, &theme).into_any_element()
                    } else {
                        avatars::project(p, 40.0, &theme).into_any_element()
                    };
                    let sub = if p.is_people() {
                        format!("{} members", p.people.len() + 1)
                    } else {
                        format!("shared project · {} people", p.people.len() + 1)
                    };
                    let pid = p.id.clone();
                    list = list.child(
                        list_row(
                            ElementId::Name(SharedString::from(format!("profile-group-{}", p.id))),
                            leading,
                            p.name.clone(),
                            sub,
                            theme.text_muted,
                            Some(icon("chevron-right", 13.0, theme.text_faint).into_any_element()),
                            &theme,
                        )
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_2))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let pid = pid.clone();
                            this.state.update(cx, |s, cx| {
                                s.select_project(pid.clone(), cx);
                                s.open_profile(ProfilePage::Group { space_id: pid }, cx);
                            });
                        })),
                    );
                }
                list
            }
        }
    };

    let body = div()
        .flex()
        .flex_col()
        .pb(px(16.0))
        .child(hero)
        .child(actions)
        .children(more)
        .child(info)
        .child(tabs)
        .child(body_tab)
        .when(links.is_empty() && tab == 0 && any_chats, |el| {
            el.child(
                div()
                    .px(px(16.0))
                    .text_size(px(TEXT_SMALL - 1.0))
                    .text_color(theme.text_faint)
                    .child("Links appear here as chats load."),
            )
        });
    Some(frame(body, None, None, &theme, cx))
}
