//! Root view: rail · center column (tab bar, banner, transcript, composer) ·
//! right panel (browser live view, S16) · Details drawer. Layout per
//! `arbos-design.md` §3.2 and the mock.

use std::time::Duration;

use gpui::{
    actions, div, prelude::*, px, Animation, AnimationExt, App, Context, Div, ElementId, Entity,
    FocusHandle, Focusable, FontWeight, Render, SharedString, Stateful, Window,
};

use crate::app::{AppState, DetailsTarget};
use crate::model::{now_ms, BoxState, Chat, Link, Project};
use crate::ui::composer::{Composer, ComposerEvent};
use crate::ui::motion::{ease_out_cubic, PulseClock, COMPOSER_DROP_MS, SHAKE_MS};
use crate::ui::theme::{
    reapply_window_background, Appearance, Surface, Theme, COMPOSER_WIDTH, DRAWER_WIDTH,
    PANEL_RADIUS, RAIL_TILE, RAIL_WIDTH, RAIL_WIDTH_COLLAPSED, RIGHT_PANEL_WIDTH, TAB_BAR_HEIGHT,
    TAB_MIN_WIDTH,
};
use crate::ui::transcript::{Transcript, COLUMN_MAX_WIDTH};

actions!(shell, [CloseTab, NewTab, ToggleRail, TogglePanel, Quit]);

pub struct Shell {
    state: Entity<AppState>,
    transcript: Entity<Transcript>,
    composer: Entity<Composer>,
    focus_handle: FocusHandle,
    settings_open: bool,
    shake_started_at: Option<u64>,
}

impl Shell {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        let composer = cx.new(|cx| Composer::new(state.clone(), cx));
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.subscribe(
            &composer,
            |this, _, event: &ComposerEvent, cx| match event {
                ComposerEvent::Send(text, attachments) => {
                    let text = text.clone();
                    let attachments = attachments.clone();
                    this.state.update(cx, |s, cx| s.send(text, attachments, cx));
                }
                ComposerEvent::Stop => this.state.update(cx, |s, cx| s.stop(cx)),
            },
        )
        .detach();
        Composer::focus(&composer, window, cx);
        Self {
            state,
            transcript,
            composer,
            focus_handle: cx.focus_handle(),
            settings_open: false,
            shake_started_at: None,
        }
    }

    fn close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        let active = self.state.read(cx).active_chat_id();
        if let Some(chat_id) = active {
            let closed = self.state.update(cx, |s, cx| s.close_tab(chat_id, cx));
            if !closed {
                self.shake_started_at = Some(now_ms());
                cx.notify();
            }
        }
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.new_tab(cx));
        Composer::focus(&self.composer, window, cx);
    }

    fn toggle_rail(&mut self, _: &ToggleRail, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.toggle_rail(cx));
    }

    fn toggle_panel(&mut self, _: &TogglePanel, _: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.toggle_right_panel(cx));
    }

    fn set_theme(&mut self, appearance: Appearance, surface: Surface, cx: &mut Context<Self>) {
        cx.set_global(Theme::with(appearance, surface));
        reapply_window_background(cx);
        cx.notify();
    }

    // ----- rail ------------------------------------------------------------

    fn render_rail(&self, collapsed: bool, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let selected = state.selected_project.clone();
        let projects: Vec<(Project, bool, usize)> = state
            .projects
            .iter()
            .map(|p| {
                (
                    p.clone(),
                    state.project_is_streaming(&p.id),
                    state.project_unread(&p.id),
                )
            })
            .collect();
        let width = if collapsed {
            RAIL_WIDTH_COLLAPSED
        } else {
            RAIL_WIDTH
        };

        let mut tiles = div()
            .id("rail-tiles")
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.0))
            .pt(px(if cfg!(target_os = "macos") {
                44.0
            } else {
                10.0
            }))
            .flex_1()
            .overflow_y_scroll();
        for (project, streaming, unread) in projects {
            let is_selected = selected.as_deref() == Some(project.id.as_str());
            let asleep = project.state.is_asleep();
            let id = project.id.clone();
            if streaming {
                PulseClock::request(cx);
            }
            let tile_size = if collapsed { 36.0 } else { RAIL_TILE };
            let mut tile = div()
                .id(ElementId::Name(SharedString::from(format!(
                    "tile-{}",
                    project.id
                ))))
                .relative()
                .w(px(tile_size))
                .h(px(if collapsed { 36.0 } else { RAIL_TILE - 6.0 }))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.0))
                .rounded(px(PANEL_RADIUS))
                .border_1()
                .border_color(if is_selected {
                    theme.accent
                } else {
                    theme.border
                })
                .bg(if is_selected {
                    theme.surface_2
                } else {
                    theme.surface_1
                })
                .cursor_pointer()
                .hover(|s| s.border_color(theme.border_strong))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.state
                        .update(cx, |s, cx| s.select_project(id.clone(), cx));
                    Composer::focus(&this.composer, window, cx);
                }));
            tile = tile.child(
                div()
                    .text_size(px(if collapsed { 16.0 } else { 20.0 }))
                    .text_color(theme.text)
                    .when(asleep, |el| el.opacity(0.6))
                    .child(project.icon.clone()),
            );
            if !collapsed {
                tile = tile.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text)
                        .child(project.name.clone()),
                );
            }
            if asleep {
                tile = tile.child(
                    div()
                        .absolute()
                        .top(px(4.0))
                        .right(px(6.0))
                        .text_size(px(9.0))
                        .text_color(theme.text_faint)
                        .child("zz"),
                );
            }
            if streaming {
                let phase = PulseClock::phase(cx, 1200);
                tile = tile.child(
                    div()
                        .absolute()
                        .top(px(5.0))
                        .left(px(6.0))
                        .size(px(6.0))
                        .rounded_full()
                        .bg(Theme::with_alpha(theme.accent, 0.4 + 0.6 * phase)),
                );
            } else if unread > 0 {
                tile = tile.child(
                    div()
                        .absolute()
                        .top(px(-4.0))
                        .right(px(-4.0))
                        .min_w(px(16.0))
                        .h(px(16.0))
                        .px(px(4.0))
                        .rounded_full()
                        .bg(theme.accent)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.0))
                        .text_color(theme.bg)
                        .font_weight(FontWeight::BOLD)
                        .child(unread.to_string()),
                );
            }
            tiles = tiles.child(tile);
        }

        let bottom = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .pb(px(14.0))
            .child(
                div()
                    .id("rail-new-project")
                    .size(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .text_size(px(18.0))
                    .text_color(theme.text_muted)
                    .hover(|s| s.bg(theme.surface_2))
                    .cursor_pointer()
                    .child("+")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.update(cx, |s, cx| {
                            s.open_details(DetailsTarget::Blob("new-project".into()), cx)
                        });
                    })),
            )
            .child(
                div()
                    .id("rail-settings")
                    .size(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .text_size(px(15.0))
                    .text_color(theme.text_muted)
                    .hover(|s| s.bg(theme.surface_2))
                    .cursor_pointer()
                    .child("⚙")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = !this.settings_open;
                        cx.notify();
                    })),
            );

        div()
            .relative()
            .h_full()
            .w(px(width))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(theme.rail_wash)
            .border_r_1()
            .border_color(theme.border)
            .child(tiles)
            .child(bottom)
    }

    // ----- tab bar -----------------------------------------------------------

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let project = state.selected().cloned();
        let tabs: Vec<(Chat, bool, bool)> = project
            .as_ref()
            .map(|p| {
                state
                    .tabs(&p.id)
                    .iter()
                    .map(|c| {
                        (
                            c.clone(),
                            state.active_tab.get(&p.id) == Some(&c.id),
                            state.chat_is_streaming(&c.id),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rail_collapsed = state.rail_collapsed;
        let panel_open = state.right_panel_open;

        let shake = self
            .shake_started_at
            .map(|t| now_ms().saturating_sub(t))
            .filter(|dt| *dt < SHAKE_MS);
        if shake.is_some() {
            PulseClock::request(cx);
        }

        let mut strip = div()
            .id("tab-strip")
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(2.0))
            .overflow_x_scroll();
        for (chat, active, streaming) in tabs {
            let id = chat.id.clone();
            let close_id = chat.id.clone();
            if streaming {
                PulseClock::request(cx);
            }
            let mut tab = div()
                .id(ElementId::Name(SharedString::from(format!(
                    "tab-{}",
                    chat.id
                ))))
                .group("tab")
                .relative()
                .flex()
                .items_center()
                .gap(px(6.0))
                .h(px(28.0))
                .flex_shrink_0()
                .min_w(px(if chat.is_pinned {
                    0.0
                } else {
                    TAB_MIN_WIDTH.min(96.0)
                }))
                .px(px(10.0))
                .rounded(px(7.0))
                .bg(if active {
                    theme.surface_2
                } else {
                    gpui::transparent_black()
                })
                .text_size(px(12.5))
                .text_color(if active { theme.text } else { theme.text_muted })
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_1))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.state.update(cx, |s, cx| s.select_tab(id.clone(), cx));
                    Composer::focus(&this.composer, window, cx);
                }));
            if chat.is_pinned {
                let shake_offset = shake
                    .map(|dt| {
                        ((dt as f32 / SHAKE_MS as f32) * std::f32::consts::PI * 4.0).sin() * 3.0
                    })
                    .unwrap_or(0.0);
                tab = tab.left(px(shake_offset)).child(
                    div()
                        .text_size(px(14.0))
                        .text_color(theme.text)
                        .child(project.as_ref().map(|p| p.icon.clone()).unwrap_or_default()),
                );
            }
            if streaming {
                let phase = PulseClock::phase(cx, 1200);
                tab = tab.child(
                    div()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(Theme::with_alpha(theme.accent, 0.4 + 0.6 * phase)),
                );
            }
            let title = if chat.is_pinned {
                project
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or(chat.title.clone())
            } else {
                chat.title.clone()
            };
            tab = tab.child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .max_w(px(180.0))
                    .when(chat.unread && !active, |el| {
                        el.font_weight(FontWeight::SEMIBOLD).text_color(theme.text)
                    })
                    .child(title),
            );
            if !chat.is_pinned {
                tab = tab.child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "close-{}",
                            chat.id
                        ))))
                        .size(px(16.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .text_size(px(12.0))
                        .text_color(theme.text_faint)
                        .invisible()
                        .group_hover("tab", |s| s.visible())
                        .hover(|s| s.bg(theme.border_strong).text_color(theme.text))
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.update(cx, |s, cx| {
                                s.close_tab(close_id.clone(), cx);
                            });
                        })),
                );
            }
            strip = strip.child(tab);
        }

        let icon_button = |id: &'static str, glyph: &'static str, active: bool, theme: &Theme| {
            div()
                .id(id)
                .size(px(26.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .text_size(px(14.0))
                .text_color(if active { theme.text } else { theme.text_muted })
                .hover(|s| s.bg(theme.surface_2))
                .cursor_pointer()
                .child(glyph)
        };

        div()
            .h(px(TAB_BAR_HEIGHT))
            .flex_shrink_0()
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .window_control_area(gpui::WindowControlArea::Drag)
            .child(
                icon_button("rail-toggle", "☰", !rail_collapsed, &theme).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.state.update(cx, |s, cx| s.toggle_rail(cx));
                    },
                )),
            )
            .child(strip)
            .child(
                icon_button("panel-toggle", "▣", panel_open, &theme).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.state.update(cx, |s, cx| s.toggle_right_panel(cx));
                    },
                )),
            )
            .child(
                icon_button("new-tab", "+", false, &theme)
                    .text_size(px(18.0))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.state.update(cx, |s, cx| s.new_tab(cx));
                        Composer::focus(&this.composer, window, cx);
                    })),
            )
    }

    // ----- banners (S11–S14) ------------------------------------------------

    fn render_banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let link = state.link;
        let project = state.selected().cloned()?;
        let project_id = project.id.clone();

        let banner = |bg: gpui::Hsla, fg: gpui::Hsla| {
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(16.0))
                .py(px(6.0))
                .bg(bg)
                .text_size(px(12.0))
                .text_color(fg)
        };

        if link == Link::Reconnecting {
            return Some(
                banner(theme.surface_1, theme.text_muted)
                    .child(cards_spinner(&theme, cx))
                    .child("Reconnecting… the transcript stays readable from your local replica."),
            );
        }

        match project.state {
            BoxState::Awake => None,
            BoxState::Waking => {
                PulseClock::request(cx);
                let progress = project.wake_progress.unwrap_or(0.0).clamp(0.0, 1.0);
                let detail = project
                    .state_detail
                    .clone()
                    .unwrap_or_else(|| "Waking sandbox…".to_string());
                Some(
                    banner(theme.surface_1, theme.text)
                        .child(cards_spinner(&theme, cx))
                        .child(detail)
                        .child(
                            div()
                                .flex_1()
                                .h(px(4.0))
                                .rounded_full()
                                .bg(theme.border)
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .bg(theme.accent)
                                        .w(gpui::relative(progress.max(0.02))),
                                ),
                        )
                        .child(
                            div()
                                .text_color(theme.text_faint)
                                .child("your message is queued"),
                        ),
                )
            }
            BoxState::HibernatedLight | BoxState::HibernatedDeep => Some(
                banner(theme.surface_1, theme.text_muted)
                    .child(div().text_color(theme.text_faint).child("zz"))
                    .child("Sleeping · will wake on your next message")
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("banner-wake")
                            .text_color(theme.accent)
                            .cursor_pointer()
                            .child("Wake now")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state
                                    .update(cx, |s, cx| s.wake(project_id.clone(), cx));
                            })),
                    ),
            ),
            BoxState::StoppedByUser => Some(
                banner(theme.surface_1, theme.text_muted)
                    .child(div().text_color(theme.danger).child("■"))
                    .child("Stopped by you · tap to resume")
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("banner-resume")
                            .text_color(theme.accent)
                            .cursor_pointer()
                            .child("Resume")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state
                                    .update(cx, |s, cx| s.wake(project_id.clone(), cx));
                            })),
                    ),
            ),
            BoxState::Error => {
                let details_id = project.id.clone();
                Some(
                    banner(theme.danger_soft, theme.text)
                        .child(div().text_color(theme.danger).child("✗"))
                        .child("Could not wake sandbox.")
                        .child(div().flex_1())
                        .child(
                            div()
                                .id("banner-retry")
                                .text_color(theme.accent)
                                .cursor_pointer()
                                .child("Retry")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state
                                        .update(cx, |s, cx| s.wake(project_id.clone(), cx));
                                })),
                        )
                        .child(
                            div()
                                .id("banner-details")
                                .text_color(theme.text_muted)
                                .cursor_pointer()
                                .child("Details")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.update(cx, |s, cx| {
                                        s.open_details(
                                            DetailsTarget::Project(details_id.clone()),
                                            cx,
                                        )
                                    });
                                })),
                        ),
                )
            }
        }
    }

    // ----- center column ------------------------------------------------------

    fn render_center(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let project = state.selected().cloned();
        let chat_id = state.active_chat_id();
        let ui = chat_id
            .as_deref()
            .map(|id| state.chat_ui(id))
            .unwrap_or_default();
        let has_messages = chat_id
            .as_deref()
            .map(|id| !state.messages_of(id).is_empty())
            .unwrap_or(false);

        let dropped = ui.composer_dropped || has_messages;
        let drop_elapsed = ui
            .drop_started_at
            .map(|t| now_ms().saturating_sub(t))
            .unwrap_or(COMPOSER_DROP_MS + 1);
        let animating = dropped && drop_elapsed < COMPOSER_DROP_MS;
        if animating {
            PulseClock::request(cx);
        }

        let composer_block = div()
            .w_full()
            .max_w(px(COMPOSER_WIDTH.max(COLUMN_MAX_WIDTH - 56.0)))
            .child(self.composer.clone());

        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_tab_bar(cx));
        if let Some(banner) = self.render_banner(cx) {
            column = column.child(banner);
        }

        if !dropped {
            // S1/S2: empty chat, composer centered with the project name and a hint.
            let name = project.as_ref().map(|p| p.name.clone()).unwrap_or_default();
            column = column.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(14.0))
                    .px(px(28.0))
                    .child(
                        div()
                            .text_size(px(22.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "Ask {name} anything. It will plan, delegate, or answer."
                            )),
                    )
                    .child(
                        div()
                            .w(px(COMPOSER_WIDTH))
                            .max_w_full()
                            .pt(px(8.0))
                            .child(self.composer.clone()),
                    ),
            );
        } else {
            // Transcript above, composer pinned at the bottom. On the first
            // send the composer slides down from the center over 220 ms using
            // a relative `top` inset (GPUI has no transform).
            let body = div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(self.transcript.clone());
            let composer_row = div()
                .w_full()
                .flex()
                .justify_center()
                .px(px(28.0))
                .pb(px(18.0))
                .pt(px(8.0))
                .child(composer_block);
            let composer_row = if animating {
                composer_row
                    .relative()
                    .with_animation(
                        "composer-drop",
                        Animation::new(Duration::from_millis(COMPOSER_DROP_MS))
                            .with_easing(ease_out_cubic),
                        |el, t| el.top(px(-240.0 * (1.0 - t))),
                    )
                    .into_any_element()
            } else {
                composer_row.into_any_element()
            };
            column = column.child(body).child(composer_row);
        }
        column
    }

    // ----- right panel (S16) and Details drawer --------------------------------

    fn render_right_panel(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let live = state.live_browser();
        if live.is_some() {
            PulseClock::request(cx);
        }
        let mut panel = div()
            .w(px(RIGHT_PANEL_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.rail_wash);
        panel = panel.child(
            div()
                .h(px(TAB_BAR_HEIGHT))
                .flex()
                .items_center()
                .justify_between()
                .px(px(12.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(12.5))
                .text_color(theme.text)
                .child("Browser live view")
                .child(
                    div()
                        .id("panel-close")
                        .text_color(theme.text_faint)
                        .cursor_pointer()
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.update(cx, |s, cx| s.toggle_right_panel(cx));
                        })),
                ),
        );
        match live {
            Some((title, url, action)) => {
                let phase = PulseClock::phase(cx, 2000);
                panel = panel.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .p(px(12.0))
                        .child(
                            div()
                                .w_full()
                                .h(px(220.0))
                                .rounded(px(8.0))
                                .bg(theme.code_bg)
                                .border_1()
                                .border_color(theme.border)
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child(format!(
                                    "live JPEG stream · frame {}",
                                    (phase * 60.0) as u32
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(theme.text)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child(url),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child(format!("last action: {action}")),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child("Watch only in v1 — the agent drives the page."),
                        ),
                );
            }
            None => {
                panel = panel.child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(8.0))
                        .p(px(16.0))
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child("No browser session is running.")
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme.text_faint)
                                .text_center()
                                .child(
                                    "When a subagent opens the browser, its frames stream here.",
                                ),
                        ),
                );
            }
        }
        panel
    }

    fn render_details(&self, target: &DetailsTarget, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let asleep = state.box_state().map(|s| s.is_asleep()).unwrap_or(false);
        let mut rows: Vec<(String, String)> = Vec::new();
        let title = match target {
            DetailsTarget::Project(id) => {
                if let Some(p) = state.project(id) {
                    rows.push(("Project".into(), p.name.clone()));
                    rows.push(("Box state".into(), format!("{:?}", p.state)));
                    rows.push((
                        "Model".into(),
                        format!("{} · {}", p.model, p.effort.label()),
                    ));
                    rows.push((
                        "Spend today".into(),
                        format!("${:.2} of ${:.2}", p.spend_today_usd, p.spend_cap_usd),
                    ));
                    if let Some(d) = &p.state_detail {
                        rows.push(("Detail".into(), d.clone()));
                    }
                }
                "Project details".to_string()
            }
            DetailsTarget::Message {
                chat_id,
                message_id,
            } => {
                if let Some(m) = state
                    .messages_of(chat_id)
                    .iter()
                    .find(|m| &m.id == message_id)
                {
                    rows.push(("Message".into(), m.id.clone()));
                    rows.push(("Status".into(), format!("{:?}", m.status)));
                    if let Some(t) = m.turn {
                        rows.push(("Turn".into(), format!("{t:?}")));
                    }
                    if let Some(model) = &m.model {
                        rows.push(("Model".into(), model.clone()));
                    }
                    rows.push(("Parts".into(), m.parts.len().to_string()));
                }
                "Message details".to_string()
            }
            DetailsTarget::Blob(key) => {
                if key == "new-project" {
                    rows.push(("Status".into(), "Not in this slice".into()));
                    rows.push((
                        "What happens".into(),
                        "The relay creates a sandbox from the image and registers the Project (M3).".into(),
                    ));
                    "New project".to_string()
                } else {
                    rows.push(("Blob".into(), key.clone()));
                    rows.push((
                        "Full output".into(),
                        if asleep {
                            "needs the box awake".into()
                        } else {
                            "fetched lazily from GET /blob/… (stub)".into()
                        },
                    ));
                    "Details".to_string()
                }
            }
        };

        let mut drawer = div()
            .w(px(DRAWER_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.card())
            .child(
                div()
                    .h(px(TAB_BAR_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(12.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .child(title)
                    .child(
                        div()
                            .id("details-close")
                            .text_color(theme.text_faint)
                            .cursor_pointer()
                            .child("×")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.update(cx, |s, cx| s.close_details(cx));
                            })),
                    ),
            );
        let mut body = div().flex().flex_col().gap(px(10.0)).p(px(12.0));
        for (k, v) in rows {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.text_faint)
                            .child(k),
                    )
                    .child(div().text_size(px(12.5)).text_color(theme.text).child(v)),
            );
        }
        drawer = drawer.child(body);
        drawer
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let source = state.source_name;
        let link = state.link;
        let glass_ok = Theme::glass_available();

        let option = |id: &'static str,
                      label: &'static str,
                      selected: bool,
                      enabled: bool,
                      theme: &Theme| {
            div()
                .id(id)
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(6.0))
                .text_size(px(12.5))
                .bg(if selected {
                    theme.surface_2
                } else {
                    gpui::transparent_black()
                })
                .text_color(if enabled {
                    theme.text
                } else {
                    theme.text_faint
                })
                .when(enabled, |el| {
                    el.cursor_pointer().hover(|s| s.bg(theme.surface_2))
                })
                .child(label)
        };

        div()
            .id("settings-popover")
            .absolute()
            .left(px(RAIL_WIDTH + 8.0))
            .bottom(px(14.0))
            .w(px(260.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .rounded(px(PANEL_RADIUS))
            .bg(theme.card())
            .border_1()
            .border_color(theme.border_strong)
            .shadow_lg()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("APPEARANCE · SURFACE"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        option(
                            "surface-matte",
                            "Matte",
                            theme.surface == Surface::Matte,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let a = Theme::of(cx).appearance;
                            this.set_theme(a, Surface::Matte, cx);
                        })),
                    )
                    .child(
                        option(
                            "surface-glass",
                            "Glass",
                            theme.surface == Surface::Glass,
                            glass_ok,
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if glass_ok {
                                let a = Theme::of(cx).appearance;
                                this.set_theme(a, Surface::Glass, cx);
                            }
                        })),
                    ),
            )
            .when(!glass_ok, |el| {
                el.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child("Glass needs macOS vibrancy; this build uses matte."),
                )
            })
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("APPEARANCE · MODE"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        option(
                            "mode-dark",
                            "Dark",
                            theme.appearance == Appearance::Dark,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let s = Theme::of(cx).surface;
                            this.set_theme(Appearance::Dark, s, cx);
                        })),
                    )
                    .child(
                        option(
                            "mode-light",
                            "Light",
                            theme.appearance == Appearance::Light,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let s = Theme::of(cx).surface;
                            this.set_theme(Appearance::Light, s, cx);
                        })),
                    ),
            )
            .child(div().h(px(1.0)).bg(theme.border))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(format!("Source: {source} · link: {link:?}")),
            )
    }
}

fn cards_spinner(theme: &Theme, cx: &App) -> Div {
    crate::ui::cards::spinner(theme, cx)
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let width: f32 = window.viewport_size().width.into();
        let state = self.state.read(cx);
        let collapsed = state.rail_collapsed || state.window_is_narrow(width);
        let panel_open = state.right_panel_open;
        let details = state.details_open.clone();

        let mut root = div()
            .id("shell-root")
            .key_context("Shell")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::toggle_rail))
            .on_action(cx.listener(Self::toggle_panel))
            .relative()
            .flex()
            .flex_row()
            .size_full()
            .bg(theme.shell())
            .text_color(theme.text)
            .font_family(theme.font_sans)
            .text_size(px(13.0))
            .child(self.render_rail(collapsed, cx))
            .child(self.render_center(cx));
        if panel_open {
            root = root.child(self.render_right_panel(cx));
        }
        if let Some(target) = details {
            root = root.child(self.render_details(&target, cx));
        }
        if self.settings_open {
            root = root.child(self.render_settings(cx));
        }
        root
    }
}
