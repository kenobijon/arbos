//! A server's pane (D88 rules 5 and 6): the group pane's skeleton — big disc, name, a tab
//! pill — with **Channels**, **Members** and **Settings**. Owners and admins rename the server,
//! add / rename / delete channels and flip "Admins only post", change roles and remove or ban
//! members, and see the invite link (the owner resets it). Members read everything and can
//! leave. Nothing here says "Matrix".

use std::sync::Arc;

use gpui::{
    div, prelude::*, px, Context, Div, ElementId, FontWeight, RenderImage, SharedString, Stateful,
};

use crate::app::{ProfilePage, ServerSheet};
use crate::model::{Person, ServerChannel, ServerInfo};
use crate::ui::glass;
use crate::ui::icons::icon;
use crate::ui::people;
use crate::ui::shell::Shell;
use crate::ui::sidebar::server_picture;
use crate::ui::theme::{Theme, TEXT_BODY, TEXT_SMALL};

use super::{
    card, editable_name, editable_picture, empty, frame, hero, kit, list_row, tab_bar, HeroClick,
    HERO_AVATAR,
};

const TABS: [&str; 3] = ["Channels", "Members", "Settings"];

/// Owner first, then admins, then members, each group by handle.
fn sorted_members(server: &ServerInfo) -> Vec<Person> {
    let rank = |m: &Person| match m.role.as_str() {
        "owner" => 0,
        "admin" => 1,
        _ => 2,
    };
    let mut out = server.members.clone();
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.handle.cmp(&b.handle)));
    out
}

fn role_word(role: &str) -> Option<&'static str> {
    match role {
        "owner" => Some("Owner"),
        "admin" => Some("Admin"),
        _ => None,
    }
}

pub(super) fn page(
    server_id: &str,
    shell: &Shell,
    cx: &mut Context<Shell>,
) -> Option<Stateful<Div>> {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let server = state.server(server_id)?.clone();
    let tab = state.profile_tab;
    let me_id = state.me_id();
    let can_admin = server.can_admin();
    let is_owner = server.is_owner();
    let renaming = state.profile_editing_name && can_admin;
    let members = sorted_members(&server);
    let images: Vec<Option<Arc<RenderImage>>> = members
        .iter()
        .map(|m| state.avatar(&m.avatar_url))
        .collect();
    let selected_channel = state
        .selected_channel
        .as_ref()
        .filter(|c| c.server_id == server.id)
        .map(|c| c.channel_id.clone());
    let channel_rename = shell.channel_rename.clone();
    let member_armed = shell.server_member_armed.clone();
    let leave_armed = shell.server_leave_armed;

    // ----- hero: the disc, the name as an inline field for admins ---------------
    // The picture stays as it is: the relay's blob slot for server images is not
    // open to the apps yet, so there is no crayon here.
    let picture = server_picture(&server, HERO_AVATAR, &theme).into_any_element();
    let avatar = editable_picture("profile-avatar", picture, None).into_any_element();
    let sid_name = server.id.clone();
    let name_click = can_admin.then(|| {
        let f = cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
            this.start_rename_server(sid_name.clone(), window, cx);
        });
        Box::new(f) as HeroClick
    });
    let name = editable_name(
        "profile-name",
        &server.name,
        renaming,
        &shell.agent_name_input,
        name_click,
        &theme,
    )
    .into_any_element();
    let count = if members.is_empty() {
        server.member_count as usize
    } else {
        members.len()
    };
    let subtitle = format!("{count} member{}", if count == 1 { "" } else { "s" });
    let hero = hero(avatar, name, &subtitle, theme.text_muted, &theme);

    // ----- tabs -----------------------------------------------------------------
    let tabs = tab_bar(&TABS, tab, &theme, cx);
    let body_tab = match tab {
        0 => channels_tab(
            &server,
            selected_channel.as_deref(),
            channel_rename.as_deref(),
            shell,
            &theme,
            cx,
        ),
        1 => members_tab(
            &server,
            &members,
            &images,
            &me_id,
            member_armed.as_deref(),
            &theme,
            cx,
        ),
        _ => settings_tab(&server, is_owner, can_admin, leave_armed, &theme, cx),
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

/// Channels: "Add channel" for admins, then one row per channel — `#name`, a lock word when
/// admins-only; admins get the "Admins only" switch and a hover ✕ (never on `#general`), and
/// a double-click renames in place.
fn channels_tab(
    server: &ServerInfo,
    selected: Option<&str>,
    renaming: Option<&str>,
    shell: &Shell,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Div {
    let can_admin = server.can_admin();
    let mut add = None;
    if can_admin {
        let sid = server.id.clone();
        add = Some(
            kit::row(
                "profile-add-channel",
                kit::Leading::Glyph("plus"),
                "Add channel",
                None,
                None,
                None,
                false,
                theme,
            )
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_server_sheet(
                    ServerSheet::NewChannel {
                        server_id: sid.clone(),
                    },
                    window,
                    cx,
                );
            })),
        );
    }
    let channels: Vec<ServerChannel> = server.ordered_channels().into_iter().cloned().collect();
    if channels.is_empty() {
        return div()
            .flex()
            .flex_col()
            .children(add.map(|row| card(theme).child(row)))
            .child(empty("No channels yet", theme));
    }
    let general_id = server.general().map(|c| c.id.clone());
    let mut list = card(theme);
    for ch in channels {
        let key = ch.id.clone();
        let is_selected = selected == Some(ch.id.as_str());
        if renaming == Some(ch.id.as_str()) {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(kit::GAP))
                    .min_h(px(kit::ROW_MIN_H))
                    .px(px(kit::INSET))
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .size(px(kit::AVATAR))
                            .flex_shrink_0()
                            .rounded(px(10.0))
                            .bg(theme.surface_2)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("hash", 18.0, theme.text_muted)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(30.0))
                            .px(px(8.0))
                            .flex()
                            .items_center()
                            .rounded(px(8.0))
                            .bg(theme.field)
                            .border_1()
                            .border_color(theme.primary)
                            .text_size(px(TEXT_BODY))
                            .child(shell.server_input.clone()),
                    ),
            );
            continue;
        }
        let subtitle: Option<SharedString> = ch.restricted.then(|| "Admins only post".into());
        let is_general = general_id.as_deref() == Some(ch.id.as_str());
        let trailing = can_admin.then(|| {
            let (sid, cid, on) = (server.id.clone(), ch.id.clone(), ch.restricted);
            let mut controls = div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .flex_shrink_0()
                .child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "channel-restricted-{key}"
                        ))))
                        .cursor_pointer()
                        .child(glass::switch(on, theme))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            let (sid, cid) = (sid.clone(), cid.clone());
                            this.state
                                .update(cx, |s, cx| s.set_channel_restricted(sid, cid, !on, cx));
                        })),
                );
            if !is_general {
                let (sid, cid) = (server.id.clone(), ch.id.clone());
                let group = SharedString::from(format!("channel-row-{key}"));
                controls = controls.child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "channel-delete-{key}"
                        ))))
                        .size(px(24.0))
                        .flex_shrink_0()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.0)
                        .group_hover(group, |s| s.opacity(1.0))
                        .hover(|s| s.bg(theme.surface_2))
                        .child(icon("xmark", 11.0, theme.text_muted))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            // Deleting a channel is one click: the row is gone at once and the
                            // relay confirms; `#general` cannot go (no ✕ on it).
                            let (sid, cid) = (sid.clone(), cid.clone());
                            this.state
                                .update(cx, |s, cx| s.delete_channel(sid, cid, cx));
                        })),
                );
            }
            controls.into_any_element()
        });
        let (sid, cid) = (server.id.clone(), ch.id.clone());
        let rename_name = ch.name.clone();
        let rename_id = ch.id.clone();
        list = list.child(
            kit::row(
                ElementId::Name(SharedString::from(format!("profile-channel-{key}"))),
                kit::Leading::Glyph("hash"),
                format!("#{}", ch.name),
                subtitle,
                None,
                trailing,
                false,
                theme,
            )
            .group(SharedString::from(format!("channel-row-{key}")))
            .when(is_selected, |el| el.bg(theme.surface_2))
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_click(cx.listener(
                move |this, event: &gpui::ClickEvent, window, cx| {
                    if can_admin && event.click_count() >= 2 {
                        this.start_rename_channel(
                            rename_id.clone(),
                            rename_name.clone(),
                            window,
                            cx,
                        );
                        return;
                    }
                    let (sid, cid) = (sid.clone(), cid.clone());
                    this.state.update(cx, |s, cx| {
                        s.open_server = Some(sid.clone());
                        s.select_channel(sid, cid, cx);
                    });
                },
            )),
        );
    }
    div()
        .flex()
        .flex_col()
        .children(add.map(|row| card(theme).child(row)))
        .child(list)
        .when(can_admin, |el| {
            el.child(kit::footnote(
                "Double-click a channel to rename it. The switch makes it admins-only.",
                theme,
            ))
        })
}

/// Members: owner first. The owner flips admin ↔ member with the crown and removes or bans
/// with the hover ✕; admins remove or ban members; members only look.
fn members_tab(
    server: &ServerInfo,
    members: &[Person],
    images: &[Option<Arc<RenderImage>>],
    me_id: &str,
    armed: Option<&str>,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Div {
    if members.is_empty() {
        return empty("Loading members…", theme);
    }
    let is_owner = server.is_owner();
    let can_admin = server.can_admin();
    let mut list = card(theme);
    for (i, m) in members.iter().enumerate() {
        let is_me = m.id == me_id;
        let key = m.id.clone();
        if armed == Some(key.as_str()) {
            list = list.child(remove_confirm(server, m, theme, cx));
            continue;
        }
        let title = if is_me {
            format!("{} (you)", m.display_name())
        } else {
            m.display_name()
        };
        let subtitle = match role_word(&m.role) {
            Some(word) => format!("@{} · {word}", m.handle),
            None => format!("@{}", m.handle),
        };
        // The owner cannot be touched; admins touch members only.
        let removable =
            !is_me && m.role != "owner" && (is_owner || (can_admin && m.role != "admin"));
        let promotable = is_owner && !is_me && m.role != "owner";
        let trailing = (removable || promotable).then(|| {
            let group = SharedString::from(format!("member-row-{key}"));
            let mut controls = div().flex().items_center().gap(px(4.0)).flex_shrink_0();
            if promotable {
                let (sid, uid) = (server.id.clone(), m.id.clone());
                let is_admin = m.role == "admin";
                controls = controls.child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "member-role-{key}"
                        ))))
                        .size(px(24.0))
                        .flex_shrink_0()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .when(!is_admin, |el| {
                            el.opacity(0.0)
                                .group_hover(group.clone(), |s| s.opacity(1.0))
                        })
                        .hover(|s| s.bg(theme.surface_2))
                        .child(icon(
                            "crown",
                            12.0,
                            if is_admin {
                                theme.warn
                            } else {
                                theme.text_muted
                            },
                        ))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            let (sid, uid) = (sid.clone(), uid.clone());
                            let role = if is_admin { "member" } else { "admin" };
                            this.state
                                .update(cx, |s, cx| s.set_member_role(sid, uid, role, cx));
                        })),
                );
            }
            if removable {
                let uid = m.id.clone();
                controls = controls.child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "member-remove-{key}"
                        ))))
                        .size(px(24.0))
                        .flex_shrink_0()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.0)
                        .group_hover(group, |s| s.opacity(1.0))
                        .hover(|s| s.bg(theme.surface_2))
                        .child(icon("xmark", 11.0, theme.text_muted))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.server_member_armed = Some(uid.clone());
                            cx.notify();
                        })),
                );
            }
            controls.into_any_element()
        });
        let handle = m.handle.clone();
        list = list.child(
            list_row(
                ElementId::Name(SharedString::from(format!("profile-member-{key}"))),
                people::avatar(images[i].clone(), m, kit::AVATAR, theme).into_any_element(),
                title,
                subtitle,
                theme.text_muted,
                trailing,
                theme,
            )
            .group(SharedString::from(format!("member-row-{key}")))
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .on_click(cx.listener(move |this, _, _, cx| {
                let handle = handle.clone();
                this.state.update(cx, |s, cx| {
                    s.open_profile(
                        ProfilePage::User {
                            handle,
                            space_id: None,
                        },
                        cx,
                    )
                });
            })),
        );
    }
    div().flex().flex_col().child(list)
}

/// "Remove @x from Server?" in the row's place: Cancel keeps, Remove removes, Ban keeps them out.
fn remove_confirm(server: &ServerInfo, m: &Person, theme: &Theme, cx: &mut Context<Shell>) -> Div {
    let (sid, uid) = (server.id.clone(), m.id.clone());
    let (ban_sid, ban_uid) = (server.id.clone(), m.id.clone());
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .px(px(kit::INSET))
        .py(px(10.0))
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .text_size(px(TEXT_SMALL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(format!("Remove @{} from {}?", m.handle, server.name)),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.0))
                .child(
                    kit::confirm_button("server-member-cancel", "Cancel", false, theme).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.server_member_armed = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    kit::confirm_button("server-member-ban", "Ban", false, theme).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.server_member_armed = None;
                            let (sid, uid) = (ban_sid.clone(), ban_uid.clone());
                            this.state
                                .update(cx, |s, cx| s.remove_server_member(sid, uid, true, cx));
                        }),
                    ),
                )
                .child(
                    kit::confirm_button("server-member-go", "Remove", true, theme).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.server_member_armed = None;
                            let (sid, uid) = (sid.clone(), uid.clone());
                            this.state
                                .update(cx, |s, cx| s.remove_server_member(sid, uid, false, cx));
                        }),
                    ),
                ),
        )
}

/// Settings: the invite link (owner/admin; Copy, and Reset for the owner), then Leave server
/// — or Delete server for the owner. Members see only the leave card.
fn settings_tab(
    server: &ServerInfo,
    is_owner: bool,
    can_admin: bool,
    leave_armed: bool,
    theme: &Theme,
    cx: &mut Context<Shell>,
) -> Div {
    let mut body = div().flex().flex_col();
    if can_admin {
        let mut invite = card(theme);
        match &server.invite {
            Some(link) => {
                let url = link.url.clone();
                let copy = glass::sheet_button("server-invite-copy", "Copy", theme)
                    .h(px(28.0))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                        this.state.update(cx, |s, cx| {
                            s.push_notice("Invite link copied");
                            cx.notify();
                        });
                    }));
                invite = invite.child(kit::row(
                    "server-invite",
                    kit::Leading::Tile("link"),
                    "Invite link",
                    Some(SharedString::from(link.url.clone())),
                    None,
                    Some(copy.into_any_element()),
                    false,
                    theme,
                ));
                if is_owner {
                    let sid = server.id.clone();
                    invite = invite.child(
                        kit::row(
                            "server-invite-reset",
                            kit::Leading::None,
                            "Reset invite link",
                            None,
                            None,
                            None,
                            false,
                            theme,
                        )
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.surface_2))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let sid = sid.clone();
                            this.state.update(cx, |s, cx| s.reset_invite(sid, cx));
                        })),
                    );
                }
            }
            None => {
                invite = invite.child(kit::row(
                    "server-invite",
                    kit::Leading::Tile("link"),
                    "Invite link",
                    Some(SharedString::from("Loading…")),
                    None,
                    None,
                    false,
                    theme,
                ));
            }
        }
        body = body.child(invite).child(kit::footnote(
            if is_owner {
                "Anyone with the link joins as a member. Resetting it makes the old link stop working."
            } else {
                "Anyone with the link joins as a member."
            },
            theme,
        ));
    }

    let (label, question, note) = if is_owner {
        (
            "Delete server",
            format!("Delete “{}”?", server.name),
            "Deletes the server and its channels for everyone in it.",
        )
    } else {
        (
            "Leave server",
            format!("Leave “{}”?", server.name),
            "You leave the server. It stays for everyone else.",
        )
    };
    let sid = server.id.clone();
    let leave = if leave_armed {
        card(theme).child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .px(px(kit::INSET))
                .py(px(10.0))
                .child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(question),
                )
                .child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child(note),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(6.0))
                        .child(
                            kit::confirm_button("server-leave-cancel", "Cancel", false, theme)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.server_leave_armed = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            kit::confirm_button(
                                "server-leave-go",
                                if is_owner { "Delete" } else { "Leave" },
                                true,
                                theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.server_leave_armed = false;
                                    let sid = sid.clone();
                                    this.state.update(cx, |s, cx| s.leave_server(sid, cx));
                                    this.close_profile(window, cx);
                                },
                            )),
                        ),
                ),
        )
    } else {
        card(theme).child(
            kit::danger_row("server-leave", label, theme).on_click(cx.listener(
                |this, _, _, cx| {
                    this.server_leave_armed = true;
                    cx.notify();
                },
            )),
        )
    };
    body.child(leave)
}
