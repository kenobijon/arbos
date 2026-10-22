//! A channel's pane (D88, #1317) on the shared profile pane (D93): what the `# general` header
//! pill opens. Minimal until the approved channel profile (Profile | Settings) lands: the `#`
//! glyph tile, the name with a lock badge when admins-only, the topic, who posts, the server's
//! member count, and a row that pushes the server's pane.

use gpui::{div, prelude::*, px, AnyElement, Context, Div, Stateful};

use crate::app::ProfilePage;
use crate::ui::profiles::kit;
use crate::ui::shell::Shell;
use crate::ui::sidebar::server_picture;
use crate::ui::theme::{Theme, TEXT_BODY};

use super::pane::{self, Hero, Picture, ProfilePane, Section};

const TABS: [&str; 0] = [];

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

    let count = server.member_count.max(server.members.len() as u64);
    let hero = Hero {
        picture: Picture::Glyph("hash"),
        name: pane::name(channel.name.clone(), &theme),
        badge: restricted.then_some("lock"),
        subtitle: Some(
            format!(
                "{count} member{} · {}",
                if count == 1 { "" } else { "s" },
                server.name
            )
            .into(),
        ),
        subtitle_color: theme.text_muted,
    };

    let mut about: Vec<AnyElement> = Vec::new();
    if !channel.topic.trim().is_empty() {
        about.push(
            div()
                .px(px(kit::INSET))
                .py(px(12.0))
                .text_size(px(TEXT_BODY))
                .text_color(theme.text)
                .child(channel.topic.clone())
                .into_any_element(),
        );
    }
    about.push(
        kit::value_row(
            "channel-privacy",
            "Posting",
            if restricted {
                "Admins only"
            } else {
                "Everyone"
            },
            &theme,
        )
        .into_any_element(),
    );

    let page = ProfilePage::Server {
        server_id: server.id.clone(),
    };
    let server_row = kit::row(
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
        this.push_profile(page.clone(), cx);
    }))
    .into_any_element();

    Some(pane::render(
        ProfilePane {
            page: ProfilePage::Channel {
                server_id: server.id.clone(),
                channel_id: channel.id.clone(),
            },
            hero,
            tabs: &TABS,
            selected: 0,
            body: vec![Section::Card(about), Section::Card(vec![server_row])],
            footer: Vec::new(),
            done: None,
        },
        shell,
        cx,
    ))
}
