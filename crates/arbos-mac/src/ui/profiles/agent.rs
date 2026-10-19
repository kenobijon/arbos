//! An agent's page: the D79 avatar, name, what it is doing, call / mute /
//! search / more, the info card (owner, machine, model, description), an
//! Appearance section holding the avatar colour picker (D79 override, saved
//! on the relay), then Media / Files / Links from its chats.
//!
//! Only controls that work end to end are shown (#1066): the colour swatches
//! PATCH `avatar_color_index`; there is no avatar-image picker on the Mac yet,
//! so none is offered.

use gpui::{
    div, prelude::*, px, AnyElement, Context, Div, ElementId, Focusable, SharedString, Stateful,
};

use crate::app::{CallTarget, ProfilePage};
use crate::model::{Effort, Id, MachineState, Person, Project};
use crate::sync::fixture::MODELS;
use crate::ui::avatars::{self, AvatarState};
use crate::ui::glass;
use crate::ui::icons::icon;
use crate::ui::motion::PulseClock;
use crate::ui::shell::Shell;
use crate::ui::sidebar::machine_dot;
use crate::ui::theme::{Theme, TEXT_BODY, TEXT_SMALL, TEXT_TINY};
use crate::ui::voicepicker;

use super::{
    action_button, action_row, card, chats_of, collect, file_list, frame, hero, hero_name,
    info_row, link_list, media_grid, more_menu, tab_bar, Action,
};

const TABS: [&str; 3] = ["Media", "Files", "Links"];

/// Which agent-settings row is expanded into its dropdown (#1064): rows open
/// as the pointer passes over them and close when it leaves, like a menu bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentPicker {
    Model,
    Reasoning,
    Voice,
}

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
    let secrets_data = state.project_secrets.get(&project.id).cloned();
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
    let renaming = mine
        && state
            .renaming_project
            .as_ref()
            .is_some_and(|p| p == &project.id);
    let muted = state.is_muted(&project.id);
    let content = collect(state, &chats_of(state, &project.id));
    let pid: Id = project.id.clone();

    if avatar_state != AvatarState::Idle {
        PulseClock::request(cx);
    }

    // ----- hero ----------------------------------------------------------------
    let avatar = div()
        .relative()
        .size(px(112.0))
        .child(avatars::project(&project, 112.0, &theme))
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
    let name_el = if renaming {
        div()
            .id("profile-rename")
            .w(px(220.0))
            .h(px(36.0))
            .px(px(10.0))
            .rounded(px(10.0))
            .bg(theme.field)
            .border_1()
            .border_color(theme.field_border)
            .text_size(px(TEXT_BODY + 8.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .child(shell.agent_name_input.clone())
            .into_any_element()
    } else if mine {
        let p_rename = pid.clone();
        hero_name(&project.name, &theme)
            .id("profile-name")
            .cursor_pointer()
            .hover(|s| s.opacity(0.85))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        this.start_rename_agent(p_rename.clone(), window, cx);
                    }
                }),
            )
            .into_any_element()
    } else {
        hero_name(&project.name, &theme).into_any_element()
    };
    let hero = hero(avatar, name_el, &status, status_color, &theme).child(
        div()
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_faint)
            .child(format!("@{}", Project::mention_slug(&project.name))),
    );

    // ----- actions --------------------------------------------------------------
    let p_call = pid.clone();
    let p_mute = pid.clone();
    let name_q = project.name.clone();
    // No separate "voice" tile: calls always start audio-only, so it was the
    // same action as "call" under a second name (#1066).
    let actions = action_row(vec![
        action_button(&Action::new("profile-call", "phone", "call"), &theme).on_click(cx.listener(
            move |this, _, _, cx| {
                let p = p_call.clone();
                this.state
                    .update(cx, |s, cx| s.start_call(CallTarget::Agent(p), false, cx));
            },
        )),
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
    let picker = shell.agent_picker;
    let mut info = card(&theme)
        .child(info_row("owner", owner_text, &theme))
        .child(info_row("machine", machine_text, &theme));
    if mine {
        // Model and reasoning: hover the row and its dropdown unfolds (#1064).
        let model_list = (picker == Some(AgentPicker::Model))
            .then(|| model_list(shell, &project, &theme, cx).into_any_element());
        info = info.child(picker_row(
            AgentPicker::Model,
            "model",
            model_text,
            model_list,
            &theme,
            cx,
        ));
        let effort_list = (picker == Some(AgentPicker::Reasoning))
            .then(|| effort_list(&project, &theme, cx).into_any_element());
        info = info.child(picker_row(
            AgentPicker::Reasoning,
            "reasoning",
            project.effort.label().to_string(),
            effort_list,
            &theme,
            cx,
        ));
    } else {
        info = info.child(info_row("model", model_text, &theme));
    }
    // D83: what this agent may use; opens the agent's Secrets page.
    let secrets_text = match secrets_data.as_ref() {
        Some(d) => {
            let on: Vec<String> = d
                .items
                .iter()
                .filter(|i| i.enabled)
                .map(|i| i.title())
                .collect();
            if on.is_empty() {
                if d.requests.iter().any(|r| r.is_pending()) {
                    "waiting for you".to_string()
                } else {
                    "none".to_string()
                }
            } else {
                format!("{} · {}", on.len(), on.join(", "))
            }
        }
        None => "—".to_string(),
    };
    let p_secrets = pid.clone();
    info = info.child(
        div()
            .id("agent-secrets-row")
            .flex()
            .items_center()
            .gap(px(10.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(TEXT_TINY))
                            .text_color(theme.text_muted)
                            .child("secrets"),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_BODY))
                            .text_color(theme.text)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(secrets_text),
                    ),
            )
            .child(icon("chevron-right", 14.0, theme.text_faint))
            .on_click(cx.listener(move |this, _, _, cx| {
                let pid = p_secrets.clone();
                this.secrets.update(cx, |p, cx| p.show_agent(pid, cx));
                cx.notify();
            })),
    );
    if mine {
        // Call voice: hover unfolds the voice list (play a sample, pick one).
        let voice_list = (picker == Some(AgentPicker::Voice))
            .then(|| voicepicker::inline_list(shell, &project, cx).into_any_element());
        info = info.child(picker_row(
            AgentPicker::Voice,
            "voice",
            voice_text,
            voice_list,
            &theme,
            cx,
        ));
        // Camera vision (voice API §11): a switch row, default on.
        let vision_on = project.vision_enabled;
        let p_vision = pid.clone();
        info = info.child(
            div()
                .id("agent-vision-row")
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(16.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(TEXT_TINY))
                                .text_color(theme.text_muted)
                                .child("camera vision"),
                        )
                        .child(div().text_size(px(TEXT_BODY)).text_color(theme.text).child(
                            if vision_on {
                                "On — the agent can look through your camera on calls"
                            } else {
                                "Off"
                            },
                        )),
                )
                .child(glass::switch(vision_on, &theme))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let p = p_vision.clone();
                    this.state
                        .update(cx, |s, cx| s.set_vision_enabled(p, !vision_on, cx));
                })),
        );
    }
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

    // ----- appearance (mine only): Automatic + the seven gradients, folded ----
    let appearance = mine.then(|| {
        let current_index = avatars::index_for(&project.id, project.avatar_color_override);
        let label = match project.avatar_color_override {
            None => format!("Automatic · {}", avatars::GRADIENTS[current_index as usize].name),
            Some(i) => avatars::GRADIENTS[(i as usize) % 7].name.to_string(),
        };
        let mut section = card(&theme).child(
            glass::nav_row(
                "profile-appearance",
                "Avatar colour",
                Some(if picker_open { "pick".into() } else { label }),
                &theme,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.toggle_profile_picker(cx));
            })),
        );
        if picker_open {
            section = section
                .child(colour_row(&project, &theme, cx))
                .child(
                    div()
                        .px(px(16.0))
                        .pb(px(12.0))
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.text_muted)
                        .child("Automatic picks the colour from the agent's id, the same on every device; choose one to make it yours."),
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
        // Avatar colour sits right under the big avatar it changes.
        .children(appearance)
        .child(info)
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

// ----- hover-expanding pickers (#1064) --------------------------------------------

/// An info row (caption over value, chevron) that unfolds `list` beneath it
/// while the pointer is over the row or the list. Hovering another picker
/// row swaps the open one; a pick collapses it (`agent_picker = None`).
fn picker_row(
    which: AgentPicker,
    caption: &'static str,
    value: String,
    list: Option<AnyElement>,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Stateful<Div> {
    let open = list.is_some();
    let id = match which {
        AgentPicker::Model => "agent-model-row",
        AgentPicker::Reasoning => "agent-reasoning-row",
        AgentPicker::Voice => "agent-voice-row",
    };
    let row = div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(16.0))
        .py(px(10.0))
        .cursor_default()
        .when(open, |el| el.bg(theme.surface_2))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.text_muted)
                        .child(caption),
                )
                .child(
                    div()
                        .text_size(px(TEXT_BODY))
                        .text_color(theme.text)
                        .truncate()
                        .child(value),
                ),
        )
        .child(icon(
            if open { "chevron-up" } else { "chevron-down" },
            14.0,
            theme.text_faint,
        ));
    div()
        .id(id)
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(theme.border)
        .child(row)
        .children(list.map(|list| {
            div()
                .mx(px(12.0))
                .mb(px(10.0))
                .rounded(px(10.0))
                .bg(theme.field)
                .border_1()
                .border_color(theme.field_border)
                .overflow_hidden()
                .child(list)
        }))
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            if *hovered {
                if this.agent_picker == Some(which) {
                    return;
                }
                this.agent_picker = Some(which);
                match which {
                    AgentPicker::Model => {
                        this.model_search_input.update(cx, |i, cx| i.clear(cx));
                        let handle = this.model_search_input.read(cx).focus_handle(cx);
                        window.focus(&handle, cx);
                    }
                    AgentPicker::Voice => this.state.update(cx, |s, _| s.fetch_voices()),
                    AgentPicker::Reasoning => {}
                }
                cx.notify();
            } else if this.agent_picker == Some(which) {
                // Typing a model filter keeps the list up until a pick or Escape.
                let typing = which == AgentPicker::Model
                    && this
                        .model_search_input
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window)
                    && !this.model_search_input.read(cx).text().trim().is_empty();
                if typing {
                    return;
                }
                this.agent_picker = None;
                if which == AgentPicker::Voice {
                    this.state.update(cx, |s, cx| s.stop_voice_sample(cx));
                }
                cx.notify();
            }
        }))
}

fn picker_item(id: ElementId, label: String, on: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .py(px(6.0))
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text)
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .child(div().flex_1().min_w_0().truncate().child(label))
        .when(on, |el| el.child(icon("check", 13.0, theme.accent)))
}

/// Search field + the relay's model list (fixture list when none arrived yet).
fn model_list(shell: &Shell, project: &Project, theme: &Theme, cx: &mut Context<Shell>) -> Div {
    let state = shell.state.read(cx);
    let current = project.model.clone();
    let mut names: Vec<String> = if state.models.is_empty() {
        MODELS.iter().map(|m| m.to_string()).collect()
    } else {
        state.models.clone()
    };
    if !current.is_empty() && !names.contains(&current) {
        names.insert(0, current.clone());
    }
    let query = shell
        .model_search_input
        .read(cx)
        .text()
        .trim()
        .to_lowercase();
    if !query.is_empty() {
        names.retain(|n| n.to_lowercase().contains(&query));
    }
    let pid = project.id.clone();
    let mut list = div().flex().flex_col().child(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(7.0))
            .border_b_1()
            .border_color(theme.field_border)
            .text_size(px(TEXT_SMALL))
            .child(icon("search", 13.0, theme.text_faint))
            .child(div().flex_1().child(shell.model_search_input.clone())),
    );
    let mut items = div()
        .id("agent-model-list")
        .flex()
        .flex_col()
        .max_h(px(240.0))
        .overflow_y_scroll();
    if names.is_empty() {
        items = items.child(
            div()
                .px(px(10.0))
                .py(px(8.0))
                .text_size(px(TEXT_TINY))
                .text_color(theme.text_muted)
                .child("No models match."),
        );
    }
    for (i, name) in names.into_iter().enumerate() {
        let on = name == current;
        let p = pid.clone();
        let pick = name.clone();
        items = items.child(
            picker_item(
                ElementId::NamedInteger("agent-model-item".into(), i as u64),
                name,
                on,
                theme,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.state
                    .update(cx, |s, cx| s.set_project_model(p.clone(), pick.clone(), cx));
                this.agent_picker = None;
                this.model_search_input.update(cx, |i, cx| i.clear(cx));
                crate::ui::composer::Composer::focus(&this.composer, window, cx);
                cx.notify();
            })),
        );
    }
    list = list.child(items);
    list
}

/// Low · Medium · High.
fn effort_list(project: &Project, theme: &Theme, cx: &mut Context<Shell>) -> Div {
    let pid = project.id.clone();
    let mut list = div().flex().flex_col();
    for (i, level) in Effort::ALL.iter().enumerate() {
        let level = *level;
        let p = pid.clone();
        list = list.child(
            picker_item(
                ElementId::NamedInteger("agent-effort-item".into(), i as u64),
                level.label().to_string(),
                level == project.effort,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state
                    .update(cx, |s, cx| s.set_project_effort(p.clone(), level, cx));
                this.agent_picker = None;
                cx.notify();
            })),
        );
    }
    list
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
