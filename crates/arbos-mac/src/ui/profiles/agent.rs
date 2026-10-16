//! An agent's page: the coloured tree, name, what it is doing, call / voice
//! (soon) / mute / search / more, the info card (owner, machine, model,
//! description), an Appearance section holding the tree + colour picker,
//! then Media / Files / Links from its chats.

use gpui::{div, prelude::*, px, Context, Div, ElementId, Focusable, FontWeight, SharedString};

use crate::app::{CallTarget, ProfilePage};
use crate::model::{Id, MachineState, Person, Project};
use crate::ui::avatars::{self, AvatarState};
use crate::ui::glass;
use crate::ui::groups;
use crate::ui::icons::icon;
use crate::ui::motion::PulseClock;
use crate::ui::shell::Shell;
use crate::ui::sidebar::machine_dot;
use crate::ui::theme::{Theme, PANEL_RADIUS, TEXT_BODY, TEXT_SMALL, TEXT_TINY};

use super::{
    action_button, action_row, card, chats_of, collect, file_list, frame, hero, info_row,
    link_list, media_grid, more_menu, tab_bar, Action,
};

const TABS: [&str; 3] = ["Media", "Files", "Links"];

/// An agent that is only known as a member of a people chat (someone else's
/// project) is shown from that member entry.
fn project_from_member(state: &crate::app::AppState, project_id: &str) -> Option<Project> {
    state
        .projects
        .iter()
        .flat_map(|p| p.agents.iter())
        .find(|a| a.project_id == project_id)
        .map(|a| Project {
            id: a.project_id.clone(),
            name: a.name.clone(),
            icon: a.icon.clone(),
            color: a.color.clone(),
            avatar: a.avatar.clone(),
            avatar_color: a.avatar_color.clone(),
            description: a.description.clone(),
            owner_person: a.owner.clone(),
            model: a.model.clone().unwrap_or_default(),
            shared: true,
            owner_handle: Some(a.owner_handle.clone()),
            ..Default::default()
        })
}

pub(super) fn page(project_id: &str, shell: &Shell, cx: &mut Context<Shell>) -> Option<Div> {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let (project, mine): (Project, bool) = match state.project(project_id) {
        Some(p) => (p.clone(), !p.is_guest()),
        None => (project_from_member(state, project_id)?, false),
    };
    let current = state.avatar_slug(&project);
    let current_color = state.avatar_color(&project);
    let avatar_state = state.avatar_state(&project.id);
    let streaming = state.project_is_streaming(&project.id);
    let machine_state: MachineState = match &project.machine_info {
        Some(m) if !mine => match m.state.as_str() {
            "awake" => MachineState::Awake,
            "waking" => MachineState::Waking,
            "error" => MachineState::Error,
            _ => MachineState::Asleep,
        },
        _ => state.machine.shown_state(),
    };
    let always_on = project
        .machine_info
        .as_ref()
        .map(|m| m.always_on)
        .unwrap_or(state.machine.always_on);
    let machine_class = project
        .machine_info
        .as_ref()
        .map(|m| m.class.clone())
        .filter(|c| !c.is_empty() && c != "none")
        .or_else(|| state.machine.class.clone());
    let status = if streaming {
        "working".to_string()
    } else if always_on || machine_state == MachineState::Awake {
        "idle".to_string()
    } else {
        format!("machine {}", machine_state.label())
    };
    let status_color = if streaming {
        theme.online
    } else {
        theme.text_muted
    };
    let owner: Option<Person> = project
        .owner_person
        .clone()
        .or_else(|| project.owner().cloned());
    let owner_text = match (&owner, mine) {
        (Some(o), _) => format!(
            "@{}{}",
            o.handle,
            if state.is_online(o) { " · online" } else { "" }
        ),
        (None, true) => format!("@{} (you)", state.me_handle()),
        (None, false) => project
            .owner_handle
            .clone()
            .map(|h| format!("@{h}"))
            .unwrap_or_else(|| "—".into()),
    };
    let model_text = if project.model.is_empty() {
        "default".to_string()
    } else {
        project.model.clone()
    };
    let machine_text = {
        let mut s = machine_state.label().to_string();
        if always_on {
            s.push_str(" · always on");
        }
        if let Some(c) = &machine_class {
            s.push_str(&format!(" · {c}"));
        }
        s
    };
    let tab = state.profile_tab;
    let more_open = state.profile_more_open;
    let picker_open = state.profile_picker_open;
    let editing = state.profile_editing_description && mine;
    let muted = state.is_muted(&project.id);
    let content = collect(state, &chats_of(state, &project.id));
    let fallback = groups::project_color(&project.color, theme.accent);
    let pid: Id = project.id.clone();

    if avatar_state != AvatarState::Idle {
        PulseClock::request(cx);
    }

    // ----- hero ----------------------------------------------------------------
    let avatar = div()
        .relative()
        .size(px(112.0))
        .child(avatars::element(
            current.as_deref(),
            current_color.as_deref(),
            avatar_state,
            112.0,
            &project.icon,
            fallback,
            &theme,
        ))
        .child(
            div()
                .absolute()
                .right(px(0.0))
                .bottom(px(0.0))
                .size(px(30.0))
                .rounded_full()
                .bg(theme.card())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(20.0))
                        .rounded_full()
                        .bg(machine_dot(machine_state, &theme)),
                ),
        )
        .into_any_element();
    let hero = hero(avatar, &project.name, &status, status_color, &theme).child(
        div()
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_faint)
            .child(format!("@{}", Project::mention_slug(&project.name))),
    );

    // ----- actions --------------------------------------------------------------
    let p_call = pid.clone();
    let p_voice = pid.clone();
    let p_mute = pid.clone();
    let name_q = project.name.clone();
    let actions = action_row(vec![
        action_button(&Action::new("profile-call", "phone", "call"), &theme).on_click(cx.listener(
            move |this, _, _, cx| {
                let p = p_call.clone();
                this.state
                    .update(cx, |s, cx| s.start_call(CallTarget::Agent(p), false, cx));
            },
        )),
        action_button(&Action::new("profile-voice", "waveform", "voice"), &theme).on_click(
            cx.listener(move |this, _, _, cx| {
                let p = p_voice.clone();
                this.state
                    .update(cx, |s, cx| s.start_call(CallTarget::Agent(p), false, cx));
            }),
        ),
        action_button(
            &Action::new(
                "profile-mute",
                if muted { "bell-slash" } else { "bell" },
                if muted { "unmute" } else { "mute" },
            ),
            &theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let p = p_mute.clone();
            this.state.update(cx, |s, cx| s.toggle_mute(p, cx));
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
        action_button(&Action::new("profile-more", "ellipsis", "more"), &theme).on_click(
            cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_profile_more(cx));
            }),
        ),
    ]);

    let more = more_open.then(|| {
        let p_open = pid.clone();
        let mut rows = vec![glass::menu_row(
            "profile-more-open",
            "chevron-right",
            "Open chat",
            false,
            &theme,
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            let p = p_open.clone();
            this.state.update(cx, |s, cx| {
                s.close_profile(cx);
                s.select_project(p, cx);
            });
            crate::ui::composer::Composer::focus(&this.composer, window, cx);
        }))];
        if project.is_groupish() {
            let p_group = pid.clone();
            rows.push(
                glass::menu_row("profile-more-group", "people", "Members", false, &theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        let p = p_group.clone();
                        this.state.update(cx, |s, cx| {
                            s.open_profile(ProfilePage::Group { space_id: p }, cx)
                        });
                    }),
                ),
            );
        }
        if let Some(o) = owner.as_ref().filter(|_| !mine) {
            let h = o.handle.clone();
            rows.push(
                glass::menu_row(
                    "profile-more-owner",
                    "person",
                    "Owner's profile",
                    false,
                    &theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let h = h.clone();
                    this.state.update(cx, |s, cx| {
                        s.open_profile(
                            ProfilePage::User {
                                handle: h,
                                space_id: None,
                            },
                            cx,
                        )
                    });
                })),
            );
        }
        more_menu(rows, &theme)
    });

    // ----- info card -------------------------------------------------------------
    let mut info = card(&theme)
        .child(info_row("owner", owner_text, &theme))
        .child(info_row("machine", machine_text, &theme))
        .child(info_row("model", model_text, &theme));
    let description = project.description.clone().filter(|d| !d.trim().is_empty());
    if editing {
        // Inline editor: the owner types, Save → PATCH {description}.
        let p_save = pid.clone();
        info = info.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .px(px(16.0))
                .py(px(10.0))
                .child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.text_muted)
                        .child("about"),
                )
                .child(
                    div()
                        .min_h(px(60.0))
                        .px(px(10.0))
                        .py(px(8.0))
                        .rounded(px(10.0))
                        .bg(theme.field)
                        .border_1()
                        .border_color(theme.field_border)
                        .text_size(px(TEXT_BODY))
                        .child(shell.description_input.clone()),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            glass::quiet_button("profile-desc-cancel", "Cancel", &theme)
                                .h(px(32.0))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.state.update(cx, |s, cx| {
                                        s.set_profile_editing_description(false, cx)
                                    });
                                })),
                        )
                        .child(
                            glass::primary_button("profile-desc-save", "Save", &theme)
                                .h(px(32.0))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let text = this.description_input.read(cx).text().to_string();
                                    let p = p_save.clone();
                                    this.state
                                        .update(cx, |s, cx| s.set_description(p, text, cx));
                                })),
                        ),
                ),
        );
    } else {
        let row = info_row(
            "about",
            description.clone().unwrap_or_else(|| {
                if mine {
                    "Add a description".into()
                } else {
                    "—".into()
                }
            }),
            &theme,
        );
        if mine {
            let current = description.clone().unwrap_or_default();
            info = info.child(
                div()
                    .id("profile-desc-edit")
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .child(row)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let current = current.clone();
                        this.description_input
                            .update(cx, |input, cx| input.set_text(current, cx));
                        this.state
                            .update(cx, |s, cx| s.set_profile_editing_description(true, cx));
                        let handle = this.description_input.read(cx).focus_handle(cx);
                        window.focus(&handle, cx);
                    })),
            );
        } else {
            info = info.child(row);
        }
    }

    // ----- appearance (mine only): the tree + colour picker, folded ----------
    let appearance = mine.then(|| {
        let mut section = card(&theme).child(
            glass::nav_row(
                "profile-appearance",
                "Appearance",
                Some(if picker_open {
                    "tree & colour".into()
                } else {
                    format!(
                        "{} · {}",
                        current
                            .as_deref()
                            .and_then(|s| avatars::all().iter().find(|c| c.slug == s))
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| "tree".into()),
                        avatars::palette_for(current_color.as_deref()).name
                    )
                }),
                &theme,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_profile_picker(cx));
            })),
        );
        if picker_open {
            section = section
                .child(heading("Colour", &theme))
                .child(colour_row(&project, current.as_deref(), current_color.as_deref(), fallback, &theme, cx))
                .child(div().h(px(1.0)).mx(px(16.0)).bg(theme.border))
                .child(heading("Character", &theme))
                .child(tree_grid(&project, current.as_deref(), current_color.as_deref(), fallback, &theme, cx))
                .child(
                    div()
                        .px(px(16.0))
                        .pb(px(12.0))
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.text_muted)
                        .child("Picked for you from the agent's id; choose any tree and colour to make it yours."),
                );
        }
        section
    });

    // ----- tabs -----------------------------------------------------------------
    let tabs = tab_bar(&TABS, tab, &theme, cx);
    let body_tab = match tab {
        0 => media_grid(&content.media, &theme),
        1 => file_list(&content.files, "doc", "No files shared yet", &theme),
        _ => link_list(&content.links, &theme, cx),
    };

    let body = div()
        .flex()
        .flex_col()
        .pb(px(16.0))
        .child(hero)
        .child(actions)
        .children(more)
        .child(info)
        .children(appearance)
        .child(tabs)
        .child(body_tab);
    // "Edit" opens the agent editor: the header menu (model, effort, members, avatar).
    let edit = mine.then(|| {
        let p = pid.clone();
        glass::sheet_button("profile-edit", "Edit", &theme).on_click(cx.listener(
            move |this, _, _, cx| {
                let p = p.clone();
                this.state.update(cx, |s, cx| {
                    s.close_profile(cx);
                    s.select_project(p, cx);
                });
                this.members_open = false;
                this.add_agent_open = false;
                this.menu_open = true;
                cx.notify();
            },
        ))
    });
    Some(frame(body, edit, &theme, cx))
}

fn heading(text: &str, theme: &Theme) -> Div {
    div()
        .px(px(16.0))
        .pt(px(14.0))
        .pb(px(10.0))
        .text_size(px(TEXT_BODY + 1.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(text.to_string())
}

/// Every palette entry, drawn as the current character; one click sets it.
fn colour_row(
    project: &Project,
    current: Option<&str>,
    current_color: Option<&str>,
    fallback: gpui::Hsla,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Div {
    let mut colours = div()
        .flex()
        .flex_wrap()
        .gap(px(10.0))
        .px(px(16.0))
        .pb(px(14.0));
    let palette = avatars::palette_for(current_color);
    // Mono is the brand mark's look, never an agent's (D74): only real colours.
    for p in avatars::colors().iter().filter(|p| !p.is_mono()) {
        let selected = p.id == palette.id;
        let pid = project.id.clone();
        let id = p.id.clone();
        colours = colours.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "avatar-color-{}",
                    p.id
                ))))
                .size(px(52.0))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .border_2()
                .border_color(if selected {
                    theme.text
                } else {
                    gpui::transparent_black()
                })
                .cursor_pointer()
                .hover(|st| st.border_color(theme.border_strong))
                .child(avatars::element(
                    current,
                    Some(&p.id),
                    AvatarState::Idle,
                    44.0,
                    &project.icon,
                    fallback,
                    theme,
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let p = pid.clone();
                    let c = id.clone();
                    this.state
                        .update(cx, |st, cx| st.set_avatar_color(p, c, cx));
                })),
        );
    }
    colours
}

/// Every character in the current colour; one click sets it (the page stays open).
fn tree_grid(
    project: &Project,
    current: Option<&str>,
    current_color: Option<&str>,
    fallback: gpui::Hsla,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Div {
    let mut grid = div()
        .flex()
        .flex_wrap()
        .gap(px(6.0))
        .px(px(10.0))
        .pb(px(10.0));
    for c in avatars::all() {
        let slug = c.slug.clone();
        let selected = current == Some(c.slug.as_str());
        let pid = project.id.clone();
        grid = grid.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "avatar-pick-{}",
                    c.slug
                ))))
                .w(px(96.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.0))
                .py(px(10.0))
                .rounded(px(PANEL_RADIUS - 2.0))
                .when(!c.ready, |el| el.opacity(0.6))
                .bg(if selected {
                    theme.surface_2
                } else {
                    gpui::transparent_black()
                })
                .cursor_pointer()
                .hover(|st| st.bg(theme.surface_2))
                .child(avatars::element(
                    Some(&c.slug),
                    current_color,
                    AvatarState::Idle,
                    60.0,
                    "·",
                    fallback,
                    theme,
                ))
                .child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(if selected {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .max_w(px(88.0))
                        .truncate()
                        .child(c.name.clone()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let p = pid.clone();
                    let sl = slug.clone();
                    this.state.update(cx, |st, cx| {
                        st.set_avatar(p, sl, cx);
                        // Keep the page open: picking a tree then a colour is one visit.
                        st.avatar_picker = Some(pid.clone());
                        st.profile = Some(ProfilePage::Agent {
                            project_id: pid.clone(),
                        });
                        cx.notify();
                    });
                })),
        );
    }
    let _ = icon;
    grid
}
