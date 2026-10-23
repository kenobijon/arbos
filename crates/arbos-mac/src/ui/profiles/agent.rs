//! An agent's page on the shared profile pane (D93): one [`ProfilePane`]
//! spec — hero (leaf / photo with the owner's crayon, editable name,
//! "working" or "Agent · run by @owner"), the tabs **Profile** (name, handle,
//! description) | **Settings** (model, reasoning) | **Library** (images,
//! files, links from the agent's chats), and a Delete agent footer for the
//! owner. The same three tabs as the iPhone's agent page. Voice, Share
//! project and Add to this chat left the pane on 2026-10-22 (Jacob: simple,
//! consistent, powerful); the avatar picker opens from the hero while the
//! pane is up.

use gpui::{div, prelude::*, px, Context, Div, ElementId, Focusable, SharedString, Stateful};

use crate::app::{AgentPick, ProfilePage};
use crate::model::{Id, MachineState, Project};
use crate::ui::avatars::{self, AvatarState};
use crate::ui::glass;
use crate::ui::hoverpick;
use crate::ui::motion::PulseClock;
use crate::ui::shell::Shell;
use crate::ui::sidebar::machine_dot;
use crate::ui::theme::{Theme, TEXT_SMALL, TEXT_TINY};

use super::kit::{self, Leading};
use super::pane::{self, Hero, Picture, ProfilePane, Section};
use super::{
    chats_of, collect, editable_name, editable_picture, file_list, link_list, HeroClick,
    HERO_AVATAR,
};

/// The agent pane's tabs — the same three on the iPhone (`AgentProfileView`).
/// Jacob (2026-10-22 22:32 UTC): Library stays; every pane shares the
/// structure (hero, pill, options), not a tab count.
pub(super) const TABS: [&str; 3] = ["Profile", "Settings", "Library"];

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
    // ----- hero (D93): picture, name, one subtitle ---------------------------------
    // "working" while a turn runs, else who runs the agent; the machine state
    // sits on the disc's dot. The @slug lives on the Profile tab (Handle).
    let owner_handle = project
        .owner_handle
        .as_deref()
        .map(|o| o.trim_start_matches('@').to_owned())
        .filter(|o| !o.is_empty());
    let subtitle: SharedString = if streaming {
        "working".into()
    } else if mine {
        "Agent · run by you".into()
    } else if let Some(o) = &owner_handle {
        format!("Agent · run by @{o}").into()
    } else {
        format!("Agent · {status}").into()
    };
    let hero = Hero {
        picture: Picture::Element(avatar),
        name: name_el,
        badge: None,
        subtitle: Some(subtitle),
        subtitle_color: status_color,
        editor: None,
    };

    // ----- appearance (avatar click) -------------------------------------------
    // D92: the leaf picker (nine leaves, ten colours; the hero above is the live
    // preview). With the flag off: Automatic + the seven D79 gradients. Shown
    // only while the picker is open, on whichever tab is up, so it is not a
    // permanent settings row.
    let leafy = avatars::leaf_avatars_on();
    let appearance: Option<Section> = (mine && picker_open).then(|| {
        let header = kit::row(
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
        }))
        .into_any_element();
        let (swatches, note) = if leafy {
            (
                leaf_picker(&project, &theme, cx).into_any_element(),
                "Automatic picks the leaf and colour from the agent's id, the same on every device; choose your own to make it yours.",
            )
        } else {
            (
                colour_row(&project, &theme, cx).into_any_element(),
                "Automatic picks the colour from the agent's id, the same on every device; choose one to make it yours.",
            )
        };
        Section::Picker {
            header,
            swatches,
            note: note.into(),
        }
    });

    let description = project.description.clone().unwrap_or_default();
    let slug = format!("@{}", Project::mention_slug(&project.name));
    let mut body: Vec<Section> = Vec::new();
    body.extend(appearance);

    match tab {
        0 => {
            // ----- Profile: who the agent is ------------------------------------------
            let mut rows: Vec<gpui::AnyElement> = Vec::new();
            let name_row = kit::value_row("agent-name-row", "Name", project.name.clone(), &theme);
            rows.push(if mine {
                let p = pid.clone();
                name_row
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_edit_agent(p.clone(), false, window, cx);
                    }))
                    .into_any_element()
            } else {
                name_row.into_any_element()
            });
            rows.push(
                kit::value_row("agent-handle-row", "Handle", slug, &theme).into_any_element(),
            );
            if editing_description {
                rows.push(
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
                        )
                        .into_any_element(),
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
                rows.push(if mine {
                    let p = pid.clone();
                    row.cursor_pointer()
                        .hover(|s| s.bg(theme.surface_2))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_edit_agent(p.clone(), true, window, cx);
                        }))
                        .into_any_element()
                } else {
                    row.into_any_element()
                });
            }
            body.push(Section::Card(rows));
            body.push(Section::Footnote(
                if mine {
                    "The name, handle, picture and description are what people and their agents see, here and on the iPhone."
                } else {
                    "Someone else's agent: what it shares with you shows in your chats."
                }
                .into(),
            ));
        }
        2 => {
            // ----- Library: everything the agent's chats hold ----------------------------
            let content = content.expect("collected for the Library tab");
            if content.media.is_empty() && content.files.is_empty() && content.links.is_empty() {
                body.push(Section::Empty("Nothing shared here yet".into()));
            } else {
                if !content.media.is_empty() {
                    body.push(Section::Title(
                        format!("Images · {}", content.media.len()).into(),
                    ));
                    body.push(Section::Grid(content.media));
                }
                if !content.files.is_empty() {
                    body.push(Section::Title(
                        format!("Files · {}", content.files.len()).into(),
                    ));
                    // Custom: the kit's file list is a list of its own rows (noted on #1330).
                    body.push(Section::Custom(
                        file_list(&content.files, "doc", "", &theme).into_any_element(),
                    ));
                }
                if !content.links.is_empty() {
                    body.push(Section::Title(
                        format!("Links · {}", content.links.len()).into(),
                    ));
                    body.push(Section::Custom(
                        link_list(&content.links, shell, &theme, cx).into_any_element(),
                    ));
                }
            }
        }
        _ => {
            // ----- Settings: how the agent runs — Model · Reasoning ---------------------
            // (Voice, Share project and Add to this chat left the pane on
            // 2026-10-22 — Jacob: simple, consistent, powerful.)
            if mine {
                let model_open = pick == Some(AgentPick::Model);
                let reasoning_open = pick == Some(AgentPick::Reasoning);
                let rows = vec![
                    hoverpick::row(
                        "Model",
                        model_text,
                        AgentPick::Model,
                        model_open,
                        model_open.then(|| hoverpick::model_panel(shell, &project, cx)),
                        &theme,
                        cx,
                    )
                    .into_any_element(),
                    hoverpick::row(
                        "Reasoning",
                        project.effort.label(),
                        AgentPick::Reasoning,
                        reasoning_open,
                        reasoning_open.then(|| hoverpick::reasoning_panel(&project, cx)),
                        &theme,
                        cx,
                    )
                    .into_any_element(),
                ];
                // When a row has just opened, the pane scrolls so its list is
                // in view (Section::Reveal).
                let open = pick.and_then(|open| match open {
                    AgentPick::Model => Some(0),
                    AgentPick::Reasoning => Some(1),
                    AgentPick::Voice => None,
                });
                body.push(match open {
                    Some(open) => Section::Reveal { rows, open },
                    None => Section::Card(rows),
                });
                // Agent-to-agent messaging: who else's agents may write into this agent's
                // chat. My own agents always may; this card lists other people (D83's
                // People-card shape: avatar · name/@handle · last message · switch).
                body.push(Section::Title(
                    format!("Who may message {}", project.name).into(),
                ));
                body.push(Section::Card(message_grant_rows(
                    shell, &project, &theme, cx,
                )));
                body.push(Section::Footnote(
                    format!(
                        "Your own agents can always message {}. People listed here may have their agents message it too; their messages land in this chat and {} answers them.",
                        project.name, project.name
                    )
                    .into(),
                ));
            } else {
                body.push(Section::Card(vec![kit::value_row(
                    "agent-model-row",
                    "Model",
                    model_text,
                    &theme,
                )
                .into_any_element()]));
            }
        }
    }

    // ----- footer: Delete agent (owner) ------------------------------------------
    // The one danger action, in its own card; the first click swaps the row
    // for "Delete “Name”?" with Cancel / Delete (the sidebar's rule).
    let footer: Vec<Section> = if mine {
        let delete_id = pid.clone();
        let row: gpui::AnyElement = if shell.agent_delete_armed {
            kit::confirm_row(
                &format!("Delete “{}”?", project.name),
                "Delete",
                kit::confirm_button("agent-delete-cancel", "Cancel", false, &theme).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.agent_delete_armed = false;
                        cx.notify();
                    }),
                ),
                kit::confirm_button("agent-delete-go", "Delete", true, &theme).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.agent_delete_armed = false;
                        let id = delete_id.clone();
                        this.state.update(cx, |s, cx| s.delete_project(id, cx));
                        this.close_profile(window, cx);
                    }),
                ),
                &theme,
            )
            .into_any_element()
        } else {
            kit::danger_row("agent-delete", "Delete agent", &theme)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.agent_delete_armed = true;
                    cx.notify();
                }))
                .into_any_element()
        };
        vec![Section::Card(vec![row])]
    } else {
        Vec::new()
    };

    // No "Edit" — click the avatar to change its colour; click the name or
    // the Description row to edit. "Done" only while editing.
    let done = (mine && (renaming || editing_description)).then(|| {
        glass::sheet_button("profile-done", "Done", &theme)
            .on_click(cx.listener(|this, _, window, cx| this.finish_edit_agent(window, cx)))
    });
    Some(pane::render(
        ProfilePane {
            page: ProfilePage::Agent {
                project_id: project.id.clone(),
            },
            hero,
            tabs: &TABS,
            selected: tab,
            body,
            footer,
            done,
        },
        shell,
        cx,
    ))
}

/// The rows of "Who may message X": one per grant (person or agent of another owner) with a
/// switch, then "Give a person access…" → the people picker.
fn message_grant_rows(
    shell: &Shell,
    project: &Project,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Vec<gpui::AnyElement> {
    let state = shell.state.read(cx);
    let mut rows: Vec<gpui::AnyElement> = Vec::new();
    for g in &project.message_grants {
        let person = crate::model::Person {
            id: g.id.clone(),
            handle: g.handle.clone().unwrap_or_else(|| g.id.clone()),
            name: g.name.clone(),
            avatar_url: g.avatar_url.clone(),
            ..Default::default()
        };
        let title = g
            .name
            .clone()
            .or_else(|| g.handle.clone().map(|h| format!("@{h}")))
            .unwrap_or_else(|| g.id.clone());
        let mut subtitle = match (&g.handle, g.kind.as_str()) {
            (Some(h), "person") => format!("@{h}"),
            (Some(h), _) => format!("@{h} · agent"),
            (None, k) => k.to_owned(),
        };
        if let Some(at) = g.last_at {
            subtitle = format!("{subtitle} · messaged {}", crate::model::ago(at));
        } else if !g.note.is_empty() {
            subtitle = format!("{subtitle} · {}", g.note);
        }
        let (pid, grantee, on) = (project.id.clone(), g.grantee.clone(), g.enabled);
        let avatar = if g.kind == "agent" {
            avatars::element(&g.id, &title, None, kit::AVATAR, theme).into_any_element()
        } else {
            crate::ui::people::avatar(state.avatar(&g.avatar_url), &person, kit::AVATAR, theme)
                .into_any_element()
        };
        let switch = div()
            .id(ElementId::Name(SharedString::from(format!(
                "message-grant-switch-{}",
                g.grantee
            ))))
            .cursor_pointer()
            .child(glass::switch(on, theme))
            .on_click(cx.listener({
                let (pid, grantee) = (pid.clone(), grantee.clone());
                move |this, _, _, cx| {
                    let (pid, grantee) = (pid.clone(), grantee.clone());
                    this.state
                        .update(cx, |s, cx| s.set_message_grant(pid, grantee, !on, cx));
                }
            }))
            .into_any_element();
        rows.push(
            kit::row(
                ElementId::Name(SharedString::from(format!("message-grant-{}", g.grantee))),
                Leading::Avatar(avatar),
                title,
                Some(subtitle.into()),
                None,
                Some(switch),
                false,
                theme,
            )
            // Right-click removes the row (the switch only pauses it).
            .on_mouse_down(gpui::MouseButton::Right, {
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let (pid, grantee) = (pid.clone(), grantee.clone());
                    this.state
                        .update(cx, |s, cx| s.remove_message_grant(pid, grantee, cx));
                })
            })
            .into_any_element(),
        );
    }
    let pid = project.id.clone();
    rows.push(
        kit::row(
            "message-grant-add",
            Leading::Glyph("person-plus"),
            "Give a person access…",
            None,
            None,
            None,
            false,
            theme,
        )
        .text_color(theme.accent)
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .on_click(cx.listener(move |this, _, window, cx| {
            let id = pid.clone();
            this.state
                .update(cx, |s, cx| s.open_message_grant_picker(id, cx));
            let handle = this.people_input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }))
        .into_any_element(),
    );
    rows
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
