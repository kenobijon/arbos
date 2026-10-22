//! A channel's pane (D88, #1317): what the `# general` header pill opens. Minimal until the
//! channel profile (Profile | Settings) is approved: the `#` disc, the name, the topic, who
//! can post, how many members the server has, and a row that opens the server's pane.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use crate::app::ProfilePage;
use crate::ui::icons::icon;
use crate::ui::profiles::kit;
use crate::ui::shell::Shell;
use crate::ui::sidebar::server_picture;
use crate::ui::theme::{Theme, TEXT_BODY, TEXT_SMALL};

use super::{card, frame, hero, HERO_AVATAR};

pub(super) fn page(
    server_id: &str,
    channel_id: &str,
    shell: &Shell,
    cx: &mut Context<Shell>,
) -> Option<Stateful<Div>> {
    let theme = Theme::of(cx).clone();
    let state = shell.state.read(cx);
    let server = state.server(server_id)?.clone();
    let channel = server.channel(channel_id)?.clone();
    let restricted =
        channel.restricted || state.channel_row(&channel).is_some_and(|r| r.restricted);

    let disc = div()
        .size(px(HERO_AVATAR))
        .rounded(px(HERO_AVATAR * 0.3))
        .bg(theme.surface_2)
        .flex()
        .items_center()
        .justify_center()
        .child(icon("hash", HERO_AVATAR * 0.5, theme.text_muted))
        .into_any_element();
    let name = div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(TEXT_BODY + 6.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(channel.name.clone()),
        )
        .when(restricted, |el| {
            el.child(icon("lock", 13.0, theme.text_faint))
        })
        .into_any_element();
    let count = server.member_count.max(server.members.len() as u64);
    let subtitle = format!(
        "{count} member{} · {}",
        if count == 1 { "" } else { "s" },
        server.name
    );
    let hero = hero(disc, name, &subtitle, theme.text_muted, &theme);

    let mut about = card(&theme);
    if !channel.topic.trim().is_empty() {
        about = about.child(
            div()
                .px(px(kit::INSET))
                .py(px(10.0))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text)
                .child(channel.topic.clone()),
        );
    }
    about = about.child(kit::value_row(
        "channel-privacy",
        "Posting",
        if restricted {
            "Admins only"
        } else {
            "Everyone"
        },
        &theme,
    ));

    let page = ProfilePage::Server {
        server_id: server.id.clone(),
    };
    let server_row = card(&theme).child(
        kit::row(
            "channel-server",
            kit::Leading::Avatar(server_picture(&server, kit::AVATAR, &theme).into_any_element()),
            server.name.clone(),
            Some("Server".into()),
            None,
            None,
            true,
            &theme,
        )
        .cursor_pointer()
        .hover(|s| s.bg(theme.surface_2))
        .on_click(cx.listener(move |this, _, _, cx| {
            let page = page.clone();
            this.state.update(cx, |s, cx| s.push_profile(page, cx));
        })),
    );

    let body = div()
        .flex()
        .flex_col()
        .gap(px(14.0))
        .pb(px(16.0))
        .child(hero)
        .child(about)
        .child(server_row);
    Some(frame(body, None, Some(&shell.profile_scroll), &theme, cx))
}
