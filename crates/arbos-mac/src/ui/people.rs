//! People chats (D72) in the shell: avatars, the New Message sheet, the
//! member chips under a DM header and its pickers, the typing line. Pure
//! rendering over `AppState`; the shell owns the open/closed flags.

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, Context, Div, ElementId, Entity, Focusable as _, FontWeight, RenderImage,
    SharedString, Stateful,
};

use crate::app::AppState;
use crate::model::{AgentMember, Person, Project};
use crate::ui::composer::ComposerInput;
use crate::ui::frost::{frosted, MENU_BLUR};
use crate::ui::glass;
use crate::ui::groups::{project_color, sender_color};
use crate::ui::icons::icon;
use crate::ui::shell::Shell;
use crate::ui::theme::{Theme, PANEL_RADIUS, TEXT_BODY, TEXT_SMALL, TEXT_TINY};

/// Members of a space with their decoded avatars (list rows, headers).
pub type PeopleWithAvatars = Vec<(Person, Option<Arc<RenderImage>>)>;

/// Round avatar: the GitHub picture when decoded, else an initial on a stable colour.
pub fn avatar(image: Option<Arc<RenderImage>>, person: &Person, size: f32, theme: &Theme) -> Div {
    let base = div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center();
    match image {
        Some(image) => base.child(
            gpui::img(image)
                .size(px(size))
                .rounded_full()
                .object_fit(gpui::ObjectFit::Cover),
        ),
        None => {
            let color = sender_color(&person.handle, theme);
            base.bg(Theme::with_alpha(color, 0.25))
                .border_1()
                .border_color(Theme::with_alpha(color, 0.8))
                .text_size(px(size * 0.46))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(person.initial())
        }
    }
}

/// A small square glyph for a shared agent (its Project's icon and colour),
/// or its tree avatar when the agent carries one.
pub fn agent_glyph(agent: &AgentMember, size: f32, theme: &Theme) -> Div {
    let color = project_color(&agent.color, theme.accent);
    if let Some(slug) = agent.avatar.as_deref() {
        return crate::ui::avatars::element(
            Some(slug),
            agent.avatar_color.as_deref(),
            crate::ui::avatars::AvatarState::Idle,
            size,
            &agent.icon,
            color,
            theme,
        );
    }
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded(px(size * 0.28))
        .bg(Theme::with_alpha(color, 0.18))
        .border_1()
        .border_color(Theme::with_alpha(color, 0.8))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.55))
        .text_color(theme.text)
        .child(agent.icon.clone())
}

/// The "New Message" sheet (iPhone / Telegram layout): Cancel · title, a
/// "Search people" field, the "New agent" row (expands into the name field),
/// then everyone on Arbos. A person row starts the DM — or, when the sheet
/// was opened from a project's "Share", invites them into it.
pub fn new_chat_sheet(
    shell: &Shell,
    state: &Entity<AppState>,
    input: &Entity<ComposerInput>,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let query = s.people_query.clone();
    let busy = s.people_busy;
    let error = s.people_error.clone();
    let results: Vec<(Person, Option<Arc<RenderImage>>)> = s
        .people_results
        .iter()
        .map(|p| (p.clone(), s.avatar(&p.avatar_url)))
        .collect();
    let typed = query.trim().trim_start_matches('@').to_string();
    let invite_into = s
        .invite_target
        .as_ref()
        .and_then(|id| s.projects.iter().find(|p| &p.id == id))
        .map(|p| p.name.clone());
    let creating = s.new_project_open;

    let search = div()
        .mx(px(16.0))
        .h(px(40.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded(px(12.0))
        .bg(theme.field)
        .border_1()
        .border_color(theme.field_border)
        .child(icon("search", 15.0, theme.text_muted))
        .child(div().flex_1().min_w_0().child(input.clone()));

    let mut body = div()
        .id("new-chat-body")
        .flex()
        .flex_col()
        .gap(px(14.0))
        .pb(px(18.0))
        .min_h_0()
        .overflow_y_scroll()
        .child(search);

    // "New agent" (and, in the Grok layout, "New group" — groups are v2).
    if invite_into.is_none() {
        let mut agent_row = div()
            .id("new-agent")
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(14.0))
            .py(px(10.0))
            .cursor_pointer()
            .hover(|st| st.bg(theme.surface_2))
            .child(
                glass::circle("new-agent-glyph", 40.0, &theme)
                    .child(icon("sparkles", 17.0, theme.text)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(TEXT_BODY))
                            .text_color(theme.text)
                            .child("New agent"),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child("Its own workspace on your machine"),
                    ),
            )
            .child(icon(
                if creating {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                13.0,
                theme.text_faint,
            ))
            .on_click(cx.listener(|this, _, window, cx| {
                let open = !this.state.read(cx).new_project_open;
                this.state.update(cx, |s, cx| {
                    s.new_project_open = open;
                    cx.notify();
                });
                let handle = if open {
                    this.project_input.read(cx).focus_handle(cx)
                } else {
                    this.people_input.read(cx).focus_handle(cx)
                };
                window.focus(&handle, cx);
            }));
        if creating {
            agent_row = agent_row.border_b_1().border_color(theme.border);
        }
        let mut group = glass::group(&theme).mx(px(16.0)).child(agent_row);
        if creating {
            group = group.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .child(
                        div()
                            .flex_1()
                            .h(px(38.0))
                            .px(px(12.0))
                            .flex()
                            .items_center()
                            .rounded(px(10.0))
                            .bg(theme.field)
                            .border_1()
                            .border_color(theme.field_border)
                            .child(shell.project_input.clone()),
                    )
                    .child(
                        glass::primary_button("new-agent-create", "Create", &theme)
                            .h(px(34.0))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit_project(window, cx)),
                            ),
                    ),
            );
        }
        body = body.child(group);
    }

    // People.
    let mut list = glass::group(&theme).mx(px(16.0));
    let mut any = false;
    if let Some(err) = error {
        any = true;
        list = list.child(
            div()
                .px(px(14.0))
                .py(px(10.0))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.danger)
                .child(err),
        );
    }
    if results.is_empty() && !typed.is_empty() && !busy {
        any = true;
        list = list.child(
            div()
                .px(px(14.0))
                .py(px(10.0))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(format!(
                    "No one on Arbos matches @{typed} yet. Enter sends an invite by GitHub login."
                )),
        );
    }
    if busy && results.is_empty() {
        any = true;
        list = list.child(
            div()
                .px(px(14.0))
                .py(px(10.0))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_faint)
                .child("Searching…"),
        );
    }
    let count = results.len();
    for (i, (person, image)) in results.into_iter().enumerate() {
        any = true;
        let handle = person.handle.clone();
        let invited = person.status == "invited";
        list = list.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "person-{}",
                    person.id
                ))))
                .flex()
                .items_center()
                .gap(px(12.0))
                .px(px(14.0))
                .py(px(9.0))
                .when(i + 1 < count, |el| {
                    el.border_b_1().border_color(theme.border)
                })
                .cursor_pointer()
                .hover(|st| st.bg(theme.surface_2))
                .child(avatar(image, &person, 40.0, &theme))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(px(TEXT_BODY))
                                .text_color(theme.text)
                                .truncate()
                                .child(person.display_name()),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_SMALL))
                                .text_color(theme.text_muted)
                                .child(format!("@{}", person.handle)),
                        ),
                )
                .child(if invited {
                    glass::chip("Invite by GitHub", theme.text_muted)
                } else {
                    glass::chip("On Arbos", theme.online)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state
                        .update(cx, |s, cx| s.pick_person(handle.clone(), cx));
                })),
        );
    }
    if any {
        body = body.child(
            div()
                .flex()
                .flex_col()
                .child(
                    glass::section_label(
                        if typed.is_empty() {
                            "Arbos users"
                        } else {
                            "Results"
                        },
                        &theme,
                    )
                    .px(px(30.0)),
                )
                .child(list),
        );
    }
    if let Some(name) = &invite_into {
        body = body.child(
            div()
                .px(px(30.0))
                .text_size(px(TEXT_TINY))
                .text_color(theme.text_faint)
                .child(format!(
                    "They see every chat and the workspace output of {name}; your agent answers them too."
                )),
        );
    }

    let title = match &invite_into {
        Some(name) => format!("Invite to {name}"),
        None => "New Message".to_string(),
    };
    let header = glass::sheet_header(
        &title,
        Some(
            glass::sheet_button("new-chat-close", "Cancel", &theme).on_click(cx.listener(
                |this, _, window, cx| {
                    this.state.update(cx, |s, cx| {
                        s.new_project_open = false;
                        s.close_new_chat(cx)
                    });
                    crate::ui::composer::Composer::focus(&this.composer, window, cx);
                },
            )),
        ),
        None,
        &theme,
    );
    glass::sheet(460.0, &theme, header, body.max_h(px(620.0)))
}

/// Chips under the header of a people chat: each agent in the room and
/// "+ Add agent".
pub fn member_chips(
    space: &Project,
    add_agent_open: bool,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Stateful<Div> {
    let mut row = div()
        .id("member-chips")
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(16.0))
        .pb(px(8.0))
        .overflow_x_scroll();
    for agent in &space.agents {
        row = row.child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .h(px(30.0))
                .pl(px(5.0))
                .pr(px(12.0))
                .rounded_full()
                .bg(theme.bubble_assistant)
                .child(agent_glyph(agent, 22.0, theme))
                .child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text)
                        .child(format!("@{}", agent.handle)),
                )
                .child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.text_muted)
                        .child(format!("· @{}", agent.owner_handle)),
                ),
        );
    }
    row.child(
        div()
            .id("add-agent")
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .h(px(30.0))
            .px(px(12.0))
            .rounded_full()
            .bg(if add_agent_open {
                theme.bubble
            } else {
                theme.bubble_assistant
            })
            .text_size(px(TEXT_SMALL))
            .text_color(if add_agent_open {
                theme.text
            } else {
                theme.text_muted
            })
            .cursor_pointer()
            .hover(|s| s.text_color(theme.text))
            .child(icon("plus", 12.0, theme.text_muted))
            .child("Add agent")
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.add_agent_open = !this.add_agent_open;
                    this.members_open = false;
                    this.menu_open = false;
                    cx.notify();
                }),
            ),
    )
}

/// Under "+ Add agent": my projects not yet in the chat.
pub fn add_agent_picker(
    space: &Project,
    state: &Entity<AppState>,
    right: f32,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let chat_id = space.id.clone();
    let candidates: Vec<Project> = s
        .my_projects()
        .filter(|p| !space.agents.iter().any(|a| a.project_id == p.id))
        .cloned()
        .collect();
    let mut card = div()
        .w(px(260.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(6.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child("ADD ONE OF YOUR AGENTS"),
        );
    if candidates.is_empty() {
        card = card.child(
            div()
                .px(px(8.0))
                .py(px(6.0))
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child("Every project of yours is already here."),
        );
    }
    for p in candidates {
        let pid = p.id.clone();
        let chat = chat_id.clone();
        let color = project_color(&p.color, theme.accent);
        card = card.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "add-agent-{}",
                    p.id
                ))))
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(
                    div()
                        .size(px(22.0))
                        .rounded(px(6.0))
                        .bg(Theme::with_alpha(color, 0.18))
                        .border_1()
                        .border_color(Theme::with_alpha(color, 0.8))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.0))
                        .text_color(theme.text)
                        .child(p.icon.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(p.name.clone()),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child(format!("@{}", Project::mention_slug(&p.name))),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.add_agent_open = false;
                    this.state
                        .update(cx, |s, cx| s.add_agent(chat.clone(), pid.clone(), cx));
                })),
        );
    }
    card = card.child(
        div()
            .px(px(8.0))
            .pt(px(4.0))
            .text_size(px(10.5))
            .text_color(theme.text_faint)
            .child("Mention it (@slug) to get an answer. Everyone here can ask it anything your project can do."),
    );
    div()
        .absolute()
        .top(px(crate::ui::theme::TAB_BAR_HEIGHT + 48.0))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

/// Member list: people (Block / Remove), agents (owner handle, Remove when mine), Leave.
pub fn members_popover(
    space: &Project,
    state: &Entity<AppState>,
    right: f32,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let me = s.me_handle();
    let me_id = s.me_id();
    let chat_id = space.id.clone();
    let mut card = div()
        .w(px(300.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(6.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child("MEMBERS"),
        );
    let action = |id: ElementId, label: &'static str, danger: bool, theme: &Theme| {
        div()
            .id(id)
            .px(px(7.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .text_size(px(11.0))
            .text_color(if danger {
                theme.danger
            } else {
                theme.text_muted
            })
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(label)
    };
    // Me first.
    card = card.child(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(5.0))
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .child(format!("@{me} (you)")),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child("owner"),
            ),
    );
    for p in &space.people {
        let image = s.avatar(&p.avatar_url);
        let handle = p.handle.clone();
        let user_id = p.id.clone();
        let chat_a = chat_id.clone();
        let chat_b = chat_id.clone();
        let open_handle = p.handle.clone();
        let open_space = chat_id.clone();
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "member-open-{}",
                            p.id
                        ))))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|st| st.bg(theme.surface_2))
                        .child(avatar(image, p, 22.0, &theme))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child(format!("@{}", p.handle)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.members_open = false;
                            let (handle, sid) = (open_handle.clone(), open_space.clone());
                            this.state.update(cx, |s, cx| {
                                s.open_profile(
                                    crate::app::ProfilePage::User {
                                        handle,
                                        space_id: Some(sid),
                                    },
                                    cx,
                                )
                            });
                        })),
                )
                .child(
                    action(
                        ElementId::Name(SharedString::from(format!("block-{}", p.id))),
                        "Block",
                        false,
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.members_open = false;
                        let h = handle.clone();
                        let c = chat_a.clone();
                        this.state.update(cx, |s, cx| {
                            s.block_user(h, cx);
                            s.leave_chat(c, cx);
                        });
                    })),
                )
                .child(
                    action(
                        ElementId::Name(SharedString::from(format!("remove-{}", p.id))),
                        "Remove",
                        true,
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.members_open = false;
                        let u = user_id.clone();
                        let c = chat_b.clone();
                        this.state.update(cx, |s, cx| s.remove_member(c, u, cx));
                    })),
                ),
        );
    }
    for a in &space.agents {
        let mine = a.owner_id == me_id || a.owner_handle == me;
        let pid = a.project_id.clone();
        let chat = chat_id.clone();
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "agent-open-{}",
                            a.project_id
                        ))))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|st| st.bg(theme.surface_2))
                        .child(agent_glyph(a, 22.0, &theme))
                        .on_click({
                            let pid = a.project_id.clone();
                            cx.listener(move |this, _, _, cx| {
                                this.members_open = false;
                                let pid = pid.clone();
                                this.state.update(cx, |s, cx| {
                                    s.open_profile(
                                        crate::app::ProfilePage::Agent { project_id: pid },
                                        cx,
                                    )
                                });
                            })
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child(format!("{} · @{}", a.name, a.handle)),
                        )
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme.text_faint)
                                .child(format!(
                                    "owned by @{} · {}",
                                    a.owner_handle,
                                    match a.reply_policy {
                                        crate::model::ReplyPolicy::Mention =>
                                            "answers when mentioned",
                                        crate::model::ReplyPolicy::EveryMessage =>
                                            "answers every message",
                                    }
                                )),
                        ),
                )
                .when(mine, |el| {
                    el.child(
                        action(
                            ElementId::Name(SharedString::from(format!(
                                "remove-agent-{}",
                                a.project_id
                            ))),
                            "Remove",
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.members_open = false;
                            let p = pid.clone();
                            let c = chat.clone();
                            this.state.update(cx, |s, cx| s.remove_agent(c, p, cx));
                        })),
                    )
                }),
        );
    }
    let chat_leave = chat_id.clone();
    card = card.child(div().h(px(1.0)).my(px(4.0)).bg(theme.border)).child(
        div()
            .flex()
            .items_center()
            .px(px(8.0))
            .py(px(4.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(if space.agents.is_empty() {
                        "Nobody's agent reads this chat."
                    } else {
                        "Agents answer with their owner's tools and credits; any member may approve their cards."
                    }),
            )
            .child(
                action(ElementId::Name("leave-chat".into()), "Leave", true, &theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.members_open = false;
                        let c = chat_leave.clone();
                        this.state.update(cx, |s, cx| s.leave_chat(c, cx));
                    }),
                ),
            ),
    );
    div()
        .absolute()
        .top(px(crate::ui::theme::TAB_BAR_HEIGHT + 48.0))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}

/// "@sam is typing…" above the composer (Grok's muted system line).
pub fn typing_line(handle: &str, theme: &Theme) -> Div {
    div().w_full().flex().justify_center().pb(px(4.0)).child(
        div()
            .w(px(crate::ui::theme::COMPOSER_WIDTH))
            .max_w_full()
            .px(px(crate::ui::theme::PLUS_BUTTON + 26.0))
            .text_size(px(TEXT_TINY))
            .text_color(theme.text_muted)
            .child(format!("@{handle} is typing…")),
    )
}

/// Stacked avatars for a project with people in it (tile corner / header).
pub fn avatar_stack(
    people: &[(Person, Option<Arc<RenderImage>>)],
    size: f32,
    max: usize,
    theme: &Theme,
) -> Div {
    let mut row = div().flex().items_center();
    for (i, (p, image)) in people.iter().take(max).enumerate() {
        let image = image.clone();
        row = row.child(
            div()
                .when(i > 0, |el| el.ml(px(-size * 0.3)))
                .rounded_full()
                .border_1()
                .border_color(theme.bg)
                .child(avatar(image, p, size, theme)),
        );
    }
    if people.len() > max {
        row = row.child(
            div()
                .ml(px(2.0))
                .text_size(px(size * 0.5))
                .text_color(theme.text_faint)
                .child(format!("+{}", people.len() - max)),
        );
    }
    row
}

/// Members of a project: the owner, the invited people (Remove for the owner).
pub fn project_members_popover(
    project: &Project,
    state: &Entity<AppState>,
    right: f32,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let me = s.me_handle();
    let project_id = project.id.clone();
    let mut card = div()
        .w(px(280.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .p(px(6.0))
        .rounded(px(PANEL_RADIUS))
        .bg(theme.card())
        .border_1()
        .border_color(theme.border_strong)
        .shadow_lg()
        .child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .text_size(px(11.0))
                .text_color(theme.text_faint)
                .child(format!("PEOPLE IN {}", project.name.to_uppercase())),
        );
    if !project.is_guest() {
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.5))
                        .text_color(theme.text)
                        .child(format!("@{me} (you)")),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child("owner"),
                ),
        );
    }
    for p in &project.people {
        let image = s.avatar(&p.avatar_url);
        let user_id = p.id.clone();
        let pid = project_id.clone();
        let is_owner = p.role == "owner";
        let open_handle = p.handle.clone();
        let open_space = project_id.clone();
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(5.0))
                .child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "pmember-open-{}",
                            p.id
                        ))))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .hover(|st| st.bg(theme.surface_2))
                        .child(avatar(image, p, 22.0, &theme))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child(format!("@{}", p.handle)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.members_open = false;
                            let (handle, sid) = (open_handle.clone(), open_space.clone());
                            this.state.update(cx, |s, cx| {
                                s.open_profile(
                                    crate::app::ProfilePage::User {
                                        handle,
                                        space_id: Some(sid),
                                    },
                                    cx,
                                )
                            });
                        })),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child(if is_owner { "owner" } else { "member" }),
                )
                .when(!project.is_guest() && !is_owner, |el| {
                    el.child(
                        div()
                            .id(ElementId::Name(SharedString::from(format!(
                                "remove-member-{}",
                                p.id
                            ))))
                            .px(px(7.0))
                            .py(px(2.0))
                            .rounded(px(5.0))
                            .text_size(px(11.0))
                            .text_color(theme.danger)
                            .cursor_pointer()
                            .hover(|st| st.bg(theme.surface_2))
                            .child("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.members_open = false;
                                let u = user_id.clone();
                                let p = pid.clone();
                                this.state
                                    .update(cx, |s, cx| s.remove_project_member(p, u, cx));
                            })),
                    )
                }),
        );
    }
    card = card.child(
        div()
            .px(px(8.0))
            .pt(px(4.0))
            .text_size(px(10.5))
            .text_color(theme.text_faint)
            .child(if project.is_guest() {
                "You can chat and drive the agent here; settings and members stay with the owner."
            } else {
                "Members see every tab and the workspace output, and the agent answers them as it answers you."
            }),
    );
    div()
        .absolute()
        .top(px(crate::ui::theme::TAB_BAR_HEIGHT + 6.0))
        .right(px(right))
        .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
}
