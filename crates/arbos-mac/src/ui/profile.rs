//! The agent profile sheet (iPhone `AgentProfileView`): the big avatar with
//! the machine dot, name and @mention, then the Colour row (every palette
//! entry drawn as the current tree) and the Character grid. One click sets
//! the colour or the tree; the relay stores both on the project.

use gpui::{div, prelude::*, px, Context, Div, ElementId, Entity, FontWeight, SharedString};

use crate::app::AppState;
use crate::model::{Id, Project};
use crate::ui::avatars::{self, AvatarState};
use crate::ui::glass;
use crate::ui::groups;
use crate::ui::shell::Shell;
use crate::ui::sidebar::machine_dot;
use crate::ui::theme::{Theme, PANEL_RADIUS, TEXT_BODY, TEXT_SMALL, TEXT_TINY};

pub fn sheet(project: &Project, state: &Entity<AppState>, cx: &mut Context<Shell>) -> Div {
    let theme = Theme::of(cx).clone();
    let s = state.read(cx);
    let current = s.avatar_slug(project);
    let current_color = s.avatar_color(project);
    let avatar_state = s.avatar_state(&project.id);
    let machine = s.machine.shown_state();
    let project_id: Id = project.id.clone();
    let fallback = groups::project_color(&project.color, theme.accent);

    let hero = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.0))
        .pb(px(18.0))
        .child(
            div()
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
                        .bg(theme.surface_1)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .size(px(20.0))
                                .rounded_full()
                                .bg(machine_dot(machine, &theme)),
                        ),
                ),
        )
        .child(
            div()
                .pt(px(8.0))
                .text_size(px(TEXT_BODY + 7.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(project.name.clone()),
        )
        .child(
            div()
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(format!("@{}", Project::mention_slug(&project.name))),
        );

    // Colour row: every palette entry, drawn as the current character.
    let mut colours = div()
        .flex()
        .flex_wrap()
        .gap(px(10.0))
        .px(px(16.0))
        .pb(px(14.0));
    let palette = avatars::palette_for(current_color.as_deref());
    // Mono is the brand mark's look, never an agent's (D74): only real colours.
    for p in avatars::colors().iter().filter(|p| !p.is_mono()) {
        let selected = p.id == palette.id;
        let pid = project_id.clone();
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
                    current.as_deref(),
                    Some(&p.id),
                    AvatarState::Idle,
                    44.0,
                    &project.icon,
                    fallback,
                    &theme,
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let p = pid.clone();
                    let c = id.clone();
                    this.state
                        .update(cx, |st, cx| st.set_avatar_color(p, c, cx));
                })),
        );
    }

    // Character grid in the current colour.
    let mut grid = div()
        .flex()
        .flex_wrap()
        .gap(px(6.0))
        .px(px(10.0))
        .pb(px(10.0));
    for c in avatars::all() {
        let slug = c.slug.clone();
        let selected = current.as_deref() == Some(c.slug.as_str());
        let pid = project_id.clone();
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
                    current_color.as_deref(),
                    AvatarState::Idle,
                    60.0,
                    "·",
                    fallback,
                    &theme,
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
                        // Keep the sheet open: picking a tree then a colour is one visit.
                        st.avatar_picker = Some(pid.clone());
                        cx.notify();
                    });
                })),
        );
    }

    let heading = |text: &str, theme: &Theme| {
        div()
            .px(px(16.0))
            .pt(px(14.0))
            .pb(px(10.0))
            .text_size(px(TEXT_BODY + 1.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text)
            .child(text.to_string())
    };
    let body = div()
        .id("profile-body")
        .flex()
        .flex_col()
        .px(px(16.0))
        .pb(px(16.0))
        .min_h_0()
        .overflow_y_scroll()
        .child(hero)
        .child(
            glass::group(&theme)
                .child(heading("Colour", &theme))
                .child(colours)
                .child(div().h(px(1.0)).mx(px(16.0)).bg(theme.border))
                .child(heading("Character", &theme))
                .child(grid),
        )
        .child(
            div()
                .px(px(16.0))
                .pt(px(10.0))
                .text_size(px(TEXT_TINY))
                .text_color(theme.text_muted)
                .child("Picked for you from the agent's id; choose any tree and colour to make it yours."),
        );

    let header = glass::sheet_header(
        "",
        None,
        Some(
            glass::sheet_button("profile-done", "Done", &theme).on_click(cx.listener(
                |this, _, window, cx| {
                    let id = this.state.read(cx).avatar_picker.clone();
                    if let Some(id) = id {
                        this.state
                            .update(cx, |s, cx| s.toggle_avatar_picker(id, cx));
                    }
                    crate::ui::composer::Composer::focus(&this.composer, window, cx);
                },
            )),
        ),
        &theme,
    );
    glass::sheet(480.0, &theme, header, body.max_h(px(760.0)))
}
