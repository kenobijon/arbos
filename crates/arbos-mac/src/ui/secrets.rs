//! Settings → Secrets (D83, `docs/spec-secrets.md` §8.7): the kill switch, connected vaults,
//! the secrets list, a Secret page with per-agent switches, the New secret form with agent
//! chips, requests waiting for the four D76 answers, and the agent's Secrets page. D80 grouped
//! cards inside the Apps & tools panel. No value is ever displayed: the relay never sends one.

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Render, SharedString, Stateful, Window,
};

use crate::app::AppState;
use crate::model::{
    ago, secret_env_name, secret_usage, ConnectedVault, Id, NewSecret, ProjectSecrets, Secret,
    SecretActivity, SecretRequest, SecretsView,
};
use crate::ui::avatars;
use crate::ui::composer::ComposerInput;
use crate::ui::frost::{frosted, MENU_BLUR};
use crate::ui::glass;
use crate::ui::icons::icon;
use crate::ui::theme::{Theme, PANEL_RADIUS, RAIL_WIDTH, TEXT_BODY, TEXT_SMALL, TEXT_TINY};

pub enum SecretsEvent {
    /// "‹ Settings": return to the settings sheet.
    Back,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    List,
    Secret(Id),
    Add,
    Agent(Id),
    Connect,
    Vault(Id),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddKind {
    Key,
    Login,
    Text,
}

impl AddKind {
    fn as_str(self) -> &'static str {
        match self {
            AddKind::Key => "key",
            AddKind::Login => "login",
            AddKind::Text => "text",
        }
    }
}

pub struct SecretsPanel {
    state: Entity<AppState>,
    open: bool,
    page: Page,
    add_kind: AddKind,
    /// Agents picked in the Add / Connect / Give-access forms.
    picked: Vec<Id>,
    /// Inline pickers open on the Secret / Agent pages.
    giving: bool,
    replacing: bool,
    /// Two-click confirmations: the row id armed for deletion / pause.
    confirm: Option<String>,
    name_input: Entity<ComposerInput>,
    url_input: Entity<ComposerInput>,
    user_input: Entity<ComposerInput>,
    secret_input: Entity<ComposerInput>,
    note_input: Entity<ComposerInput>,
    hosts_input: Entity<ComposerInput>,
    token_input: Entity<ComposerInput>,
    error: Option<String>,
    focus_handle: FocusHandle,
}

impl EventEmitter<SecretsEvent> for SecretsPanel {}

impl SecretsPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let mk = |cx: &mut Context<Self>, placeholder: &str, masked: bool| {
            let input = cx.new(ComposerInput::new);
            input.update(cx, |i, _| {
                i.set_placeholder(placeholder);
                if masked {
                    i.set_masked(true);
                }
            });
            cx.observe(&input, |_, _, cx| cx.notify()).detach();
            input
        };
        let name_input = mk(cx, "STRIPE_KEY", false);
        let url_input = mk(cx, "https://github.com/login", false);
        let user_input = mk(cx, "name@example.com", false);
        let secret_input = mk(cx, "Sent once and encrypted. You won't see it again.", true);
        let note_input = mk(cx, "What is it for? Agents read this.", false);
        let hosts_input = mk(cx, "api.stripe.com", false);
        let token_input = mk(cx, "ops_••••••••••••••••••••", true);
        {
            let weak = cx.weak_entity();
            hosts_input.update(cx, |input, _| {
                input.on_submit = Some(Rc::new(move |_window, cx| {
                    let _ = weak.update(cx, |this, cx| this.submit_add(cx));
                }));
            });
        }
        Self {
            state,
            open: false,
            page: Page::List,
            add_kind: AddKind::Key,
            picked: Vec::new(),
            giving: false,
            replacing: false,
            confirm: None,
            name_input,
            url_input,
            user_input,
            secret_input,
            note_input,
            hosts_input,
            token_input,
            error: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn show(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        self.page = Page::List;
        self.state.update(cx, |s, cx| s.secrets_refresh(cx));
        cx.notify();
    }

    /// Agent profile → secrets: open straight on that agent's page.
    pub fn show_agent(&mut self, project_id: Id, cx: &mut Context<Self>) {
        self.open = true;
        self.page = Page::Agent(project_id.clone());
        self.state.update(cx, |s, cx| {
            s.secrets_refresh(cx);
            s.project_secrets_refresh(project_id, cx);
        });
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.reset_form(cx);
        self.page = Page::List;
        cx.notify();
    }

    /// True while one of the form fields has keyboard focus (the shell must not steal it).
    pub fn input_focused(&self, window: &Window, cx: &App) -> bool {
        self.inputs()
            .iter()
            .any(|i| i.read(cx).focus_handle(cx).is_focused(window))
    }

    fn inputs(&self) -> [&Entity<ComposerInput>; 7] {
        [
            &self.name_input,
            &self.url_input,
            &self.user_input,
            &self.secret_input,
            &self.note_input,
            &self.hosts_input,
            &self.token_input,
        ]
    }

    fn reset_form(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.picked.clear();
        self.giving = false;
        self.replacing = false;
        self.confirm = None;
        for input in self.inputs() {
            input.update(cx, |i, cx| i.clear(cx));
        }
    }

    fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        self.reset_form(cx);
        if let Page::Agent(pid) = &page {
            let pid = pid.clone();
            self.state
                .update(cx, |s, cx| s.project_secrets_refresh(pid, cx));
        }
        self.page = page;
        cx.notify();
    }

    fn hosts_from(&self, cx: &App) -> Vec<String> {
        self.hosts_input
            .read(cx)
            .text()
            .split([',', ' ', '\n'])
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn submit_add(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Add {
            return;
        }
        let kind = self.add_kind;
        let name = self.name_input.read(cx).text().trim().to_string();
        let secret = self.secret_input.read(cx).text().to_string();
        if name.is_empty()
            || name.len() > 64
            || secret_env_name(&name).is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        {
            self.error = Some("Letters, numbers and _ (1–64 characters).".into());
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
                fields.push((
                    "url".to_string(),
                    self.url_input.read(cx).text().trim().to_string(),
                ));
                fields.push((
                    "username".to_string(),
                    self.user_input.read(cx).text().trim().to_string(),
                ));
                fields.push(("password".to_string(), secret));
            }
            AddKind::Key | AddKind::Text => fields.push(("value".to_string(), secret)),
        }
        let item = NewSecret {
            name,
            kind: kind.as_str().into(),
            fields,
            note: self.note_input.read(cx).text().trim().to_string(),
            hosts: self.hosts_from(cx),
            agents: self.picked.clone(),
        };
        self.state.update(cx, |s, cx| s.secret_add(item, cx));
        self.go(Page::List, cx);
    }

    fn submit_replace(&mut self, id: Id, kind: &str, cx: &mut Context<Self>) {
        let value = self.secret_input.read(cx).text().to_string();
        if value.is_empty() {
            self.error = Some("Enter the new value.".into());
            cx.notify();
            return;
        }
        let body = if kind == "login" {
            serde_json::json!({ "fields": { "url": self.url_input.read(cx).text().trim(), "username": self.user_input.read(cx).text().trim(), "password": value } })
        } else {
            serde_json::json!({ "value": value })
        };
        self.state
            .update(cx, |s, cx| s.secret_replace_value(id, body, cx));
        self.replacing = false;
        self.error = None;
        self.secret_input.update(cx, |i, cx| i.clear(cx));
        cx.notify();
    }

    fn submit_connect(&mut self, cx: &mut Context<Self>) {
        let token = self.token_input.read(cx).text().trim().to_string();
        if token.len() < 20 {
            self.error = Some("Paste the whole service-account token.".into());
            cx.notify();
            return;
        }
        let note = self.note_input.read(cx).text().trim().to_string();
        let agents = self.picked.clone();
        self.state
            .update(cx, |s, cx| s.vault_connect(token, agents, note, cx));
        self.go(Page::List, cx);
    }

    // ---- building blocks -----------------------------------------------------------------------

    fn group(theme: &Theme) -> Div {
        glass::group(theme).bg(theme.bubble_assistant)
    }

    fn section(label: &str, group: Div, note: Option<String>, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .child(glass::section_label(label, theme))
            .child(group)
            .when_some(note, |el, n| el.child(Self::footnote(n, theme)))
    }

    fn footnote(text: impl Into<SharedString>, theme: &Theme) -> Div {
        div()
            .px(px(16.0))
            .pt(px(6.0))
            .text_size(px(TEXT_TINY))
            .text_color(theme.text_muted)
            .child(text.into())
    }

    /// Tile · title over subtitle · trailing.
    fn row(
        id: impl Into<gpui::ElementId>,
        glyph: &str,
        title: impl Into<SharedString>,
        subtitle: Option<String>,
        dim: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(10.0))
            .min_h(px(52.0))
            .px(px(16.0))
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .child(glass::icon_tile(glyph, theme).when(dim, |el| el.opacity(0.55)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(TEXT_BODY))
                            .text_color(if dim { theme.text_muted } else { theme.text })
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(title.into()),
                    )
                    .when_some(subtitle.filter(|s| !s.is_empty()), |el, s| {
                        el.child(
                            div()
                                .text_size(px(TEXT_TINY))
                                .text_color(theme.text_muted)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(s),
                        )
                    }),
            )
    }

    fn add_row(id: &'static str, label: &'static str, theme: &Theme) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(10.0))
            .min_h(px(44.0))
            .px(px(16.0))
            .text_size(px(TEXT_BODY))
            .text_color(theme.text)
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(
                div()
                    .size(px(30.0))
                    .flex_shrink_0()
                    .rounded(px(7.5))
                    .bg(theme.field)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("plus", 15.0, theme.text)),
            )
            .child(label)
    }

    fn value_row(label: &str, value: impl Into<SharedString>, theme: &Theme) -> Div {
        glass::row(label.to_string(), theme).child(
            div()
                .flex_shrink_0()
                .max_w(px(220.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(TEXT_BODY))
                .text_color(theme.text_muted)
                .child(value.into()),
        )
    }

    fn action_row(
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        danger: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .min_h(px(44.0))
            .px(px(16.0))
            .text_size(px(TEXT_BODY))
            .text_color(if danger { theme.danger } else { theme.text })
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(label.into())
    }

    fn field(label: &str, input: Entity<ComposerInput>, hint: Option<&str>, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(
                div()
                    .text_size(px(TEXT_TINY))
                    .text_color(theme.text_muted)
                    .child(label.to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .min_h(px(34.0))
                    .px(px(12.0))
                    .py(px(4.0))
                    .rounded(px(10.0))
                    .bg(theme.field)
                    .border_1()
                    .border_color(theme.field_border)
                    .child(input),
            )
            .when_some(hint, |el, h| {
                el.child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.text_muted)
                        .child(h.to_string()),
                )
            })
    }

    fn small_pill(
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        danger: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(26.0))
            .px(px(10.0))
            .rounded_full()
            .bg(theme.field)
            .flex()
            .items_center()
            .text_size(px(TEXT_TINY))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if danger { theme.danger } else { theme.text })
            .cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(label.into())
    }

    /// Agent chips: `( Research ✓ ) ( Comms )`, toggling `self.picked`.
    fn agent_chips(
        &self,
        prefix: &str,
        exclude: &[Id],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let projects: Vec<_> = self
            .state
            .read(cx)
            .projects
            .iter()
            .filter(|p| !p.is_guest() && !exclude.contains(&p.id))
            .cloned()
            .collect();
        let mut row = div().flex().flex_wrap().gap(px(6.0));
        if projects.is_empty() {
            return row.child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child("No agents yet"),
            );
        }
        for p in projects {
            let on = self.picked.contains(&p.id);
            let pid = p.id.clone();
            row = row.child(
                div()
                    .id(format!("{prefix}-chip-{}", p.id))
                    .h(px(30.0))
                    .pl(px(5.0))
                    .pr(px(11.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .bg(if on { theme.primary } else { theme.field })
                    .border_1()
                    .border_color(if on {
                        theme.primary
                    } else {
                        theme.field_border
                    })
                    .text_size(px(TEXT_SMALL))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if on { theme.on_primary } else { theme.text })
                    .cursor_pointer()
                    .child(avatars::project(&p, 20.0, theme))
                    .child(p.name.clone())
                    .when(on, |el| el.child(icon("check", 11.0, theme.on_primary)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(i) = this.picked.iter().position(|x| *x == pid) {
                            this.picked.remove(i);
                        } else {
                            this.picked.push(pid.clone());
                        }
                        cx.notify();
                    })),
            );
        }
        row
    }

    fn switch_row(
        id: impl Into<gpui::ElementId>,
        row: Stateful<Div>,
        on: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        let _ = id;
        row.cursor_pointer()
            .hover(|s| s.bg(theme.surface_2))
            .child(glass::switch(on, theme))
    }

    fn activity_rows(items: &[SecretActivity], theme: &Theme) -> Vec<Div> {
        items
            .iter()
            .map(|a| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .px(px(16.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(if a.action == "refused" {
                                theme.danger
                            } else {
                                theme.text
                            })
                            .child(a.line()),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_TINY))
                            .text_color(theme.text_muted)
                            .child(ago(a.at)),
                    )
            })
            .collect()
    }

    fn request_row(&self, r: &SecretRequest, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let agent = r
            .project_name
            .clone()
            .unwrap_or_else(|| "this agent".to_string());
        let title = match &r.host {
            Some(h) => format!("{} at {h}", r.name),
            None => r.name.clone(),
        };
        let mut sub: Vec<String> = vec![agent.clone()];
        if !r.purpose.is_empty() {
            sub.push(r.purpose.clone());
        }
        sub.push(ago(r.created_at));
        let mut answers = div().flex().flex_wrap().gap(px(6.0)).pt(px(4.0));
        for (choice, label, danger) in [
            ("once", "Allow once".to_string(), false),
            ("chat", "Allow for this chat".to_string(), false),
            ("always", format!("Always allow {agent}"), false),
            ("deny", "Deny".to_string(), true),
        ] {
            let rid = r.id.clone();
            answers = answers.child(
                Self::small_pill(format!("req-{}-{choice}", r.id), label, danger, theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        let rid = rid.clone();
                        this.state
                            .update(cx, |s, cx| s.secret_decide(rid, choice.to_string(), cx));
                    }),
                ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_size(px(TEXT_BODY))
                    .text_color(theme.text)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(TEXT_TINY))
                    .text_color(theme.text_muted)
                    .child(sub.join(" · ")),
            )
            .child(answers)
    }

    // ---- pages -----------------------------------------------------------------------------------

    fn render_list(&self, view: &SecretsView, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap(px(18.0));

        // Kill switch.
        let paused = view.paused;
        let armed = self.confirm.as_deref() == Some("pause");
        let pause_row = Self::row(
            "secrets-pause",
            "pause",
            "Pause all secrets",
            armed.then(|| {
                "Pause all secrets? Every agent loses every secret within seconds. Click again."
                    .to_string()
            }),
            false,
            theme,
        )
        .border_b_0();
        let pause_row = Self::switch_row("secrets-pause-switch", pause_row, paused, theme)
            .on_click(cx.listener(move |this, _, _, cx| {
                if paused {
                    this.confirm = None;
                    this.state.update(cx, |s, cx| s.secrets_pause(false, cx));
                } else if this.confirm.as_deref() == Some("pause") {
                    this.confirm = None;
                    this.state.update(cx, |s, cx| s.secrets_pause(true, cx));
                } else {
                    this.confirm = Some("pause".into());
                }
                cx.notify();
            }));
        body = body.child(
            div()
                .flex()
                .flex_col()
                .child(Self::group(theme).child(pause_row))
                .child(Self::footnote(
                    if paused {
                        "Secrets are paused. Agents can't use any secret until you turn this off."
                    } else {
                        "Agents can't use any secret while paused."
                    },
                    theme,
                )),
        );

        let pending: Vec<&SecretRequest> = view.pending().collect();
        if !pending.is_empty() {
            let mut g = Self::group(theme);
            for r in pending {
                g = g.child(self.request_row(r, theme, cx));
            }
            body = body.child(Self::section("Waiting for you", g, None, theme));
        }

        if !view.vaults.is_empty() || !view.providers.is_empty() {
            let mut g = Self::group(theme);
            for v in &view.vaults {
                let vid = v.id.clone();
                let agents: Vec<&str> = v
                    .agents
                    .iter()
                    .filter(|a| a.enabled)
                    .map(|a| a.name.as_str())
                    .collect();
                let mut sub = vec![format!(
                    "{} item{}",
                    v.items_count,
                    if v.items_count == 1 { "" } else { "s" }
                )];
                if !agents.is_empty() {
                    sub.push(agents.join(", "));
                }
                if let Some(t) = v.token_expires_at {
                    sub.push(format!("token expires {}", crate::model::short_date(t)));
                }
                g = g.child(
                    Self::row(
                        format!("vault-{}", v.id),
                        "lock",
                        format!("{} · {}", v.provider_label(), v.name),
                        Some(sub.join(" · ")),
                        false,
                        theme,
                    )
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.surface_2))
                    .child(icon("chevron-right", 13.0, theme.text_faint))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.go(Page::Vault(vid.clone()), cx)),
                    ),
                );
            }
            if !view.providers.is_empty() {
                g = g.child(
                    Self::add_row("secrets-connect", "Connect a vault…", theme).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.go(Page::Connect, cx);
                            let h = this.token_input.read(cx).focus_handle(cx);
                            window.focus(&h, cx);
                        }),
                    ),
                );
            }
            body = body.child(Self::section("Vaults", g, None, theme));
        }

        let mut g = Self::group(theme);
        for s in &view.secrets {
            let sid = s.id.clone();
            let agents: Vec<&str> = s
                .agents
                .iter()
                .filter(|a| a.enabled)
                .map(|a| a.name.as_str())
                .collect();
            let who = if agents.is_empty() {
                "no agents".to_string()
            } else {
                agents.join(", ")
            };
            g = g.child(
                Self::row(
                    format!("secret-{}", s.id),
                    s.glyph(),
                    s.env.clone(),
                    Some(format!(
                        "{who} · {}",
                        secret_usage(s.last_used_at, None, true)
                    )),
                    paused,
                    theme,
                )
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
                .child(icon("chevron-right", 13.0, theme.text_faint))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.go(Page::Secret(sid.clone()), cx)),
                ),
            );
        }
        g = g.child(
            Self::add_row("secrets-add", "Add a secret…", theme).on_click(cx.listener(
                |this, _, window, cx| {
                    this.go(Page::Add, cx);
                    let h = this.name_input.read(cx).focus_handle(cx);
                    window.focus(&h, cx);
                },
            )),
        );
        let note = if view.secrets.is_empty() && view.vaults.is_empty() {
            if view.loaded {
                "Add an API key or password here, pick which agents may use it, and they'll use it without ever seeing it."
            } else {
                "Loading from the relay…"
            }
        } else {
            "Agents use secrets without seeing them. Values are never shown again after you save them."
        };
        body = body.child(Self::section("Secrets", g, Some(note.to_string()), theme));

        if !view.activity.is_empty() {
            let mut g = Self::group(theme);
            for row in Self::activity_rows(&view.activity[..view.activity.len().min(3)], theme) {
                g = g.child(row);
            }
            body = body.child(Self::section("Recent activity", g, None, theme));
        }
        body
    }

    fn render_secret(&self, s: &Secret, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap(px(18.0));
        let (paused, projects, acts) = {
            let st = self.state.read(cx);
            let acts: Vec<SecretActivity> = st
                .secrets
                .activity
                .iter()
                .filter(|a| a.name == s.env)
                .take(5)
                .cloned()
                .collect();
            (st.secrets.paused, st.projects.clone(), acts)
        };
        if paused {
            body = body.child(Self::footnote(
                "Secrets are paused. Agents can't use any secret until you turn this off.",
                theme,
            ));
        }
        if !s.note.is_empty() {
            body = body.child(
                Self::group(theme).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .px(px(16.0))
                        .py(px(10.0))
                        .child(
                            div()
                                .text_size(px(TEXT_TINY))
                                .text_color(theme.text_muted)
                                .child("Note"),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_BODY))
                                .text_color(theme.text)
                                .child(s.note.clone()),
                        ),
                ),
            );
        }

        // Facts.
        let mut g = Self::group(theme).child(Self::value_row("Kind", s.kind_label(), theme));
        if s.name != s.env {
            g = g.child(Self::value_row("Name", s.name.clone(), theme));
        }
        if let Some(u) = s.username.as_deref().filter(|u| !u.is_empty()) {
            g = g.child(Self::value_row("Username", u.to_string(), theme));
        }
        g = g.child(Self::value_row(
            "Used at",
            if s.hosts.is_empty() {
                "Decided at first use".to_string()
            } else {
                s.hosts.join(", ")
            },
            theme,
        ));
        let sid = s.id.clone();
        let kind = s.kind.clone();
        if self.replacing {
            let mut form = div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .px(px(16.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(theme.border);
            if s.kind == "login" {
                form = form
                    .child(Self::field(
                        "Sign-in URL",
                        self.url_input.clone(),
                        None,
                        theme,
                    ))
                    .child(Self::field(
                        "Username",
                        self.user_input.clone(),
                        None,
                        theme,
                    ))
                    .child(Self::field(
                        "Password",
                        self.secret_input.clone(),
                        Some("Sent once and encrypted. You won't see it again."),
                        theme,
                    ));
            } else {
                form = form.child(Self::field(
                    "Value",
                    self.secret_input.clone(),
                    Some("Sent once and encrypted. The old value keeps working for 24 hours."),
                    theme,
                ));
            }
            if let Some(e) = &self.error {
                form = form.child(
                    div()
                        .text_size(px(TEXT_TINY))
                        .text_color(theme.danger)
                        .child(e.clone()),
                );
            }
            let sid2 = sid.clone();
            form = form.child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        glass::primary_button("secret-replace-save", "Replace", theme)
                            .h(px(30.0))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.submit_replace(sid2.clone(), &kind, cx)
                            })),
                    )
                    .child(
                        glass::quiet_button("secret-replace-cancel", "Cancel", theme)
                            .h(px(30.0))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.replacing = false;
                                this.error = None;
                                cx.notify();
                            })),
                    ),
            );
            g = g.child(form);
        } else {
            g = g.child(
                glass::row("Value", theme)
                    .child(
                        div()
                            .text_size(px(TEXT_BODY))
                            .text_color(theme.text_muted)
                            .child("••••••"),
                    )
                    .child(
                        div()
                            .id("secret-replace")
                            .text_size(px(TEXT_BODY))
                            .text_color(theme.accent)
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.8))
                            .child("Replace…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.replacing = true;
                                this.error = None;
                                let h = this.secret_input.read(cx).focus_handle(cx);
                                window.focus(&h, cx);
                                cx.notify();
                            })),
                    ),
            );
        }
        g = g.child(Self::value_row(
            "Expires",
            s.expires_at
                .map(crate::model::short_date)
                .unwrap_or_else(|| "Never".to_string()),
            theme,
        ));
        let ask = s.ask_every_time;
        let sid_ask = sid.clone();
        g = g.child(
            glass::row("Ask every time", theme)
                .id("secret-ask")
                .border_b_0()
                .cursor_pointer()
                .hover(|st| st.bg(theme.surface_2))
                .child(glass::switch(ask, theme))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let id = sid_ask.clone();
                    this.state.update(cx, |st, cx| {
                        st.secret_patch(id, serde_json::json!({ "ask_every_time": !ask }), cx)
                    });
                })),
        );
        body = body.child(div().flex().flex_col().child(g).child(Self::footnote(
            "Ask every time shows you a card before each use, even for agents that have access.",
            theme,
        )));

        // Agents.
        let mut g = Self::group(theme);
        for a in &s.agents {
            let mut sub: Vec<String> = Vec::new();
            if !a.note.is_empty() {
                sub.push(a.note.clone());
            }
            if a.scope.kind == "chat" {
                sub.push("this chat only".into());
            }
            sub.push(secret_usage(a.last_used_at, a.disabled_at, a.enabled));
            let avatar = match projects.iter().find(|p| p.id == a.project_id) {
                Some(p) => avatars::project(p, 30.0, theme),
                None => avatars::element(&a.project_id, &a.name, None, 30.0, theme),
            };
            let (sid_a, pid_a, on) = (sid.clone(), a.project_id.clone(), a.enabled);
            g = g.child(
                div()
                    .id(format!("access-{}", a.access_id))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .min_h(px(52.0))
                    .px(px(16.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|st| st.bg(theme.surface_2))
                    .child(avatar)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(TEXT_BODY))
                                    .text_color(theme.text)
                                    .child(a.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(TEXT_TINY))
                                    .text_color(theme.text_muted)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(sub.join(" · ")),
                            ),
                    )
                    .child(glass::switch(on, theme))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (id, pid) = (sid_a.clone(), pid_a.clone());
                        this.state
                            .update(cx, |st, cx| st.secret_access(id, pid, !on, None, cx));
                    })),
            );
        }
        if s.agents.is_empty() && !self.giving {
            g = g.child(Self::footnote("No agent has access yet.", theme).pb(px(8.0)));
        }
        if self.giving {
            let have: Vec<Id> = s
                .agents
                .iter()
                .filter(|a| a.enabled)
                .map(|a| a.project_id.clone())
                .collect();
            let sid_g = sid.clone();
            g = g.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .child(self.agent_chips("give", &have, theme, cx))
                    .child(Self::field("Note", self.note_input.clone(), None, theme))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(
                                glass::primary_button("give-save", "Save", theme)
                                    .h(px(30.0))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let note =
                                            this.note_input.read(cx).text().trim().to_string();
                                        let picked = this.picked.clone();
                                        for pid in picked {
                                            let id = sid_g.clone();
                                            let n = (!note.is_empty()).then(|| note.clone());
                                            this.state.update(cx, |st, cx| {
                                                st.secret_access(id, pid, true, n, cx)
                                            });
                                        }
                                        this.giving = false;
                                        this.picked.clear();
                                        this.note_input.update(cx, |i, cx| i.clear(cx));
                                        cx.notify();
                                    })),
                            )
                            .child(
                                glass::quiet_button("give-cancel", "Cancel", theme)
                                    .h(px(30.0))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.giving = false;
                                        this.picked.clear();
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        } else {
            g = g.child(
                Self::add_row("secret-give", "Give another agent access…", theme).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.giving = true;
                        this.picked.clear();
                        cx.notify();
                    }),
                ),
            );
        }
        body = body.child(Self::section("Agents", g, None, theme));

        // Activity for this secret (from the overview; the relay keeps 30 days).
        let mut g = Self::group(theme);
        if acts.is_empty() {
            g = g.child(Self::footnote("No activity in the last 30 days.", theme).pb(px(8.0)));
        }
        for row in Self::activity_rows(&acts, theme) {
            g = g.child(row);
        }
        body = body.child(Self::section("Activity", g, None, theme));

        // Remove from all agents · Delete.
        let any_on = s.agents.iter().any(|a| a.enabled);
        let sid_r = sid.clone();
        let revoke_armed = self.confirm.as_deref() == Some("revoke");
        body = body.child(
            Self::group(theme).child(
                Self::action_row(
                    "secret-revoke",
                    if revoke_armed {
                        "Remove from all agents? Click again to confirm."
                    } else {
                        "Remove from all agents"
                    },
                    false,
                    theme,
                )
                .when(!any_on, |el| el.opacity(0.5))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !any_on {
                        return;
                    }
                    if this.confirm.as_deref() == Some("revoke") {
                        this.confirm = None;
                        let id = sid_r.clone();
                        this.state.update(cx, |st, cx| st.secret_revoke_all(id, cx));
                    } else {
                        this.confirm = Some("revoke".into());
                    }
                    cx.notify();
                })),
            ),
        );
        let delete_armed = self.confirm.as_deref() == Some("delete");
        let sid_d = sid.clone();
        let env = s.env.clone();
        body = body.child(
            Self::group(theme).child(
                Self::action_row(
                    "secret-delete",
                    if delete_armed {
                        format!("Delete {env}? Agents lose it immediately. Activity is kept. Click again.")
                    } else {
                        "Delete secret".to_string()
                    },
                    true,
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.confirm.as_deref() == Some("delete") {
                        this.confirm = None;
                        let id = sid_d.clone();
                        this.state.update(cx, |st, cx| st.secret_delete(id, cx));
                        this.go(Page::List, cx);
                    } else {
                        this.confirm = Some("delete".into());
                        cx.notify();
                    }
                })),
            ),
        );
        body
    }

    fn render_add(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let kind = self.add_kind;
        let segs = div()
            .flex()
            .gap(px(2.0))
            .p(px(3.0))
            .rounded_full()
            .bg(theme.field)
            .children(
                [AddKind::Key, AddKind::Login, AddKind::Text]
                    .into_iter()
                    .map(|k| {
                        let label = match k {
                            AddKind::Key => "Key",
                            AddKind::Login => "Login",
                            AddKind::Text => "Text",
                        };
                        glass::segment(format!("add-kind-{label}"), label, k == kind, theme)
                            .flex_1()
                            .justify_center()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_kind = k;
                                this.error = None;
                                cx.notify();
                            }))
                    }),
            );
        let env = secret_env_name(self.name_input.read(cx).text());
        let name_hint = format!(
            "Letters, numbers and _. Agents see it as ${}.",
            if env.is_empty() {
                "NAME".to_string()
            } else {
                env
            }
        );
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(segs)
            .child(Self::field(
                "Name",
                self.name_input.clone(),
                Some(&name_hint),
                theme,
            ));
        match kind {
            AddKind::Login => {
                form = form
                    .child(Self::field(
                        "Sign-in URL",
                        self.url_input.clone(),
                        None,
                        theme,
                    ))
                    .child(Self::field(
                        "Username",
                        self.user_input.clone(),
                        None,
                        theme,
                    ))
                    .child(Self::field(
                        "Password",
                        self.secret_input.clone(),
                        Some("Sent once and encrypted. You won't see it again."),
                        theme,
                    ));
            }
            AddKind::Key | AddKind::Text => {
                form = form.child(Self::field(
                    "Value",
                    self.secret_input.clone(),
                    Some(if kind == AddKind::Text {
                        "Paste a certificate, JSON key, or any text. Sent once and encrypted."
                    } else {
                        "Sent once and encrypted. You won't see it again."
                    }),
                    theme,
                ));
            }
        }
        form = form
            .child(Self::field("Note", self.note_input.clone(), None, theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(
                        div()
                            .text_size(px(TEXT_TINY))
                            .text_color(theme.text_muted)
                            .child("Agents"),
                    )
                    .child(self.agent_chips("add", &[], theme, cx)),
            )
            .child(Self::field(
                "Where it may be used (optional)",
                self.hosts_input.clone(),
                Some("Leave empty to decide the first time an agent uses it."),
                theme,
            ));
        if let Some(err) = &self.error {
            form = form.child(
                div()
                    .text_size(px(TEXT_TINY))
                    .text_color(theme.danger)
                    .child(err.clone()),
            );
        }
        form.child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.0))
                .pt(px(4.0))
                .child(
                    glass::quiet_button("add-cancel", "Cancel", theme)
                        .h(px(32.0))
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::List, cx))),
                )
                .child(
                    glass::primary_button("add-save", "Save", theme)
                        .h(px(32.0))
                        .on_click(cx.listener(|this, _, _, cx| this.submit_add(cx))),
                ),
        )
    }

    fn render_agent(&self, project_id: &str, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let name = state
            .project(project_id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Agent".into());
        let data: ProjectSecrets = state
            .project_secrets
            .get(project_id)
            .cloned()
            .unwrap_or_default();
        let mut body = div().flex().flex_col().gap(px(18.0));
        if data.paused {
            body = body.child(Self::footnote(
                "Secrets are paused. Agents can't use any secret until you turn this off.",
                theme,
            ));
        }
        let mut g = Self::group(theme);
        for item in &data.items {
            let (glyph, title, sub) = if let Some(v) = &item.vault {
                (
                    "lock",
                    format!("{} (vault)", v.name),
                    format!(
                        "everything in this vault · {} item{}",
                        v.items_count,
                        if v.items_count == 1 { "" } else { "s" }
                    ),
                )
            } else {
                let mut sub: Vec<String> = Vec::new();
                if !item.note.is_empty() {
                    sub.push(item.note.clone());
                }
                sub.push(secret_usage(
                    item.last_used_at,
                    item.disabled_at,
                    item.enabled,
                ));
                (
                    item.secret.as_ref().map(|s| s.glyph()).unwrap_or("key"),
                    item.title(),
                    sub.join(" · "),
                )
            };
            let on = item.enabled;
            let pid = project_id.to_string();
            let target = item
                .secret
                .as_ref()
                .map(|s| ("secret", s.id.clone()))
                .or_else(|| item.vault.as_ref().map(|v| ("vault", v.id.clone())));
            let row = Self::row(
                format!("agent-access-{}", item.access_id),
                glyph,
                title,
                Some(sub),
                data.paused,
                theme,
            );
            g = g.child(Self::switch_row("x", row, on, theme).on_click(cx.listener(
                move |this, _, _, cx| {
                    let pid = pid.clone();
                    match target.clone() {
                        Some(("secret", id)) => this
                            .state
                            .update(cx, |st, cx| st.secret_access(id, pid, !on, None, cx)),
                        Some((_, id)) => this
                            .state
                            .update(cx, |st, cx| st.vault_access(id, pid, !on, cx)),
                        None => {}
                    }
                },
            )));
        }
        if self.giving {
            let have: Vec<Id> = data
                .items
                .iter()
                .filter(|i| i.enabled)
                .filter_map(|i| i.secret.as_ref().map(|s| s.id.clone()))
                .collect();
            let candidates: Vec<Secret> = state
                .secrets
                .secrets
                .iter()
                .filter(|s| !have.contains(&s.id))
                .cloned()
                .collect();
            let mut chips = div().flex().flex_wrap().gap(px(6.0));
            if candidates.is_empty() {
                chips = chips.child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child("Every secret is already on. Add one in Settings → Secrets."),
                );
            }
            for s in candidates {
                let on = self.picked.contains(&s.id);
                let sid = s.id.clone();
                chips = chips.child(
                    div()
                        .id(format!("pick-secret-{}", s.id))
                        .h(px(30.0))
                        .px(px(11.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .bg(if on { theme.primary } else { theme.field })
                        .border_1()
                        .border_color(if on {
                            theme.primary
                        } else {
                            theme.field_border
                        })
                        .text_size(px(TEXT_SMALL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if on { theme.on_primary } else { theme.text })
                        .cursor_pointer()
                        .child(s.env.clone())
                        .when(on, |el| el.child(icon("check", 11.0, theme.on_primary)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(i) = this.picked.iter().position(|x| *x == sid) {
                                this.picked.remove(i);
                            } else {
                                this.picked.push(sid.clone());
                            }
                            cx.notify();
                        })),
                );
            }
            let pid = project_id.to_string();
            g = g.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .child(chips)
                    .child(Self::field("Note", self.note_input.clone(), None, theme))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(
                                glass::primary_button("agent-give-save", "Save", theme)
                                    .h(px(30.0))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let note =
                                            this.note_input.read(cx).text().trim().to_string();
                                        for sid in this.picked.clone() {
                                            let pid = pid.clone();
                                            let n = (!note.is_empty()).then(|| note.clone());
                                            this.state.update(cx, |st, cx| {
                                                st.secret_access(sid, pid, true, n, cx)
                                            });
                                        }
                                        this.giving = false;
                                        this.picked.clear();
                                        this.note_input.update(cx, |i, cx| i.clear(cx));
                                        cx.notify();
                                    })),
                            )
                            .child(
                                glass::quiet_button("agent-give-cancel", "Cancel", theme)
                                    .h(px(30.0))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.giving = false;
                                        this.picked.clear();
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        } else if data.owner {
            g = g.child(
                Self::add_row("agent-give", "Give access to a secret…", theme).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.giving = true;
                        this.picked.clear();
                        cx.notify();
                    }),
                ),
            );
        }
        let note = if data.items.is_empty() {
            format!("{name} has no secrets yet.")
        } else {
            format!(
                "{name} uses these without seeing them. Turn one off and it stops within seconds."
            )
        };
        body = body.child(Self::section("Secrets", g, Some(note), theme));

        let pending: Vec<&SecretRequest> =
            data.requests.iter().filter(|r| r.is_pending()).collect();
        if !pending.is_empty() {
            let mut g = Self::group(theme);
            for r in pending {
                g = g.child(self.request_row(r, theme, cx));
            }
            body = body.child(Self::section("Waiting for you", g, None, theme));
        }

        let mut g = Self::group(theme);
        if data.activity.is_empty() {
            g = g.child(Self::footnote("No activity in the last 30 days.", theme).pb(px(8.0)));
        }
        for row in Self::activity_rows(&data.activity[..data.activity.len().min(6)], theme) {
            g = g.child(row);
        }
        body.child(Self::section("Activity", g, None, theme))
    }

    fn render_connect(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let step = |n: &str, text: &str, theme: &Theme| {
            div()
                .flex()
                .gap(px(10.0))
                .child(
                    div()
                        .size(px(22.0))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(theme.primary)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(TEXT_TINY))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.on_primary)
                        .child(n.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(TEXT_BODY))
                        .text_color(theme.text)
                        .child(text.to_string()),
                )
        };
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(step(
                "1",
                "In 1Password, create a service account with read access to one vault, e.g. \"Arbos — Research\".",
                theme,
            ))
            .child(
                div()
                    .id("connect-open-1p")
                    .pl(px(32.0))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.accent)
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.8))
                    .child("Open 1password.com ›")
                    .on_click(|_, _, cx| {
                        cx.open_url("https://my.1password.com/developer-tools/infrastructure-secrets/serviceaccount/");
                    }),
            )
            .child(step("2", "Paste its token", theme))
            .child(
                div().pl(px(32.0)).child(Self::field(
                    "Token",
                    self.token_input.clone(),
                    Some("Stored encrypted. Arbos never shows it again."),
                    theme,
                )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(div().text_size(px(TEXT_TINY)).text_color(theme.text_muted).child("Agents that may use this vault"))
                    .child(self.agent_chips("connect", &[], theme, cx)),
            )
            .child(Self::field("Note", self.note_input.clone(), None, theme));
        if let Some(err) = &self.error {
            form = form.child(
                div()
                    .text_size(px(TEXT_TINY))
                    .text_color(theme.danger)
                    .child(err.clone()),
            );
        }
        form.child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.0))
                .child(
                    glass::quiet_button("connect-cancel", "Cancel", theme)
                        .h(px(32.0))
                        .on_click(cx.listener(|this, _, _, cx| this.go(Page::List, cx))),
                )
                .child(
                    glass::primary_button("connect-save", "Connect", theme)
                        .h(px(32.0))
                        .on_click(cx.listener(|this, _, _, cx| this.submit_connect(cx))),
                ),
        )
    }

    fn render_vault(&self, v: &ConnectedVault, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap(px(18.0));
        let mut g = Self::group(theme)
            .child(Self::value_row("Items", v.items_count.to_string(), theme))
            .child(Self::value_row(
                "Token expires",
                v.token_expires_at
                    .map(crate::model::short_date)
                    .unwrap_or_else(|| "Never".to_string()),
                theme,
            ));
        if v.status != "ok" && !v.status.is_empty() {
            g = g.child(Self::value_row("Status", v.status.replace('_', " "), theme));
        }
        body = body.child(g);
        let mut g = Self::group(theme);
        let projects = self.state.read(cx).projects.clone();
        for a in &v.agents {
            let avatar = match projects.iter().find(|p| p.id == a.project_id) {
                Some(p) => avatars::project(p, 30.0, theme),
                None => avatars::element(&a.project_id, &a.name, None, 30.0, theme),
            };
            let (vid, pid, on) = (v.id.clone(), a.project_id.clone(), a.enabled);
            g = g.child(
                div()
                    .id(format!("vault-access-{}", a.access_id))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .min_h(px(52.0))
                    .px(px(16.0))
                    .py(px(8.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|st| st.bg(theme.surface_2))
                    .child(avatar)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(TEXT_BODY))
                                    .text_color(theme.text)
                                    .child(a.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(TEXT_TINY))
                                    .text_color(theme.text_muted)
                                    .child(if a.note.is_empty() {
                                        secret_usage(a.last_used_at, a.disabled_at, a.enabled)
                                    } else {
                                        a.note.clone()
                                    }),
                            ),
                    )
                    .child(glass::switch(on, theme))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (id, pid) = (vid.clone(), pid.clone());
                        this.state
                            .update(cx, |st, cx| st.vault_access(id, pid, !on, cx));
                    })),
            );
        }
        if self.giving {
            let have: Vec<Id> = v
                .agents
                .iter()
                .filter(|a| a.enabled)
                .map(|a| a.project_id.clone())
                .collect();
            let vid = v.id.clone();
            g = g.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .child(self.agent_chips("vault-give", &have, theme, cx))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(
                                glass::primary_button("vault-give-save", "Save", theme)
                                    .h(px(30.0))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        for pid in this.picked.clone() {
                                            let id = vid.clone();
                                            this.state.update(cx, |st, cx| {
                                                st.vault_access(id, pid, true, cx)
                                            });
                                        }
                                        this.giving = false;
                                        this.picked.clear();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                glass::quiet_button("vault-give-cancel", "Cancel", theme)
                                    .h(px(30.0))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.giving = false;
                                        this.picked.clear();
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        } else {
            g = g.child(
                Self::add_row("vault-give", "Give another agent access…", theme).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.giving = true;
                        this.picked.clear();
                        cx.notify();
                    }),
                ),
            );
        }
        body = body.child(Self::section(
            "Agents",
            g,
            Some("An agent with access may use any item in this vault by reference; the first use at a new host asks you.".into()),
            theme,
        ));
        let armed = self.confirm.as_deref() == Some("disconnect");
        let agents: Vec<&str> = v
            .agents
            .iter()
            .filter(|a| a.enabled)
            .map(|a| a.name.as_str())
            .collect();
        let who = if agents.is_empty() {
            "Agents".to_string()
        } else {
            agents.join(" and ")
        };
        let vid = v.id.clone();
        body.child(
            div()
                .flex()
                .flex_col()
                .child(Self::group(theme).child(
                    Self::action_row(
                        "vault-disconnect",
                        if armed {
                            format!("Disconnect this vault? {who} lose every item in it. Click again.")
                        } else {
                            "Disconnect".to_string()
                        },
                        true,
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.confirm.as_deref() == Some("disconnect") {
                            this.confirm = None;
                            let id = vid.clone();
                            this.state.update(cx, |st, cx| st.vault_disconnect(id, cx));
                            this.go(Page::List, cx);
                        } else {
                            this.confirm = Some("disconnect".into());
                            cx.notify();
                        }
                    })),
                ))
                .child(Self::footnote("Also revoke the service account in 1Password.", theme)),
        )
    }

    fn title(&self, view: &SecretsView, cx: &App) -> String {
        match &self.page {
            Page::List => "Secrets".into(),
            Page::Secret(id) => view
                .secret(id)
                .map(|s| s.env.clone())
                .unwrap_or_else(|| "Secret".into()),
            Page::Add => "New secret".into(),
            Page::Agent(pid) => format!(
                "{} · Secrets",
                self.state
                    .read(cx)
                    .project(pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "Agent".into())
            ),
            Page::Connect => "Connect 1Password".into(),
            Page::Vault(id) => view
                .vaults
                .iter()
                .find(|v| v.id == *id)
                .map(|v| format!("{} · {}", v.provider_label(), v.name))
                .unwrap_or_else(|| "Vault".into()),
        }
    }
}

impl Focusable for SecretsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SecretsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let view = self.state.read(cx).secrets.clone();
        let title = self.title(&view, cx);
        let back_label = if self.page == Page::List {
            "‹ Settings"
        } else {
            "‹ Secrets"
        };

        let header = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(4.0))
            .child(
                div()
                    .id("secrets-back")
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .child(back_label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.page == Page::List {
                            this.hide(cx);
                            cx.emit(SecretsEvent::Back);
                        } else {
                            this.go(Page::List, cx);
                        }
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .text_center()
                    .text_size(px(TEXT_BODY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title),
            )
            .child(
                div()
                    .id("secrets-refresh")
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
                    .child("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let page = this.page.clone();
                        this.state.update(cx, |s, cx| {
                            s.secrets_refresh(cx);
                            if let Page::Agent(pid) = page {
                                s.project_secrets_refresh(pid, cx);
                            }
                        });
                    })),
            );

        let body = match self.page.clone() {
            Page::List => self.render_list(&view, &theme, cx),
            Page::Secret(id) => match view.secret(&id) {
                Some(s) => self.render_secret(s, &theme, cx),
                None => div().child(Self::footnote("This secret is gone.", &theme)),
            },
            Page::Add => self.render_add(&theme, cx),
            Page::Agent(pid) => self.render_agent(&pid, &theme, cx),
            Page::Connect => self.render_connect(&theme, cx),
            Page::Vault(id) => match view.vaults.iter().find(|v| v.id == id) {
                Some(v) => self.render_vault(v, &theme, cx),
                None => div().child(Self::footnote("This vault is gone.", &theme)),
            },
        };

        let card = div()
            .id("secrets-popover")
            .track_focus(&self.focus_handle)
            .w(px(460.0))
            .max_h(px(680.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(12.0))
            .rounded(px(PANEL_RADIUS))
            .bg(theme.card())
            .border_1()
            .border_color(theme.border_strong)
            .shadow_lg()
            .child(header)
            .child(
                div()
                    .id("secrets-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pr(px(2.0))
                    .child(body),
            );
        div()
            .absolute()
            .left(px(RAIL_WIDTH + 8.0))
            .bottom(px(14.0))
            .child(frosted(PANEL_RADIUS, MENU_BLUR, card))
    }
}
