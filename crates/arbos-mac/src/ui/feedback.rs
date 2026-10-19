//! "Send feedback" (⌘⇧F, the ✎ rail button, Help → Send Feedback…): the Mac twin of
//! TestFlight's screenshot feedback. Opening the sheet grabs the Arbos window as a PNG;
//! the user can grab it again, clear it, drag a region, or pick an image file, add a
//! note, and send (Enter / ⌘⏎). A note alone is enough when they skip the picture.
//! `POST /feedback` on the relay stores it; the feedback loop on the server
//! (`scripts/feedback`) turns it into a GitHub issue and an automatic fix.
//!
//! Self-contained: the shell only constructs it, calls `open` on the action, and renders it
//! while `is_open()`.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use base64::Engine as _;
use gpui::{
    actions, div, img, prelude::*, px, App, Context, Div, Entity, FocusHandle, Focusable,
    FontWeight, ObjectFit, RenderImage, Window,
};
use gpui_tokio::Tokio;

use crate::app::{decode_image, AppState};
use crate::sync::credentials;
use crate::ui::composer::ComposerInput;
use crate::ui::frost::{frosted, PANEL_BLUR};
use crate::ui::theme::{Theme, PANEL_RADIUS};

actions!(feedback, [SendFeedback, SubmitFeedback]);

/// A captured or chosen image, ready to send.
struct Shot {
    bytes: Arc<Vec<u8>>,
    mime: &'static str,
    width: u32,
    height: u32,
    preview: Option<Arc<RenderImage>>,
    /// "Arbos window", "Selected region", or the file name.
    label: String,
}

impl Shot {
    fn from_bytes(bytes: Vec<u8>, label: String) -> Result<Self> {
        let mime = match bytes.get(..4) {
            Some([0x89, b'P', b'N', b'G']) => "image/png",
            Some([0xFF, 0xD8, ..]) => "image/jpeg",
            _ => return Err(anyhow!("{label}: not a PNG or JPEG")),
        };
        let (width, height) = image::load_from_memory(&bytes)
            .map(|i| (i.width(), i.height()))
            .context("decode image")?;
        let preview = decode_image(&bytes).map(Arc::new);
        Ok(Self {
            bytes: Arc::new(bytes),
            mime,
            width,
            height,
            preview,
            label,
        })
    }
}

pub struct FeedbackSheet {
    state: Entity<AppState>,
    pub note: Entity<ComposerInput>,
    open: bool,
    shot: Option<Shot>,
    busy: bool,
    error: Option<String>,
    /// Relay id of the item just sent; the sheet shows "Sent" until closed.
    sent: Option<String>,
    focus_handle: FocusHandle,
}

impl FeedbackSheet {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let note = cx.new(ComposerInput::new);
        note.update(cx, |input, _| {
            input.set_placeholder("What is wrong, or what should change?");
        });
        // Enter in the note field submits (same as ⌘⏎ / Send).
        {
            let weak = cx.weak_entity();
            note.update(cx, |input, _| {
                input.on_submit = Some(Rc::new(move |_window, cx| {
                    let _ = weak.update(cx, |this, cx| this.send(cx));
                }));
            });
        }
        cx.observe(&note, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            note,
            open: false,
            shot: None,
            busy: false,
            error: None,
            sent: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// ⌘⇧F / rail / menu: grab the window first (the sheet must not be in the picture), then show.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
            return;
        }
        self.error = None;
        self.sent = None;
        self.busy = false;
        // Debug builds: ARBOS_FEEDBACK_IMAGE=<file> stands in for the capture (demos, Linux).
        let preset = if cfg!(debug_assertions) {
            std::env::var("ARBOS_FEEDBACK_IMAGE")
                .ok()
                .map(PathBuf::from)
                .and_then(|p| load_file(&p).ok())
        } else {
            None
        };
        match preset.map(Ok).unwrap_or_else(|| capture_window(window)) {
            Ok(shot) => self.shot = Some(shot),
            Err(e) => {
                self.shot = None;
                self.error = Some(format!(
                    "Could not capture the window ({e:#}). Choose an image or select a region."
                ));
            }
        }
        self.open = true;
        let note = self.note.read(cx).focus_handle(cx);
        window.focus(&note, cx);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.shot = None;
        self.busy = false;
        self.note.update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    fn recapture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Hide the sheet for the grab, then show it again with the new picture.
        self.open = false;
        cx.notify();
        window.refresh();
        let shot = capture_window(window);
        self.open = true;
        match shot {
            Ok(s) => {
                self.shot = Some(s);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Could not capture the window ({e:#})")),
        }
        cx.notify();
    }

    /// `screencapture -i -s`: the user drags a rectangle anywhere on screen.
    fn select_region(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            let path =
                std::env::temp_dir().join(format!("arbos-feedback-{}.png", std::process::id()));
            let status = std::process::Command::new("screencapture")
                .args(["-i", "-s", "-x"])
                .arg(&path)
                .status()
                .context("run screencapture")?;
            if !status.success() {
                return Err(anyhow!("screencapture exited with {status}"));
            }
            let bytes = std::fs::read(&path).context("read the selection")?;
            let _ = std::fs::remove_file(&path);
            Shot::from_bytes(bytes, "Selected region".into())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |sheet, cx| {
                sheet.busy = false;
                match result {
                    Ok(shot) => sheet.shot = Some(shot),
                    // Esc in the selection just keeps the previous picture.
                    Err(e) if e.to_string().contains("exited with") => {}
                    Err(e) => sheet.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    let loaded = load_file(&path);
                    let _ = this.update(cx, |sheet, cx| {
                        match loaded {
                            Ok(shot) => {
                                sheet.shot = Some(shot);
                                sheet.error = None;
                            }
                            Err(e) => sheet.error = Some(format!("{e:#}")),
                        }
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn clear_shot(&mut self, cx: &mut Context<Self>) {
        self.shot = None;
        self.error = None;
        cx.notify();
    }

    fn submit_action(&mut self, _: &SubmitFeedback, _: &mut Window, cx: &mut Context<Self>) {
        self.send(cx);
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.sent.is_some() {
            return;
        }
        let note = self.note.read(cx).text().trim().to_string();
        let shot = self.shot.as_ref();
        if shot.is_none() && note.is_empty() {
            self.error = Some("Add a note, or keep / attach a screenshot.".into());
            cx.notify();
            return;
        }
        let relay_url = crate::sync::configured_relay_url();
        let Some(creds) = credentials::load(&relay_url) else {
            self.error = Some("Sign in first: feedback goes to your relay.".into());
            cx.notify();
            return;
        };
        let project_id = self.state.read(cx).selected().map(|p| p.id.clone());
        let mut body = serde_json::json!({
            "note": note,
            "app_version": crate::update::current_version(),
            "platform": "mac",
            "os": os_version(),
            "project_id": project_id,
        });
        if let Some(shot) = shot {
            body["image"] = serde_json::json!(
                base64::engine::general_purpose::STANDARD.encode(shot.bytes.as_slice())
            );
            body["mime"] = serde_json::json!(shot.mime);
            body["width"] = serde_json::json!(shot.width);
            body["height"] = serde_json::json!(shot.height);
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let url = format!("{}/feedback", relay_url.trim_end_matches('/'));
        let token = creds.access_token.clone();
        let task = Tokio::spawn_result(cx, async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()?;
            let resp = client
                .post(url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .context("send feedback")?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                return Err(anyhow!(
                    "relay answered {status}: {}",
                    text.chars().take(200).collect::<String>()
                ));
            }
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            Ok(v["id"].as_str().unwrap_or("sent").to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |sheet, cx| {
                sheet.busy = false;
                match result {
                    Ok(id) => {
                        sheet.sent = Some(id);
                        sheet.note.update(cx, |input, cx| input.clear(cx));
                    }
                    Err(e) => sheet.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn button(
        id: &'static str,
        label: &'static str,
        primary: bool,
        enabled: bool,
        theme: &Theme,
    ) -> gpui::Stateful<Div> {
        let base = div()
            .id(id)
            .px(px(12.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .text_size(px(12.5))
            .child(label);
        if !enabled {
            return base.text_color(theme.text_faint).bg(theme.surface_1);
        }
        if primary {
            base.bg(theme.accent)
                .text_color(theme.bg)
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .hover(|s| s.opacity(0.9))
        } else {
            base.bg(theme.surface_1)
                .text_color(theme.text)
                .border_1()
                .border_color(theme.border_strong)
                .cursor_pointer()
                .hover(|s| s.bg(theme.surface_2))
        }
    }
}

impl Focusable for FeedbackSheet {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FeedbackSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let has_shot = self.shot.is_some();
        let has_note = !self.note.read(cx).text().trim().is_empty();
        let can_send = (has_shot || has_note) && !self.busy;
        let busy = self.busy;
        let sent = self.sent.clone();
        let error = self.error.clone();
        let is_mac = cfg!(target_os = "macos");

        let preview = div()
            .w_full()
            .h(px(240.0))
            .rounded(px(10.0))
            .bg(theme.surface_1)
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden();
        let preview = match self.shot.as_ref() {
            Some(shot) => {
                let caption = format!("{} · {}×{}", shot.label, shot.width, shot.height);
                let inner = match shot.preview.clone() {
                    Some(image) => {
                        preview.child(img(image).w_full().h_full().object_fit(ObjectFit::Contain))
                    }
                    None => preview.child(
                        div()
                            .text_color(theme.text_muted)
                            .child("(preview unavailable)"),
                    ),
                };
                div().flex().flex_col().gap(px(4.0)).child(inner).child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_faint)
                        .child(caption),
                )
            }
            None => div().child(
                preview.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.text_muted)
                        .child("No screenshot yet"),
                ),
            ),
        };

        let note_row = div()
            .flex()
            .items_center()
            .min_h(px(40.0))
            .px(px(14.0))
            .py(px(6.0))
            .rounded(px(12.0))
            .bg(theme.surface_1)
            .border_1()
            .border_color(theme.border_strong)
            .child(self.note.clone());

        let mut sources = div().flex().items_center().gap(px(8.0));
        if is_mac {
            sources = sources
                .child(
                    Self::button("feedback-recapture", "Capture window", false, !busy, &theme)
                        .on_click(cx.listener(|this, _, window, cx| this.recapture(window, cx))),
                )
                .child(
                    Self::button("feedback-region", "Select region", false, !busy, &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.select_region(cx))),
                );
        }
        sources = sources.child(
            Self::button("feedback-file", "Choose image…", false, !busy, &theme)
                .on_click(cx.listener(|this, _, _, cx| this.choose_file(cx))),
        );
        if has_shot {
            sources = sources.child(
                Self::button("feedback-clear", "No screenshot", false, !busy, &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.clear_shot(cx))),
            );
        }

        let status: Option<Div> = if let Some(id) = sent.as_ref() {
            Some(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "Sent. Thank you — reference {id}. The fix lands in a coming update."
                    )),
            )
        } else {
            error.map(|e| div().text_size(px(12.0)).text_color(theme.danger).child(e))
        };

        let actions = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(div().flex_1())
            .child(
                Self::button(
                    "feedback-cancel",
                    if sent.is_some() { "Done" } else { "Cancel" },
                    false,
                    true,
                    &theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .when(sent.is_none(), |el| {
                el.child(
                    Self::button(
                        "feedback-send",
                        if busy { "Sending…" } else { "Send ⏎" },
                        true,
                        can_send,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                )
            });

        let card = div()
            .w(px(520.0))
            .max_w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(18.0))
            .rounded(px(PANEL_RADIUS))
            .bg(theme.card())
            .border_1()
            .border_color(theme.border_strong)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(15.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Send feedback"),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_faint)
                            .child("⌘⇧F · ⏎ send"),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.text_muted)
                    .child("The window is captured when you open this. A note alone is enough — clear the picture if you do not want it. It goes to your relay and becomes a fix in a coming build."),
            )
            .child(preview)
            .child(sources)
            .child(note_row)
            .children(status)
            .child(actions);

        div()
            .id("feedback-sheet")
            .key_context("FeedbackSheet")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::submit_action))
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(Theme::with_alpha(theme.bg, 0.45))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(frosted(PANEL_RADIUS, PANEL_BLUR, card))
    }
}

fn load_file(path: &PathBuf) -> Result<Shot> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".into());
    Shot::from_bytes(bytes, name)
}

/// `sw_vers -productVersion` on macOS, `uname -sr` elsewhere; empty when unknown.
fn os_version() -> String {
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("sw_vers", &["-productVersion"])
    } else {
        ("uname", &["-sr"])
    };
    std::process::Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The Arbos window as a PNG: `screencapture -l <window number>` on macOS (exactly this
/// window, no shadow), by screen rectangle when the window number is unknown.
fn capture_window(window: &mut Window) -> Result<Shot> {
    if !cfg!(target_os = "macos") {
        return Err(anyhow!("window capture is macOS only"));
    }
    let path = std::env::temp_dir().join(format!("arbos-feedback-{}.png", std::process::id()));
    let mut cmd = std::process::Command::new("screencapture");
    cmd.args(["-x", "-o"]);
    match key_window_number() {
        Some(n) => {
            cmd.arg(format!("-l{n}"));
        }
        None => {
            let b = window.bounds();
            let (x, y): (f32, f32) = (b.origin.x.into(), b.origin.y.into());
            let (w, h): (f32, f32) = (b.size.width.into(), b.size.height.into());
            cmd.arg("-R").arg(format!(
                "{},{},{},{}",
                x.round(),
                y.round(),
                w.round(),
                h.round()
            ));
        }
    }
    let status = cmd.arg(&path).status().context("run screencapture")?;
    if !status.success() {
        return Err(anyhow!(
            "screencapture exited with {status} (Screen Recording permission?)"
        ));
    }
    let bytes = std::fs::read(&path).context("read the capture")?;
    let _ = std::fs::remove_file(&path);
    Shot::from_bytes(bytes, "Arbos window".into())
}

#[cfg(target_os = "macos")]
fn key_window_number() -> Option<i64> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    let window = app.keyWindow().or_else(|| app.mainWindow())?;
    let number = window.windowNumber();
    (number > 0).then_some(number as i64)
}

#[cfg(not(target_os = "macos"))]
fn key_window_number() -> Option<i64> {
    None
}
