//! An agent's page on the shared pane kit, laid out like the own-profile pane
//! (mock g; Jacob 2026-10-22): the D79 avatar (crayon = the picker for owned
//! agents), name, what it is doing, @slug, then one tab pill — **Profile**
//! (name, handle, description, owner) | **Settings** (model, reasoning,
//! voice; sharing and group chats; spoken replies) | **Library** (images,
//! files and links from the agent's chats). The same three tabs as the
//! iPhone's agent page (Jacob's rule, 2026-10-22). No quick-action row:
//! call and search live in the chat, mute in the sidebar row's menu. Grok
//! bot page otherwise: no camera vision, secrets, or Media / Files / Links.

use gpui::{div, prelude::*, px, Context, Div, ElementId, Focusable, SharedString, Stateful};

use crate::app::AgentPick;
use crate::model::{Id, MachineState, Project};
use crate::ui::avatars::{self, AvatarState};
use crate::ui::glass;
use crate::ui::hoverpick;
use crate::ui::motion::PulseClock;
use crate::ui::shell::Shell;
use crate::ui::sidebar::machine_dot;
use crate::ui::theme::{Theme, TEXT_SMALL, TEXT_TINY};

use super::kit::{self, Leading};
use super::{
    card, chats_of, collect, editable_name, editable_picture, empty, file_list, hero, link_list,
    media_grid, pushed_frame, tab_bar, HeroClick, HERO_AVATAR,
};

/// The agent pane's tabs — the same three on the iPhone (`AgentProfileView`).
const TABS: [&str; 3] = ["Profile", "Settings", "Library"];

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
            avatar_color_index: a.avatar_color_index,
            avatar_leaf: a.avatar_leaf,
            avatar_leaf_color: a.avatar_leaf_color,
            description: a.description.clone(),
            owner_person: a.owner.clone(),
            model: a.model.clone().unwrap_or_default(),
            shared: true,
            owner_handle: Some(a.owner_handle.clone()),
            ..Default::default()
        })
}

pub(super) fn page(
    project_id: &str,
    shell: &Shell,
    cx: &mut Context<Shell>,
) -> Option<Stateful<Div>> {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let (project, mine): (Project, bool) = match state.project(project_id) {
        Some(p) => (p.clone(), !p.is_guest()),
        None => (project_from_member(state, project_id)?, false),
    };
    let avatar_state = state.avatar_state(&project.id);
    let streaming = state.project_is_streaming(&project.id);
    let voice_text = crate::ui::voicepicker::current_label(&project, &state.voices);
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
    let model_text = if project.model.is_empty() {
        "default".to_string()
    } else {
        project.model.clone()
    };
    let picker_open = state.profile_picker_open;
    let pick = state.agent_pick;
    let renaming = state.profile_editing_name && mine;
    let tab = state.profile_tab;
    let editing_description = state.profile_editing_description && mine;
    let pid: Id = project.id.clone();
    // Library tab: what the agent's chats hold (read while `state` is borrowed).
    let content = (tab == 2).then(|| {
        let chats: Vec<&crate::model::Chat> = chats_of(state, &project.id);
        collect(state, &chats)
    });

    if avatar_state != AvatarState::Idle {
        PulseClock::request(cx);
    }

    // ----- hero ----------------------------------------------------------------
    // The shared pane hero: owned agents get the crayon on the picture (the
    // avatar-colour picker) and the inline name field; others are read-only.
    let picture = div()
        .relative()
        .size(px(HERO_AVATAR))
        .child(avatars::project_hero(&project, HERO_AVATAR, &theme))
        .child(
            div()
                .absolute()
                .right(px(0.0))
                .bottom(px(0.0))
                .size(px(26.0))
                .rounded_full()
                .bg(theme.card())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(16.0))
                        .rounded_full()
                        .bg(machine_dot(machine_state, &theme)),
                ),
        )
        .into_any_element();
    let pic_click = mine.then(|| {
        let f = cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
            this.state.update(cx, |s, cx| s.toggle_profile_picker(cx));
        });
        Box::new(f) as HeroClick
    });
    let avatar = editable_picture("profile-avatar", picture, pic_click).into_any_element();
    let p_rename = pid.clone();
    let name_click = mine.then(|| {
        let f = cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
            this.start_edit_agent(p_rename.clone(), false, window, cx);
        });
        Box::new(f) as HeroClick
    });
    let name_el = editable_name(
        "profile-name",
        &project.name,
        renaming,
        &shell.agent_name_input,
        name_click,
        &theme,
    )
    .into_any_element();
    let hero = hero(avatar, name_el, &status, status_color, &theme).child(
        div()
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_faint)
            .child(format!("@{}", Project::mention_slug(&project.name))),
    );

    // ----- Profile | Settings (Jacob, #1318): the own-profile pane's layout ------
    // One tab pill under the hero, like mock g. Profile = who the agent is
    // (name, handle, description, owner); Settings = how it runs (model,
    // reasoning, voice, sharing, group chats, spoken replies).
    // Jacob's rule (2026-10-22): one tab set per entity, identical on Mac and
    // iPhone. Agents: Profile | Settings | Library (what the agent's chats
    // hold — images, files, links).
    let tabs = tab_bar(&TABS, tab, &theme, cx);
    let description = project.description.clone().unwrap_or_default();
    let slug = format!("@{}", Project::mention_slug(&project.name));
    let owner = project
        .owner_handle
        .as_deref()
        .filter(|o| !mine && !o.trim().is_empty())
        .map(|o| format!("@{}", o.trim_start_matches('@')));

    // ----- appearance (avatar click) -------------------------------------------
    // D92: the leaf picker (nine leaves, ten colours; the hero above is the live
    // preview). With the flag off: Automatic + the seven D79 gradients. Shown
    // only while the picker is open so it is not a permanent settings row.
    let leafy = avatars::leaf_avatars_on();
    let appearance = (mine && picker_open).then(|| {
        let card = card(&theme).child(
            kit::row(
                "profile-appearance",
                Leading::None,
                if leafy { "Avatar" } else { "Avatar colour" },
                None,
                Some("pick".into()),
                None,
                true,
                &theme,
            )
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_profile_picker(cx));
            })),
        );
        if leafy {
            card.child(leaf_picker(&project, &theme, cx).pt(px(14.0))).child(
                div()
                    .px(px(kit::INSET))
                    .pb(px(12.0))
                    .text_size(px(TEXT_TINY))
                    .text_color(theme.text_muted)
                    .child("Automatic picks the leaf and colour from the agent's id, the same on every device; choose your own to make it yours."),
            )
        } else {
            card.child(colour_row(&project, &theme, cx).pt(px(14.0))).child(
                div()
                    .px(px(kit::INSET))
                    .pb(px(12.0))
                    .text_size(px(TEXT_TINY))
                    .text_color(theme.text_muted)
                    .child("Automatic picks the colour from the agent's id, the same on every device; choose one to make it yours."),
            )
        }
    });

    let mut body = div()
        .flex()
        .flex_col()
        .pb(px(16.0))
        .child(hero)
        .child(tabs)
        .children(appearance);

    if tab == 0 {
        // ----- Profile: who the agent is ------------------------------------------
        let mut info = card(&theme);
        let name_row = kit::value_row("agent-name-row", "Name", project.name.clone(), &theme);
        info = info.child(if mine {
            let p = pid.clone();
            name_row
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_edit_agent(p.clone(), false, window, cx);
                }))
        } else {
            name_row
        });
        info = info.child(kit::value_row("agent-handle-row", "Handle", slug, &theme));
        if editing_description {
            info = info.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .px(px(kit::INSET))
                    .py(px(10.0))
                    .child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child("Description"),
                    )
                    .child(
                        div()
                            .min_h(px(60.0))
                            .px(px(8.0))
                            .py(px(6.0))
                            .rounded(px(8.0))
                            .bg(theme.field)
                            .border_1()
                            .border_color(theme.field_border)
                            .child(shell.description_input.clone()),
                    ),
            );
        } else {
            let shown = if description.trim().is_empty() {
                if mine {
                    "Add a description".to_string()
                } else {
                    "—".to_string()
                }
            } else {
                description.clone()
            };
            let row = kit::row(
                "agent-description-row",
                Leading::None,
                "Description",
                Some(shown.into()),
                None,
                None,
                mine,
                &theme,
            );
            info = info.child(if mine {
                let p = pid.clone();
                row.cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_edit_agent(p.clone(), true, window, cx);
                    }))
            } else {
                row
            });
        }
        if let Some(owner) = owner {
            info = info.child(kit::value_row("agent-owner-row", "Owned by", owner, &theme));
        }
        body = body.child(info).child(kit::footnote(
            if mine {
                "The name, handle, picture and description are what people and their agents see, here and on the iPhone."
            } else {
                "Someone else's agent: what it shares with you shows in your chats."
            },
            &theme,
        ));
    } else if tab == 2 {
        // ----- Library: everything the agent's chats hold ----------------------------
        let content = content.expect("collected for the Library tab");
        if content.media.is_empty() && content.files.is_empty() && content.links.is_empty() {
            body = body.child(empty("Nothing shared here yet", &theme));
        } else {
            if !content.media.is_empty() {
                body = body
                    .child(kit::section_title(
                        format!("Images · {}", content.media.len()),
                        &theme,
                    ))
                    .child(media_grid(&content.media, &theme));
            }
            if !content.files.is_empty() {
                body = body
                    .child(kit::section_title(
                        format!("Files · {}", content.files.len()),
                        &theme,
                    ))
                    .child(file_list(&content.files, "doc", "", &theme));
            }
            if !content.links.is_empty() {
                body = body
                    .child(kit::section_title(
                        format!("Links · {}", content.links.len()),
                        &theme,
                    ))
                    .child(link_list(&content.links, shell, &theme, cx));
            }
        }
    } else {
        // ----- Settings: how the agent runs ----------------------------------------
        let mut info = card(&theme);
        if mine {
            // Model (searchable), Reasoning and Voice open a choice list on
            // click; a pick applies and folds the list.
            let model_open = pick == Some(AgentPick::Model);
            let reasoning_open = pick == Some(AgentPick::Reasoning);
            let voice_open = pick == Some(AgentPick::Voice);
            info = info
                .child(hoverpick::row(
                    "Model",
                    model_text,
                    AgentPick::Model,
                    model_open,
                    model_open.then(|| hoverpick::model_panel(shell, &project, cx)),
                    &theme,
                    cx,
                ))
                .child(hoverpick::row(
                    "Reasoning",
                    project.effort.label(),
                    AgentPick::Reasoning,
                    reasoning_open,
                    reasoning_open.then(|| hoverpick::reasoning_panel(&project, cx)),
                    &theme,
                    cx,
                ))
                .child(hoverpick::row(
                    "Voice",
                    voice_text,
                    AgentPick::Voice,
                    voice_open,
                    voice_open.then(|| hoverpick::voice_panel(shell, &project, cx)),
                    &theme,
                    cx,
                ));
            // Rows are the card's children 0 / 1 / 2; when one has just opened,
            // scroll the sheet so its list is in view.
            if let Some(open) = pick {
                let open_ix = match open {
                    AgentPick::Model => 0,
                    AgentPick::Reasoning => 1,
                    AgentPick::Voice => 2,
                };
                let reveal = cx.listener(
                    move |this, bounds: &Vec<gpui::Bounds<gpui::Pixels>>, window, cx| {
                        this.reveal_agent_pick(bounds.get(open_ix), window, cx);
                    },
                );
                info = info
                    .on_children_prepainted(move |bounds, window, cx| reveal(&bounds, window, cx));
            }
        } else {
            info = info.child(kit::value_row(
                "agent-model-row",
                "Model",
                model_text,
                &theme,
            ));
        }
        body = body.child(info);
        if mine {
            body = settings_cards(body, shell, &project, &theme, cx);
        }
    }

    // No "Edit" — click the avatar to change its colour; click the name or
    // the Description row to edit. "Done" only while editing.
    let done = (mine && (renaming || editing_description)).then(|| {
        glass::sheet_button("profile-done", "Done", &theme)
            .on_click(cx.listener(|this, _, window, cx| this.finish_edit_agent(window, cx)))
    });
    Some(pushed_frame(
        &project.name,
        body,
        done,
        Some(&shell.profile_scroll),
        shell,
        &theme,
        cx,
    ))
}

/// D92 picker: the nine leaves on the agent's current disc colour, the ten
/// colours as plain discs, and an Automatic reset. Each click saves at once
/// (optimistic, then `PATCH /projects/{id}`), so the hero is the live preview.
fn leaf_picker(project: &Project, theme: &Theme, cx: &mut Context<Shell>) -> Div {
    let current = avatars::project_leaf(project);
    let picked_leaf = project.avatar_leaf;
    let picked_color = project.avatar_leaf_color;
    let automatic = picked_leaf.is_none() && picked_color.is_none();
    let ring = |selected: bool, size: f32, theme: &Theme| {
        div()
            .size(px(size))
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
    };
    let label = |text: &'static str, theme: &Theme| {
        div()
            .px(px(16.0))
            .pb(px(6.0))
            .text_size(px(TEXT_TINY))
            .text_color(theme.text_muted)
            .child(text)
    };

    let mut leaves = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.0))
        .px(px(16.0))
        .pb(px(12.0));
    for (i, l) in avatars::leaf::LEAVES.iter().enumerate() {
        let index = i as u8;
        let pid = project.id.clone();
        let pick = avatars::LeafPick {
            leaf: index,
            color: current.color,
        };
        leaves = leaves.child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(3.0))
                .child(
                    ring(current.leaf == index, 50.0, theme)
                        .id(ElementId::Name(SharedString::from(format!(
                            "avatar-leaf-{index}"
                        ))))
                        .child(avatars::leaf_element(pick, 42.0))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let p = pid.clone();
                            this.state.update(cx, |st, cx| {
                                st.set_avatar_leaf(p, Some(index), picked_color, cx)
                            });
                        })),
                )
                .child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(if current.leaf == index {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .child(l.name),
                ),
        );
    }

    let mut colours = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.0))
        .px(px(16.0))
        .pb(px(10.0));
    for index in 0..avatars::leaf::COLOR_COUNT {
        let pid = project.id.clone();
        colours = colours.child(
            ring(current.color == index, 36.0, theme)
                .id(ElementId::Name(SharedString::from(format!(
                    "avatar-leaf-color-{index}"
                ))))
                .child(
                    div()
                        .size(px(28.0))
                        .rounded_full()
                        .bg(avatars::leaf_disc(index)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let p = pid.clone();
                    this.state.update(cx, |st, cx| {
                        st.set_avatar_leaf(p, picked_leaf, Some(index), cx)
                    });
                })),
        );
    }

    let pid = project.id.clone();
    let reset = div()
        .id("avatar-leaf-auto")
        .mx(px(16.0))
        .mb(px(12.0))
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(8.0))
        .text_size(px(TEXT_SMALL))
        .text_color(if automatic {
            theme.text_muted
        } else {
            theme.text
        })
        .bg(theme.surface_2)
        .cursor_pointer()
        .hover(|st| st.bg(theme.border))
        .child(if automatic {
            "Automatic"
        } else {
            "Back to automatic"
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            let p = pid.clone();
            this.state
                .update(cx, |st, cx| st.set_avatar_leaf(p, None, None, cx));
        }));

    div()
        .flex()
        .flex_col()
        .child(label("Leaf", theme))
        .child(leaves)
        .child(label("Colour", theme))
        .child(colours)
        .child(div().flex().child(reset))
}

/// The owned agent's Settings tab, after the Model card: sharing (invite
/// sheet, or the members list once it is shared) and the agent-to-agent group
/// picker in one card, the global spoken-replies switch in its own — the
/// own-profile pane's grouping.
fn settings_cards(
    body: Div,
    shell: &Shell,
    project: &Project,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Div {
    let state = shell.state.read(cx);
    let voice_mode = state.voice_mode;
    let multi_chat = state.multi_chat;
    let other_agents = state
        .projects
        .iter()
        .filter(|p| !p.is_people() && p.id != project.id)
        .count();
    let guests = project.guests().count();
    let has_guests = guests > 0;

    let share_id = project.id.clone();
    let mut card = card(theme).child(
        kit::nav_row(
            "agent-share",
            "person-plus",
            if has_guests {
                "Members"
            } else {
                "Share project…"
            },
            has_guests.then(|| format!("{}", guests + 1)),
            theme,
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            let id = share_id.clone();
            if has_guests {
                this.state.update(cx, |s, cx| s.select_project(id, cx));
                this.members_open = true;
                cx.notify();
            } else {
                this.people_input.update(cx, |i, cx| i.clear(cx));
                this.state.update(cx, |s, cx| {
                    s.select_project(id.clone(), cx);
                    s.open_invite(id, cx);
                });
                let handle = this.people_input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
        })),
    );
    if multi_chat && other_agents > 0 {
        let group_id = project.id.clone();
        card = card.child(
            kit::nav_row(
                "agent-add-to-chat",
                "people",
                "Add to this chat…",
                None,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                let id = group_id.clone();
                this.state.update(cx, |s, cx| s.select_project(id, cx));
                this.set_group_picker(true, cx);
            })),
        );
    }
    let voice = self::card(theme).child(
        kit::row(
            "agent-voice-mode",
            Leading::Tile("mic"),
            "Voice mode",
            Some("Spoken replies, every agent".into()),
            None,
            Some(glass::switch(voice_mode, theme).into_any_element()),
            false,
            theme,
        )
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .on_click(cx.listener(|this, _, _, cx| {
            this.voice.update(cx, |v, cx| v.toggle_voice_mode(cx));
        })),
    );
    body.child(card).child(voice)
}

fn colour_row(project: &Project, theme: &Theme, cx: &mut Context<Shell>) -> Div {
    let mut colours = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(10.0))
        .px(px(16.0))
        .pb(px(14.0));
    let auto_index = avatars::color_index(&project.id);
    let override_index = project.avatar_color_override;
    let ring = |selected: bool, theme: &Theme| {
        div()
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
    };
    // "Automatic": the id-derived gradient with the agent's initials.
    {
        let pid = project.id.clone();
        colours = colours.child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(
                    ring(override_index.is_none(), theme)
                        .id("avatar-color-auto")
                        .child(avatars::element(
                            &project.id,
                            &project.name,
                            None,
                            44.0,
                            theme,
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let p = pid.clone();
                            this.state
                                .update(cx, |st, cx| st.set_avatar_color_index(p, None, cx));
                        })),
                )
                .child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.text_muted)
                        .child("Automatic"),
                ),
        );
    }
    for (i, g) in avatars::GRADIENTS.iter().enumerate() {
        let index = i as u8;
        let selected = override_index == Some(index);
        let pid = project.id.clone();
        colours = colours.child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(4.0))
                .child(
                    ring(selected, theme)
                        .id(ElementId::Name(SharedString::from(format!(
                            "avatar-color-{index}"
                        ))))
                        .child(avatars::swatch(index, 44.0, theme))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let p = pid.clone();
                            this.state
                                .update(cx, |st, cx| st.set_avatar_color_index(p, Some(index), cx));
                        })),
                )
                .child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(if index == auto_index && override_index.is_none() {
                            theme.text
                        } else {
                            theme.text_muted
                        })
                        .child(g.name),
                ),
        );
    }
    colours
}
