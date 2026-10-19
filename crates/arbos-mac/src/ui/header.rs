//! The conversation header, Grok Bot Mac layout: the sidebar toggle, the
//! avatar and name on the left (click an agent to open its profile), then —
//! for an agent — its chat tabs ("Main", groups with their heads, "×"
//! archives, "+" right after the last tab: a new chat, or a dropdown of the
//! previous chats to bring back). The right edge carries no icons for an
//! agent chat (tester report app-01M5ATBWSY9E1HHBKH2QQVDEZX: no call, camera,
//! browser or "…" buttons); a people chat keeps its "add people" circle.
//! Calls start from the composer (mic + phone); the agent's own settings
//! live on its profile page; the sidebar row's control-click menu holds the
//! rest (share, groups, model, effort, spoken replies). Under the header,
//! one optional chips row: the members of a people chat.

use gpui::{
    anchored, deferred, div, point, prelude::*, px, AnyElement, Context, Div, Edges, ElementId,
    FontWeight, SharedString,
};

use crate::app::ProfilePage;
use crate::model::{Chat, Project};
use crate::ui::avatars;
use crate::ui::composer::Composer;
use crate::ui::glass::{self, WindowDrag};
use crate::ui::groups;
use crate::ui::icons::icon;
use crate::ui::motion::PulseClock;
use crate::ui::people;
use crate::ui::shell::Shell;
use crate::ui::sidebar::machine_dot;
use crate::ui::theme::{
    Theme, AVATAR_HEADER, CONTROL, HEADER_HEIGHT, TEXT_BODY, TEXT_SMALL, TEXT_TINY,
};

pub fn render(
    shell: &Shell,
    space: &Project,
    sidebar_hidden: bool,
    cx: &mut Context<Shell>,
) -> Div {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let is_people = space.is_people();

    // ----- left: avatar + name (click an agent to open its profile) -----------
    let mut title = div()
        .id("header-title")
        .flex()
        .items_center()
        .gap(px(9.0))
        .h(px(34.0))
        .pl(px(4.0))
        .pr(px(10.0))
        .rounded(px(9.0))
        .min_w_0();
    if let Some(person) = space
        .person()
        .filter(|_| is_people && !space.is_people_group())
    {
        let image = state.avatar(&person.avatar_url);
        let handle = person.handle.clone();
        let sid = space.id.clone();
        title = title
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.members_open = false;
                this.add_agent_open = false;
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
            }));
        let sub = if space.pending {
            Some("Invited · opens when they sign in".to_string())
        } else if !space.agents.is_empty() {
            Some(format!(
                "@{} · {} agent{}",
                person.handle,
                space.agents.len(),
                if space.agents.len() == 1 { "" } else { "s" }
            ))
        } else {
            Some(format!("@{}", person.handle))
        };
        title = title
            .child(people::avatar(image, person, AVATAR_HEADER, &theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(TEXT_BODY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .truncate()
                            .child(person.display_name()),
                    )
                    .when_some(sub, |el, s| {
                        el.child(
                            div()
                                .text_size(px(TEXT_TINY - 1.0))
                                .text_color(theme.text_muted)
                                .truncate()
                                .child(s),
                        )
                    }),
            );
    } else if space.is_people_group() {
        // A people group: stacked avatars, name, member count → group page.
        let stack: Vec<(
            crate::model::Person,
            Option<std::sync::Arc<gpui::RenderImage>>,
        )> = space
            .people
            .iter()
            .take(3)
            .map(|p| (p.clone(), state.avatar(&p.avatar_url)))
            .collect();
        let sid = space.id.clone();
        let members = space.people.len() + 1;
        title = title
            .child(people::avatar_stack(&stack, AVATAR_HEADER * 0.8, 3, &theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(TEXT_BODY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .truncate()
                            .child(space.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_TINY - 1.0))
                            .text_color(theme.text_muted)
                            .truncate()
                            .child(format!(
                                "{members} members{}",
                                match space.agents.len() {
                                    0 => String::new(),
                                    n => format!(" · {n} agent{}", if n == 1 { "" } else { "s" }),
                                }
                            )),
                    ),
            )
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.members_open = false;
                this.add_agent_open = false;
                let sid = sid.clone();
                this.state.update(cx, |s, cx| {
                    s.open_profile(ProfilePage::Group { space_id: sid }, cx)
                });
            }));
    } else {
        let avatar_state = state.avatar_state(&space.id);
        let machine = state.machine.shown_state();
        let pid = space.id.clone();
        let can_rename = !space.is_guest();
        let renaming = state
            .renaming_project
            .as_ref()
            .is_some_and(|p| p == &space.id);
        let sub = if space.is_guest() {
            Some(format!(
                "shared by @{}",
                space
                    .owner_handle
                    .clone()
                    .or_else(|| space.owner().map(|o| o.handle.clone()))
                    .unwrap_or_else(|| "owner".into())
            ))
        } else {
            None
        };
        let name_el = if renaming {
            div()
                .id("header-rename")
                .w(px(160.0))
                .h(px(22.0))
                .px(px(6.0))
                .rounded(px(6.0))
                .bg(theme.field)
                .border_1()
                .border_color(theme.field_border)
                .text_size(px(TEXT_BODY))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(shell.agent_name_input.clone())
                .into_any_element()
        } else {
            div()
                .text_size(px(TEXT_BODY))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .truncate()
                .child(space.name.clone())
                .into_any_element()
        };
        title = title
            .child(
                div()
                    .relative()
                    .size(px(AVATAR_HEADER))
                    .flex_shrink_0()
                    .child(avatars::project(space, AVATAR_HEADER, &theme))
                    .child(
                        div()
                            .absolute()
                            .right(px(-2.0))
                            .bottom(px(-2.0))
                            .size(px(10.0))
                            .rounded_full()
                            .bg(theme.bg)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(machine_dot(machine, &theme)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(name_el)
                    .when_some(sub, |el, s| {
                        el.child(
                            div()
                                .text_size(px(TEXT_TINY - 1.0))
                                .text_color(theme.text_muted)
                                .truncate()
                                .child(s),
                        )
                    }),
            );
        if avatar_state != avatars::AvatarState::Idle {
            PulseClock::request(cx);
        }
        // A group chat or a project with other people: the group page; my
        // own solo agent: its profile (with the tree + colour picker).
        // Double-click the name to rename (owned agents only).
        let groupish = space.is_groupish();
        title = title
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.members_open = false;
                    this.add_agent_open = false;
                    let p = pid.clone();
                    if can_rename && event.click_count() >= 2 {
                        this.start_rename_agent(p, window, cx);
                        return;
                    }
                    this.state.update(cx, |s, cx| {
                        if groupish {
                            s.open_profile(ProfilePage::Group { space_id: p }, cx);
                        } else {
                            s.toggle_avatar_picker(p, cx);
                        }
                    });
                }),
            );
    }

    // ----- right: icon buttons --------------------------------------------------
    let button = |id: &'static str, glyph: &str, on: bool, theme: &Theme| {
        div()
            .id(id)
            .size(px(CONTROL))
            .flex_shrink_0()
            .rounded(px(8.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(if on {
                theme.surface_2
            } else {
                gpui::transparent_black()
            })
            .hover(|s| s.bg(theme.surface_2))
            .child(icon(
                glyph,
                17.0,
                if on { theme.text } else { theme.text_muted },
            ))
    };
    // An agent chat has nothing here: calls (and hanging up) live on the
    // composer's phone button, everything else on the profile page or the
    // sidebar row's control-click menu. A people chat keeps "add people".
    let mut right = div().flex().items_center().gap(px(2.0));
    if is_people {
        right = right.child(
            button("header-members", "person-plus", shell.members_open, &theme).on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.members_open = !this.members_open;
                    this.add_agent_open = false;
                    cx.notify();
                }),
            ),
        );
    }

    let tabs = if is_people {
        None
    } else {
        agent_tabs(shell, space, cx)
    };
    // Expand only when the rail is hidden (collapsed or too narrow). Collapse
    // lives next to "+" in the sidebar — not in this tab bar.
    let sidebar_toggle = sidebar_hidden.then(|| {
        let overlay = shell.state.read(cx).sidebar_overlay;
        button("header-sidebar", "sidebar-left", overlay, &theme).on_click(cx.listener(
            |this, _, window, cx| {
                let width: f32 = window.viewport_size().width.into();
                this.state.update(cx, |s, cx| {
                    let narrow = s.window_is_narrow(width);
                    s.toggle_rail(narrow, cx)
                });
            },
        ))
    });
    div()
        .h(px(HEADER_HEIGHT))
        .flex_shrink_0()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .pl(px(12.0))
        .pr(px(10.0))
        .border_b_1()
        .border_color(theme.border)
        .window_drag()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .flex_shrink_0()
                .children(sidebar_toggle)
                .child(title.flex_shrink_0().max_w(px(300.0))),
        )
        .when_some(tabs, |el, t| el.child(t))
        .child(right.flex_shrink_0())
}

/// The agent's chat tabs, next to its name: every visible chat as a pill
/// (groups with their participants' heads), "×" on each but the pinned "Main"
/// (archives it), and "+" right after the last tab — a new chat opens there;
/// with previous (archived) chats it drops down a menu to bring one back.
/// With multi-chat off: the primary, groups and the Projects tab only, no
/// "+", no "×".
fn agent_tabs(shell: &Shell, space: &Project, cx: &mut Context<Shell>) -> Option<Div> {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let multi_chat = state.multi_chat;
    let tabs: Vec<(Chat, bool, bool)> = state
        .visible_tabs(&space.id)
        .into_iter()
        .map(|c| {
            (
                c.clone(),
                state.active_tab.get(&space.id) == Some(&c.id),
                state.chat_is_streaming(&c.id),
            )
        })
        .collect();
    let previous = state.archived_tabs(&space.id).len();
    if tabs.is_empty() {
        return None;
    }
    if !multi_chat && tabs.len() < 2 {
        return None;
    }
    let projects = state.projects.clone();
    // The row is as wide as its tabs (it scrolls once they overflow), so "+"
    // sits right beside the last tab instead of at the far end of the header.
    let mut row = div()
        .id("chat-tabs")
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.0))
        .overflow_x_scroll()
        .track_scroll(&shell.tabs_scroll);
    for (chat, active, streaming) in tabs {
        if streaming {
            PulseClock::request(cx);
        }
        let phase = PulseClock::phase(cx, 1200);
        let id = chat.id.clone();
        let close_id = chat.id.clone();
        let menu_chat_id = chat.id.clone();
        let ctrl_chat_id = chat.id.clone();
        let title = if chat.is_pinned {
            "Main".to_string()
        } else {
            chat.title.clone()
        };
        let closable = multi_chat && !chat.is_pinned;
        let mut pill = div()
            .id(ElementId::Name(SharedString::from(format!(
                "tab-{}",
                chat.id
            ))))
            .group("tab")
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .h(px(28.0))
            .pl(px(11.0))
            .pr(px(if closable { 6.0 } else { 11.0 }))
            .rounded_full()
            .bg(if active {
                theme.bubble
            } else {
                theme.bubble_assistant
            })
            .text_size(px(TEXT_SMALL))
            // The active chip is a user bubble (dark in both appearances):
            // white text on it, like the bubbles themselves.
            .text_color(if active {
                theme.bubble_text()
            } else {
                theme.text_muted
            })
            .when(chat.unread && !active, |el| {
                el.font_weight(FontWeight::SEMIBOLD).text_color(theme.text)
            })
            .cursor_pointer()
            .when(!active, |el| el.hover(|s| s.text_color(theme.text)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.chat_menu = None;
                this.state.update(cx, |s, cx| s.select_tab(id.clone(), cx));
                Composer::focus(&this.composer, window, cx);
            }))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu_open = false;
                    this.row_menu = None;
                    this.chat_menu = Some((menu_chat_id.clone(), event.position));
                    cx.notify();
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if !event.modifiers.control {
                        return;
                    }
                    cx.stop_propagation();
                    this.menu_open = false;
                    this.row_menu = None;
                    this.chat_menu = Some((ctrl_chat_id.clone(), event.position));
                    cx.notify();
                }),
            );
        if let Some(glyphs) = groups::tab_glyphs(&chat, &projects, &theme) {
            pill = pill.child(glyphs);
        }
        pill = pill.child(div().max_w(px(160.0)).truncate().child(title));
        if streaming {
            pill = pill.child(
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(Theme::with_alpha(theme.warn, 0.4 + 0.6 * phase)),
            );
        }
        if closable {
            pill = pill.child(
                div()
                    .id(ElementId::Name(SharedString::from(format!(
                        "tab-close-{}",
                        chat.id
                    ))))
                    .size(px(16.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(theme.border_strong))
                    .child(icon(
                        "xmark",
                        9.0,
                        if active {
                            Theme::with_alpha(theme.bubble_text(), 0.8)
                        } else {
                            theme.text_muted
                        },
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.state.update(cx, |s, cx| {
                            s.close_tab(close_id.clone(), cx);
                        });
                    })),
            );
        }
        row = row.child(pill);
    }
    let mut strip = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(row);
    if multi_chat {
        let open = shell.tab_menu_open;
        let plus = div()
            .id("tab-new")
            .flex_shrink_0()
            .size(px(28.0))
            .rounded_full()
            .bg(if open {
                theme.surface_2
            } else {
                theme.bubble_assistant
            })
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(icon("plus", 13.0, theme.text_muted))
            // Mouse-down, not click: the root closes every popover on
            // mouse-down, so a click handler could never toggle this one shut.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.menu_open = false;
                    if previous == 0 {
                        // Nothing to bring back: "+" opens the new chat directly.
                        this.tab_menu_open = false;
                        this.open_new_tab(window, cx);
                        return;
                    }
                    this.tab_menu_open = !this.tab_menu_open;
                    cx.notify();
                }),
            );
        // The dropdown hangs off the "+" itself: anchored to this wrapper
        // (taffy places it relative to its parent), deferred so it paints over
        // the transcript, kept inside the window.
        let anchor = div()
            .relative()
            .flex()
            .flex_shrink_0()
            .child(plus)
            .when(open, |el| {
                el.child(
                    deferred(
                        anchored()
                            .offset(point(px(0.0), px(34.0)))
                            .snap_to_window_with_margin(Edges::all(px(8.0)))
                            .child(glass::popover(tab_menu(shell, space, cx))),
                    )
                    .with_priority(1),
                )
            });
        strip = strip.child(anchor);
    }
    Some(strip)
}

/// The "+" dropdown: "New chat", then the agent's previous (archived) chats,
/// newest first — clicking one brings it back as a tab.
fn tab_menu(shell: &Shell, space: &Project, cx: &mut Context<Shell>) -> impl IntoElement {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let mut previous: Vec<Chat> = state
        .archived_tabs(&space.id)
        .into_iter()
        .cloned()
        .collect();
    previous.sort_by_key(|c| std::cmp::Reverse(c.last_message_at));
    let mut body = div().flex().flex_col().gap(px(1.0)).child(
        glass::menu_row("tab-menu-new", "square-pencil", "New chat", false, &theme)
            .child(
                div()
                    .text_size(px(TEXT_TINY))
                    .text_color(theme.text_faint)
                    .child("⌘T"),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.tab_menu_open = false;
                this.open_new_tab(window, cx);
            })),
    );
    if !previous.is_empty() {
        body = body.child(glass::menu_divider(&theme)).child(
            div()
                .px(px(12.0))
                .pt(px(4.0))
                .pb(px(2.0))
                .text_size(px(TEXT_TINY))
                .text_color(theme.text_faint)
                .child("Previous chats"),
        );
        let mut list = div()
            .id("tab-menu-previous")
            .flex()
            .flex_col()
            .gap(px(1.0))
            .max_h(px(320.0))
            .overflow_y_scroll();
        for chat in previous {
            let id = chat.id.clone();
            let menu_chat_id = chat.id.clone();
            let ctrl_chat_id = chat.id.clone();
            list = list.child(
                div()
                    .id(ElementId::Name(SharedString::from(format!(
                        "tab-menu-previous-{}",
                        chat.id
                    ))))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .h(px(36.0))
                    .px(px(12.0))
                    .rounded(px(9.0))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    // Right-click / control-click: the chat menu (Delete, Fork)
                    // replaces the dropdown, which would otherwise paint over it.
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.tab_menu_open = false;
                            this.row_menu = None;
                            this.chat_menu = Some((menu_chat_id.clone(), event.position));
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            if !event.modifiers.control {
                                return;
                            }
                            cx.stop_propagation();
                            this.tab_menu_open = false;
                            this.row_menu = None;
                            this.chat_menu = Some((ctrl_chat_id.clone(), event.position));
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(chat.title.clone()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(TEXT_TINY))
                            .text_color(theme.text_faint)
                            .child(crate::ui::sidebar::time_label(chat.last_message_at)),
                    )
                    .on_click(
                        cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                            if event.modifiers().control {
                                return;
                            }
                            this.tab_menu_open = false;
                            let id = id.clone();
                            this.state.update(cx, |s, cx| s.unarchive_chat(id, cx));
                            this.reveal_active_tab(cx);
                            Composer::focus(&this.composer, window, cx);
                        }),
                    ),
            );
        }
        body = body.child(list);
    }
    glass::frosted_menu(260.0, &theme, body)
}

/// Chips under the header: agents + "Add agent" for a people chat. `None` for
/// an agent — its chat tabs live in the header (`agent_tabs`).
pub fn chips(shell: &Shell, space: &Project, cx: &mut Context<Shell>) -> Option<AnyElement> {
    if !space.is_people() {
        return None;
    }
    let theme = Theme::of(cx).clone();
    Some(people::member_chips(space, shell.add_agent_open, &theme, cx).into_any_element())
}
