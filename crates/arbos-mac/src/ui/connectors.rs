//! Settings → Connectors (F1): the user's external accounts as the relay knows them.
//! Connect opens the provider's OAuth page in the browser (the relay finishes the loopback
//! callback); token providers take a pasted key; Disconnect deletes the relay-side tokens.

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Render, Window,
};

use crate::app::AppState;
use crate::model::{ConnectorProvider, ConnectorStatus, ConnectorsView};
use crate::ui::composer::ComposerInput;
use crate::ui::frost::{frosted, MENU_BLUR};
use crate::ui::theme::{Theme, PANEL_RADIUS, RAIL_WIDTH};

pub enum ConnectorsEvent {
    /// "‹ Settings": return to the settings popover.
    Back,
}

pub struct ConnectorsPanel {
    state: Entity<AppState>,
    token_input: Entity<ComposerInput>,
    /// Provider the token field is for (shown only while `Some`).
    token_for: Option<String>,
    open: bool,
    focus_handle: FocusHandle,
}

impl EventEmitter<ConnectorsEvent> for ConnectorsPanel {}

impl ConnectorsPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let token_input = cx.new(ComposerInput::new);
        {
            let weak = cx.weak_entity();
            token_input.update(cx, |input, _| {
                input.set_placeholder("Paste a token, then Enter");
                input.on_submit = Some(Rc::new(move |_window, cx| {
                    let _ = weak.update(cx, |this, cx| this.submit_token(cx));
                }));
            });
        }
        cx.observe(&token_input, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            token_input,
            token_for: None,
            open: false,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn show(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        self.state.update(cx, |s, cx| s.refresh_connectors(cx));
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.token_for = None;
        cx.notify();
    }

    /// True while the token field has keyboard focus (the shell must not steal it).
    pub fn input_focused(&self, window: &Window, cx: &App) -> bool {
        self.token_for.is_some()
            && self
                .token_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
    }

    fn submit_token(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.token_for.clone() else {
            return;
        };
        let token = self.token_input.read(cx).text().trim().to_string();
        if token.is_empty() {
            return;
        }
        self.token_input.update(cx, |input, cx| input.clear(cx));
        self.token_for = None;
        self.state
            .update(cx, |s, cx| s.connect_connector(provider, Some(token), cx));
        cx.notify();
    }

    fn chip(
        theme: &Theme,
        text: impl Into<gpui::SharedString>,
        fg: gpui::Hsla,
        bg: gpui::Hsla,
    ) -> Div {
        div()
            .px(px(7.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .text_size(px(10.5))
            .font_weight(FontWeight::SEMIBOLD)
            .bg(bg)
            .text_color(fg)
            .child(text.into())
            .border_1()
            .border_color(theme.border)
    }

    fn button(
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        primary: bool,
        theme: &Theme,
    ) -> gpui::Stateful<Div> {
        div()
            .id(id)
            .px(px(9.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .text_size(px(11.5))
            .cursor_pointer()
            .when(primary, |el| {
                el.bg(theme.text)
                    .text_color(theme.bg)
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .when(!primary, |el| {
                el.text_color(theme.text_muted)
                    .border_1()
                    .border_color(theme.border)
                    .hover(|s| s.bg(theme.surface_2))
            })
            .child(label)
    }

    fn render_row(
        &self,
        view: &ConnectorsView,
        p: &ConnectorProvider,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let cn = view.connector_for(&p.provider);
        let status = cn.map(|c| c.status);
        let (status_text, fg, bg) = match status {
            Some(s @ ConnectorStatus::Connected) => (s.label(), theme.accent, theme.accent_soft),
            Some(s @ ConnectorStatus::NeedsAuth) => (s.label(), theme.warn, theme.surface_2),
            Some(s @ ConnectorStatus::Error) => (s.label(), theme.danger, theme.danger_soft),
            Some(s @ ConnectorStatus::Other) => (s.label(), theme.text_muted, theme.surface_2),
            None => ("Not connected", theme.text_faint, theme.surface_2),
        };
        let approvals = p
            .actions
            .iter()
            .filter(|a| a.kind == "write" || a.kind == "send")
            .count();
        let sub = match cn {
            Some(c) if !c.label.is_empty() => c.label.clone(),
            _ if !p.has_oauth() => "API key: use Token…".to_owned(),
            _ if !p.oauth_configured && !view.mock => {
                "OAuth client not configured on the relay".to_owned()
            }
            _ => format!("{} actions · {approvals} need approval", p.actions.len()),
        };
        let provider = p.provider.clone();
        let connected = matches!(status, Some(ConnectorStatus::Connected));
        let token_open = self.token_for.as_deref() == Some(p.provider.as_str());

        let mut actions = div().flex().items_center().gap(px(5.0));
        if p.has_oauth() && !connected {
            let prov = provider.clone();
            actions = actions.child(
                Self::button(format!("connect-{}", p.provider), "Connect", true, theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.token_for = None;
                        let prov = prov.clone();
                        this.state
                            .update(cx, |s, cx| s.connect_connector(prov, None, cx));
                    }),
                ),
            );
        }
        if p.accepts_token() {
            let prov = provider.clone();
            actions = actions.child(
                Self::button(
                    format!("token-{}", p.provider),
                    if token_open { "Cancel" } else { "Token…" },
                    !p.has_oauth() && !connected,
                    theme,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.token_for.as_deref() == Some(prov.as_str()) {
                        this.token_for = None;
                    } else {
                        this.token_for = Some(prov.clone());
                        let handle = this.token_input.read(cx).focus_handle(cx);
                        window.focus(&handle, cx);
                    }
                    cx.notify();
                })),
            );
        }
        if let Some(c) = cn {
            let id = c.id.clone();
            actions = actions.child(
                Self::button(
                    format!("disconnect-{}", p.provider),
                    "Disconnect",
                    false,
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let id = id.clone();
                    this.state
                        .update(cx, |s, cx| s.disconnect_connector(id, cx));
                })),
            );
        }

        let mut row = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .py(px(7.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(p.label.clone()),
                    )
                    .child(Self::chip(theme, status_text, fg, bg)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(sub),
                    )
                    .child(actions),
            );
        if let Some(err) = cn.and_then(|c| c.last_error.clone()) {
            if !connected {
                row = row.child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.danger)
                        .child(err),
                );
            }
        }
        if token_open {
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .h(px(30.0))
                            .px(px(10.0))
                            .rounded(px(7.0))
                            .bg(theme.surface_2)
                            .border_1()
                            .border_color(theme.border_strong)
                            .child(self.token_input.clone()),
                    )
                    .child(
                        Self::button(format!("save-token-{}", p.provider), "Save", true, theme)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_token(cx))),
                    ),
            );
        }
        row
    }
}

impl Focusable for ConnectorsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConnectorsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let view = self.state.read(cx).connectors.clone();

        let mut header = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .id("connectors-back")
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .child("‹ Settings")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hide(cx);
                        cx.emit(ConnectorsEvent::Back);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("CONNECTORS"),
            );
        if view.mock {
            header = header.child(Self::chip(&theme, "Mock", theme.warn, theme.surface_2));
        }
        header = header.child(
            div()
                .id("connectors-refresh")
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover(|s| s.text_color(theme.text))
                .child("Refresh")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.refresh_connectors(cx));
                })),
        );

        let mut list = div().flex().flex_col();
        if view.providers.is_empty() {
            list = list.child(
                div()
                    .py(px(10.0))
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(if view.loaded {
                        "The relay offers no connectors."
                    } else {
                        "Loading from the relay…"
                    }),
            );
        }
        for p in &view.providers {
            list = list.child(self.render_row(&view, p, &theme, cx));
        }

        let card = div()
            .id("connectors-popover")
            .track_focus(&self.focus_handle)
            .w(px(360.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(10.0))
            .rounded(px(PANEL_RADIUS))
            .bg(theme.card())
            .border_1()
            .border_color(theme.border_strong)
            .shadow_lg()
            .child(header)
            .child(list)
            .child(
                div()
                    .pt(px(4.0))
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(if view.mock {
                        "Mock mode: canned replies; sends land in the relay's connector_outbox. Tokens never leave the relay."
                    } else {
                        "Tokens stay on the relay; the agent acts through POST /connectors/call and asks before it sends."
                    }),
            );
        div()
            .absolute()
            .left(px(RAIL_WIDTH + 8.0))
            .bottom(px(14.0))
            .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
    }
}
