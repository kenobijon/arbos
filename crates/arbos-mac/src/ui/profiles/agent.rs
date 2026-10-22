//! An agent's page on the shared pane kit: the D79 avatar (crayon = the
//! colour picker for owned agents), name, what it is doing, @slug, and the
//! info card (for my own agent — Model / Reasoning / Voice rows that open a
//! choice list on click). No quick-action row (panels-unify, 2026-10-21):
//! call and search live in the chat, mute in the sidebar row's menu. Matches
//! the Grok bot page otherwise: no about, camera vision, secrets, or Media /
//! Files / Links tabs. Owner and machine stay off this page.

use gpui::{div, prelude::*, px, Context, Div, ElementId, SharedString, Stateful};

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
use super::{card, editable_name, editable_picture, frame, hero, HeroClick, HERO_AVATAR};

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
    let pid: Id = project.id.clone();

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

    // ----- info card -------------------------------------------------------------
    // Grok-bot parity: model / reasoning / voice only. About, camera vision,
    // secrets, and Media / Files / Links stay off this page. Avatar colour
    // opens from a click on the big avatar, not as a settings row.
    let mut info = card(&theme);
    if mine {
        // The agent's own settings: Model (searchable), Reasoning and Voice
        // open a choice list on click; a pick applies and folds the list.
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
            info =
                info.on_children_prepainted(move |bounds, window, cx| reveal(&bounds, window, cx));
        }
    } else {
        info = info.child(kit::value_row(
            "agent-model-row",
            "Model",
            model_text,
            &theme,
        ));
    }

    // ----- appearance (avatar click) -------------------------------------------
    // D91: the leaf picker (nine leaves, ten colours; the hero above is the live
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

    let body = div()
        .flex()
        .flex_col()
        .pb(px(16.0))
        .child(hero)
        .child(info)
        .children(appearance);
    // No "Edit" — click the avatar to change its colour; double-click the
    // name to rename. "Done" only while renaming.
    let done = (mine && renaming).then(|| {
        glass::sheet_button("profile-done", "Done", &theme)
            .on_click(cx.listener(|this, _, window, cx| this.finish_edit_agent(window, cx)))
    });
    Some(frame(body, done, Some(&shell.profile_scroll), &theme, cx))
}

/// D91 picker: the nine leaves on the agent's current disc colour, the ten
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
