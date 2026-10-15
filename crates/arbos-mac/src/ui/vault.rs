//! Settings → Vault (F8): the user's logins and keys as the relay lists them (names, kinds,
//! allowed hosts; never a value), an Add form whose values go to the relay once, and the box
//! requests waiting for Approve / Deny.

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Render, Window,
};

use crate::app::AppState;
use crate::model::{now_ms, VaultItem, VaultNewItem, VaultRequest, VaultRequestStatus, VaultView};
use crate::ui::composer::ComposerInput;
use crate::ui::frost::{frosted, MENU_BLUR};
use crate::ui::theme::{Theme, PANEL_RADIUS, RAIL_WIDTH};

pub enum VaultEvent {
    /// "‹ Settings": return to the settings popover.
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddKind {
    Login,
    Secret,
    ApiKey,
}

impl AddKind {
    fn as_str(self) -> &'static str {
        match self {
            AddKind::Login => "login",
            AddKind::Secret => "secret",
            AddKind::ApiKey => "api_key",
        }
    }
}

pub struct VaultPanel {
    state: Entity<AppState>,
    open: bool,
    /// The Add form is shown for this kind.
    adding: Option<AddKind>,
    name_input: Entity<ComposerInput>,
    url_input: Entity<ComposerInput>,
    user_input: Entity<ComposerInput>,
    secret_input: Entity<ComposerInput>,
    hosts_input: Entity<ComposerInput>,
    error: Option<String>,
    focus_handle: FocusHandle,
}

impl EventEmitter<VaultEvent> for VaultPanel {}

impl VaultPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let name_input = cx.new(ComposerInput::new);
        let url_input = cx.new(ComposerInput::new);
        let user_input = cx.new(ComposerInput::new);
        let secret_input = cx.new(ComposerInput::new);
        let hosts_input = cx.new(ComposerInput::new);
        name_input.update(cx, |i, _| {
            i.set_placeholder("Name, e.g. github or stripe-live")
        });
        url_input.update(cx, |i, _| {
            i.set_placeholder("Sign-in URL, https://github.com/login")
        });
        user_input.update(cx, |i, _| i.set_placeholder("Username or email"));
        secret_input.update(cx, |i, _| {
            i.set_placeholder("Password or key (sent once, never shown again)");
            i.set_masked(true);
        });
        hosts_input.update(cx, |i, _| {
            i.set_placeholder("Allowed hosts, comma separated: api.example.com, *.example.com")
        });
        {
            let weak = cx.weak_entity();
            hosts_input.update(cx, |input, _| {
                input.on_submit = Some(Rc::new(move |_window, cx| {
                    let _ = weak.update(cx, |this, cx| this.submit_add(cx));
                }));
            });
        }
        for input in [
            &name_input,
            &url_input,
            &user_input,
            &secret_input,
            &hosts_input,
        ] {
            cx.observe(input, |_, _, cx| cx.notify()).detach();
        }
        Self {
            state,
            open: false,
            adding: None,
            name_input,
            url_input,
            user_input,
            secret_input,
            hosts_input,
            error: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn show(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        self.state.update(cx, |s, cx| s.vault_refresh(cx));
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.reset_form(cx);
        cx.notify();
    }

    /// True while one of the form fields has keyboard focus (the shell must not steal it).
    pub fn input_focused(&self, window: &Window, cx: &App) -> bool {
        self.adding.is_some()
            && self
                .inputs()
                .iter()
                .any(|i| i.read(cx).focus_handle(cx).is_focused(window))
    }

    fn inputs(&self) -> [&Entity<ComposerInput>; 5] {
        [
            &self.name_input,
            &self.url_input,
            &self.user_input,
            &self.secret_input,
            &self.hosts_input,
        ]
    }

    fn reset_form(&mut self, cx: &mut Context<Self>) {
        self.adding = None;
        self.error = None;
        for input in self.inputs() {
            input.update(cx, |i, cx| i.clear(cx));
        }
    }

    fn start_add(&mut self, kind: AddKind, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_form(cx);
        self.adding = Some(kind);
        let handle = self.name_input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn submit_add(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.adding else {
            return;
        };
        let name = self.name_input.read(cx).text().trim().to_string();
        let secret = self.secret_input.read(cx).text().to_string();
        let hosts: Vec<String> = self
            .hosts_input
            .read(cx)
            .text()
            .split([',', ' ', '\n'])
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .collect();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            self.error = Some("Name: letters, digits, '-', '_' or '.'.".into());
            cx.notify();
            return;
        }
        if secret.is_empty() {
            self.error = Some(if kind == AddKind::Login {
                "Enter the password.".into()
            } else {
                "Enter the value.".into()
            });
            cx.notify();
            return;
        }
        let mut fields = Vec::new();
        match kind {
            AddKind::Login => {
                let url = self.url_input.read(cx).text().trim().to_string();
                let user = self.user_input.read(cx).text().trim().to_string();
                if user.is_empty() {
                    self.error = Some("Enter the username.".into());
                    cx.notify();
                    return;
                }
                fields.push(("url".to_string(), url));
                fields.push(("username".to_string(), user));
                fields.push(("password".to_string(), secret));
            }
            AddKind::Secret | AddKind::ApiKey => fields.push(("value".to_string(), secret)),
        }
        if hosts.is_empty() {
            self.error = Some(
                "Add at least one allowed host; the agent can only send the value there.".into(),
            );
            cx.notify();
            return;
        }
        let item = VaultNewItem {
            name,
            kind: kind.as_str().into(),
            fields,
            allowed_hosts: hosts,
        };
        self.state.update(cx, |s, cx| s.vault_add_item(item, cx));
        self.reset_form(cx);
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

    fn section_label(theme: &Theme, text: &'static str) -> Div {
        div()
            .pt(px(6.0))
            .text_size(px(10.5))
            .text_color(theme.text_faint)
            .child(text)
    }

    fn ago(ms: i64) -> String {
        let d = (now_ms() as i64 - ms).max(0) / 1000;
        if d < 60 {
            "just now".into()
        } else if d < 3600 {
            format!("{} min ago", d / 60)
        } else if d < 86_400 {
            format!("{} h ago", d / 3600)
        } else {
            format!("{} d ago", d / 86_400)
        }
    }

    fn status_colors(theme: &Theme, s: VaultRequestStatus) -> (gpui::Hsla, gpui::Hsla) {
        match s {
            VaultRequestStatus::Pending => (theme.warn, theme.surface_2),
            VaultRequestStatus::Approved => (theme.accent, theme.accent_soft),
            VaultRequestStatus::Denied => (theme.danger, theme.danger_soft),
            VaultRequestStatus::Expired | VaultRequestStatus::Other => {
                (theme.text_muted, theme.surface_2)
            }
        }
    }

    fn render_request(&self, r: &VaultRequest, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let (fg, bg) = Self::status_colors(theme, r.status);
        let pending = r.status == VaultRequestStatus::Pending;
        let project = self
            .state
            .read(cx)
            .projects
            .iter()
            .find(|p| p.id == r.project_id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| r.project_id.clone());
        let use_label = match r.use_kind.as_str() {
            "browser_fill" => "browser fill",
            "env" => "env placeholder",
            _ => "header placeholder",
        };
        let mut actions = div().flex().flex_shrink_0().items_center().gap(px(5.0));
        if pending {
            let id_a = r.id.clone();
            let id_d = r.id.clone();
            actions = actions
                .child(
                    Self::button(format!("vault-approve-{}", r.id), "Approve", true, theme)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let id = id_a.clone();
                            this.state.update(cx, |s, cx| s.vault_decide(id, true, cx));
                        })),
                )
                .child(
                    Self::button(format!("vault-deny-{}", r.id), "Deny", false, theme).on_click(
                        cx.listener(move |this, _, _, cx| {
                            let id = id_d.clone();
                            this.state.update(cx, |s, cx| s.vault_decide(id, false, cx));
                        }),
                    ),
                );
        } else {
            actions = actions.child(Self::chip(theme, r.status.label(), fg, bg));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
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
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(format!("{project} wants {}", r.item_name)),
                    )
                    .child(Self::chip(
                        theme,
                        use_label,
                        theme.text_muted,
                        theme.surface_2,
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(11.0))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "{} · {}",
                                if r.purpose.is_empty() {
                                    "no purpose given".to_owned()
                                } else {
                                    r.purpose.clone()
                                },
                                Self::ago(r.decided_at.unwrap_or(r.created_at))
                            )),
                    )
                    .child(actions),
            )
    }

    fn render_item(&self, it: &VaultItem, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let id = it.id.clone();
        let hosts = if it.allowed_hosts.is_empty() {
            "no allowed hosts (unusable until set)".to_owned()
        } else {
            it.allowed_hosts.join(", ")
        };
        let used = it
            .last_used_at
            .map(|t| format!(" · used {}", Self::ago(t)))
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
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
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(it.name.clone()),
                    )
                    .child(Self::chip(
                        theme,
                        it.kind_label(),
                        theme.text_muted,
                        theme.surface_2,
                    ))
                    .child(
                        Self::button(format!("vault-delete-{}", it.id), "Delete", false, theme)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let id = id.clone();
                                this.state.update(cx, |s, cx| s.vault_delete_item(id, cx));
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_muted)
                    .child(format!("{hosts}{used}")),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(format!("${} · {}", it.env, it.fields.join(", "))),
            )
    }

    fn field(label: &'static str, input: Entity<ComposerInput>, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(label),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .min_h(px(30.0))
                    .px(px(10.0))
                    .py(px(3.0))
                    .rounded(px(7.0))
                    .bg(theme.surface_2)
                    .border_1()
                    .border_color(theme.border_strong)
                    .child(input),
            )
    }

    fn render_add(&self, kind: AddKind, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .child(Self::field("Name", self.name_input.clone(), theme));
        match kind {
            AddKind::Login => {
                form = form
                    .child(Self::field("Sign-in URL", self.url_input.clone(), theme))
                    .child(Self::field("Username", self.user_input.clone(), theme))
                    .child(Self::field("Password", self.secret_input.clone(), theme));
            }
            AddKind::Secret | AddKind::ApiKey => {
                form = form.child(Self::field("Value", self.secret_input.clone(), theme));
            }
        }
        form = form.child(Self::field(
            "Allowed hosts",
            self.hosts_input.clone(),
            theme,
        ));
        if let Some(err) = &self.error {
            form = form.child(
                div()
                    .text_size(px(10.5))
                    .text_color(theme.danger)
                    .child(err.clone()),
            );
        }
        form.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child("Encrypted on the relay. The agent only ever gets a placeholder."),
                )
                .child(
                    Self::button("vault-add-cancel", "Cancel", false, theme)
                        .flex_shrink_0()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reset_form(cx);
                            cx.notify();
                        })),
                )
                .child(
                    Self::button("vault-add-save", "Save", true, theme)
                        .flex_shrink_0()
                        .on_click(cx.listener(|this, _, _, cx| this.submit_add(cx))),
                ),
        )
    }

    fn render_body(&self, view: &VaultView, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col();
        let pending: Vec<&VaultRequest> = view.pending().collect();
        if !pending.is_empty() {
            body = body.child(Self::section_label(theme, "WAITING FOR YOU"));
            for r in pending {
                body = body.child(self.render_request(r, theme, cx));
            }
        }

        let mut items_header = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(div().flex_1().child(Self::section_label(theme, "ITEMS")));
        if self.adding.is_none() {
            items_header = items_header
                .child(
                    Self::button("vault-add-login", "Add login", false, theme).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.start_add(AddKind::Login, window, cx)
                        }),
                    ),
                )
                .child(
                    Self::button("vault-add-key", "Add API key", false, theme).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.start_add(AddKind::ApiKey, window, cx)
                        }),
                    ),
                )
                .child(
                    Self::button("vault-add-secret", "Add secret", true, theme).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.start_add(AddKind::Secret, window, cx)
                        }),
                    ),
                );
        }
        body = body.child(items_header);
        if let Some(kind) = self.adding {
            body = body.child(self.render_add(kind, theme, cx));
        }
        if view.items.is_empty() {
            body = body.child(
                div()
                    .py(px(10.0))
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .child(if view.loaded {
                        "No items yet. Add a login or a key the agent may use without seeing it."
                    } else {
                        "Loading from the relay…"
                    }),
            );
        }
        for it in &view.items {
            body = body.child(self.render_item(it, theme, cx));
        }

        let decided: Vec<&VaultRequest> = view
            .requests
            .iter()
            .filter(|r| r.status != VaultRequestStatus::Pending)
            .take(3)
            .collect();
        if !decided.is_empty() {
            body = body.child(Self::section_label(theme, "RECENT DECISIONS"));
            for r in decided {
                body = body.child(self.render_request(r, theme, cx));
            }
        }
        body
    }
}

impl Focusable for VaultPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for VaultPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let view = self.state.read(cx).vault.clone();

        let mut header = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .id("vault-back")
                    .text_size(px(11.5))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .child("‹ Settings")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hide(cx);
                        cx.emit(VaultEvent::Back);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("VAULT"),
            );
        if view.mock {
            header = header.child(Self::chip(&theme, "Mock", theme.warn, theme.surface_2));
        }
        header = header.child(
            div()
                .id("vault-refresh")
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .hover(|s| s.text_color(theme.text))
                .child("Refresh")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.vault_refresh(cx));
                })),
        );

        let card = div()
            .id("vault-popover")
            .track_focus(&self.focus_handle)
            .w(px(420.0))
            .max_h(px(620.0))
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
            .child(
                div()
                    .id("vault-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(self.render_body(&view, &theme, cx)),
            )
            .child(
                div()
                    .pt(px(4.0))
                    .text_size(px(10.5))
                    .text_color(theme.text_faint)
                    .child(if view.mock {
                        "Mock mode: demo items point at the relay's own echo target. Values never leave the relay; the agent sends placeholders through /egress."
                    } else {
                        "Values stay on the relay. The agent asks by name, you approve here, and the relay fills the real value in only for the item's allowed hosts."
                    }),
            );
        div()
            .absolute()
            .left(px(RAIL_WIDTH + 8.0))
            .bottom(px(14.0))
            .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
    }
}
