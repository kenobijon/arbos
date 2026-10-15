//! Question cards (design §10 "question widgets"): one [`QuestionCard`] view
//! per `question` part. Five widget types — choice, multi-select, text, date,
//! form — each answered from the card with the mouse or the keyboard (Enter
//! submits, arrows move, Space toggles). An answered card stays in the
//! transcript with its value, in the same frame as the approval card (F2).

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gpui::{
    actions, div, prelude::*, px, App, Context, Div, Entity, FocusHandle, Focusable, FontWeight,
    Render, SharedString, Window,
};
use serde_json::{json, Value};

use arbos_core::widget::{add_days, check_date, today_iso};

use crate::app::AppState;
use crate::model::{ChoiceOption, FieldKind, FormField, Id, Part, QuestionState, Widget};
use crate::ui::composer::ComposerInput;
use crate::ui::theme::{Theme, PANEL_RADIUS};
use crate::ui::transcript::ASSISTANT_MAX_WIDTH;

actions!(
    widgets,
    [SubmitAnswer, NextOption, PrevOption, ToggleOption]
);

/// Live snapshot of the part this card renders.
#[derive(Clone, Debug, PartialEq)]
struct PartView {
    question: String,
    widget: Widget,
    answer: Option<Value>,
    state: QuestionState,
}

pub struct QuestionCard {
    state: Entity<AppState>,
    chat_id: Id,
    question_id: String,
    focus_handle: FocusHandle,
    /// Keyboard cursor over the options (choice / multi-select).
    highlighted: usize,
    selected: BTreeSet<String>,
    text_input: Option<Entity<ComposerInput>>,
    form_inputs: BTreeMap<String, Entity<ComposerInput>>,
    form_choice: BTreeMap<String, String>,
    form_bool: BTreeMap<String, bool>,
    error: Option<String>,
    focused_once: bool,
}

impl QuestionCard {
    pub fn new(
        state: Entity<AppState>,
        chat_id: Id,
        question_id: String,
        widget: &Widget,
        cx: &mut Context<Self>,
    ) -> Self {
        let weak = cx.weak_entity();
        let submit_from_input = move |_: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |card, cx| card.submit(cx));
        };
        let make_input = |placeholder: &str, cx: &mut Context<Self>| {
            let input = cx.new(ComposerInput::new);
            let placeholder = placeholder.to_string();
            let on_submit = submit_from_input.clone();
            input.update(cx, |input, _| {
                input.set_placeholder(placeholder);
                input.on_submit = Some(Rc::new(on_submit));
            });
            cx.observe(&input, |_, _, cx| cx.notify()).detach();
            input
        };
        let mut text_input = None;
        let mut form_inputs = BTreeMap::new();
        let mut form_bool = BTreeMap::new();
        match widget {
            Widget::Choice { .. } | Widget::MultiSelect { .. } => {}
            Widget::Text { placeholder, .. } => {
                text_input = Some(make_input(
                    placeholder.as_deref().unwrap_or("Type your answer…"),
                    cx,
                ));
            }
            Widget::Date { .. } => {
                text_input = Some(make_input("yyyy-mm-dd", cx));
            }
            Widget::Form { fields } => {
                for f in fields {
                    match f.kind {
                        FieldKind::Text => {
                            form_inputs.insert(f.id.clone(), make_input(&f.label, cx));
                        }
                        FieldKind::Number => {
                            form_inputs.insert(f.id.clone(), make_input("0", cx));
                        }
                        FieldKind::Date => {
                            form_inputs.insert(f.id.clone(), make_input("yyyy-mm-dd", cx));
                        }
                        FieldKind::Bool => {
                            form_bool.insert(f.id.clone(), false);
                        }
                        FieldKind::Choice => {}
                    }
                }
            }
        }
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            chat_id,
            question_id,
            focus_handle: cx.focus_handle(),
            highlighted: 0,
            selected: BTreeSet::new(),
            text_input,
            form_inputs,
            form_choice: BTreeMap::new(),
            form_bool,
            error: None,
            focused_once: false,
        }
    }

    fn part(&self, cx: &App) -> Option<PartView> {
        let state = self.state.read(cx);
        state.messages_of(&self.chat_id).iter().find_map(|m| {
            m.parts.iter().find_map(|p| match p {
                Part::Question {
                    question_id,
                    question,
                    widget: Some(widget),
                    answer,
                    state,
                    ..
                } if *question_id == self.question_id => Some(PartView {
                    question: question.clone(),
                    widget: widget.clone(),
                    answer: answer.clone(),
                    state: *state,
                }),
                _ => None,
            })
        })
    }

    fn options_len(&self, cx: &App) -> usize {
        match self.part(cx).map(|p| p.widget) {
            Some(Widget::Choice { options }) | Some(Widget::MultiSelect { options }) => {
                options.len()
            }
            Some(Widget::Text { .. })
            | Some(Widget::Date { .. })
            | Some(Widget::Form { .. })
            | None => 0,
        }
    }

    fn next(&mut self, _: &NextOption, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.options_len(cx);
        if n > 0 {
            self.highlighted = (self.highlighted + 1) % n;
            cx.notify();
        }
    }

    fn prev(&mut self, _: &PrevOption, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.options_len(cx);
        if n > 0 {
            self.highlighted = (self.highlighted + n - 1) % n;
            cx.notify();
        }
    }

    fn toggle(&mut self, _: &ToggleOption, _: &mut Window, cx: &mut Context<Self>) {
        let Some(part) = self.part(cx) else {
            return;
        };
        if let Widget::MultiSelect { options } = &part.widget {
            if let Some(o) = options.get(self.highlighted) {
                self.toggle_id(o.id.clone(), cx);
            }
        }
    }

    fn toggle_id(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        self.error = None;
        cx.notify();
    }

    fn submit_action(&mut self, _: &SubmitAnswer, _: &mut Window, cx: &mut Context<Self>) {
        let Some(part) = self.part(cx) else {
            return;
        };
        // Enter on a choice card picks the highlighted option.
        if let Widget::Choice { options } = &part.widget {
            if let Some(o) = options.get(self.highlighted) {
                self.answer(json!(o.id), cx);
            }
            return;
        }
        self.submit(cx);
    }

    /// Collect the draft into a value, validate it, and send it.
    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(part) = self.part(cx) else {
            return;
        };
        if part.state == QuestionState::Answered {
            return;
        }
        let value = match &part.widget {
            Widget::Choice { options } => match options.get(self.highlighted) {
                Some(o) => Ok(json!(o.id)),
                None => Err("Pick one".to_string()),
            },
            Widget::MultiSelect { options } => {
                let picked: Vec<&str> = options
                    .iter()
                    .filter(|o| self.selected.contains(&o.id))
                    .map(|o| o.id.as_str())
                    .collect();
                if picked.is_empty() {
                    Err("Pick at least one".to_string())
                } else {
                    Ok(json!(picked))
                }
            }
            Widget::Text { .. } => {
                let text = self.input_text(&self.text_input, cx);
                if text.is_empty() {
                    Err("Type an answer".to_string())
                } else {
                    Ok(json!(text))
                }
            }
            Widget::Date { min, max } => {
                let text = self.input_text(&self.text_input, cx);
                check_date(&text, min.as_deref(), max.as_deref()).map(|d| json!(d))
            }
            Widget::Form { fields } => self.collect_form(fields, cx),
        };
        match value {
            Ok(v) => self.answer(v, cx),
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }

    fn collect_form(&self, fields: &[FormField], cx: &App) -> Result<Value, String> {
        let mut out = serde_json::Map::new();
        for f in fields {
            let v = match f.kind {
                FieldKind::Text => {
                    let t = self.input_text(&self.form_inputs.get(&f.id).cloned(), cx);
                    (!t.is_empty()).then(|| json!(t))
                }
                FieldKind::Number => {
                    let t = self.input_text(&self.form_inputs.get(&f.id).cloned(), cx);
                    if t.is_empty() {
                        None
                    } else {
                        let n: f64 = t
                            .parse()
                            .map_err(|_| format!("{}: not a number", f.label))?;
                        Some(json!(n))
                    }
                }
                FieldKind::Date => {
                    let t = self.input_text(&self.form_inputs.get(&f.id).cloned(), cx);
                    if t.is_empty() {
                        None
                    } else {
                        Some(json!(
                            check_date(&t, None, None).map_err(|e| format!("{}: {e}", f.label))?
                        ))
                    }
                }
                FieldKind::Choice => self.form_choice.get(&f.id).map(|id| json!(id)),
                FieldKind::Bool => Some(json!(self.form_bool.get(&f.id).copied().unwrap_or(false))),
            };
            match v {
                Some(v) => {
                    out.insert(f.id.clone(), v);
                }
                None if f.required => return Err(format!("{} is required", f.label)),
                None => {}
            }
        }
        Ok(Value::Object(out))
    }

    fn input_text(&self, input: &Option<Entity<ComposerInput>>, cx: &App) -> String {
        input
            .as_ref()
            .map(|i| i.read(cx).text().trim().to_string())
            .unwrap_or_default()
    }

    fn answer(&mut self, value: Value, cx: &mut Context<Self>) {
        self.error = None;
        let chat_id = self.chat_id.clone();
        let qid = self.question_id.clone();
        self.state
            .update(cx, |s, cx| s.answer_question(chat_id, qid, value, cx));
        cx.notify();
    }

    fn set_date(&mut self, iso: String, cx: &mut Context<Self>) {
        if let Some(input) = &self.text_input {
            input.update(cx, |i, cx| i.set_text(iso, cx));
        }
        self.error = None;
        cx.notify();
    }

    // ---- rendering ----------------------------------------------------------------------------

    fn chip(
        &self,
        id: impl Into<SharedString>,
        label: &str,
        selected: bool,
        highlighted: bool,
        enabled: bool,
        theme: &Theme,
    ) -> gpui::Stateful<Div> {
        div()
            .id(id.into())
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(8.0))
            .text_size(px(12.5))
            .border_1()
            .border_color(if highlighted {
                theme.accent
            } else if selected {
                Theme::with_alpha(theme.accent, 0.6)
            } else {
                theme.border_strong
            })
            .bg(if selected {
                theme.accent_soft
            } else {
                theme.surface_2
            })
            .text_color(if enabled || selected {
                theme.text
            } else {
                theme.text_faint
            })
            .when(enabled, |el| {
                el.cursor_pointer().hover(|s| s.border_color(theme.accent))
            })
            .child(label.to_string())
    }

    fn render_options(
        &self,
        options: &[ChoiceOption],
        multi: bool,
        picked: &BTreeSet<String>,
        open: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut row = div().flex().flex_wrap().gap(px(6.0));
        for (ix, o) in options.iter().enumerate() {
            let selected = picked.contains(&o.id);
            let highlighted = open && ix == self.highlighted;
            let id = o.id.clone();
            let chip = self
                .chip(
                    format!("{}-opt-{}", self.question_id, o.id),
                    &o.label,
                    selected,
                    highlighted,
                    open,
                    theme,
                )
                .when(open, |el| {
                    el.on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus_handle, cx);
                        this.highlighted = ix;
                        if multi {
                            this.toggle_id(id.clone(), cx);
                        } else {
                            this.answer(json!(id), cx);
                        }
                    }))
                });
            row = row.child(chip);
        }
        row
    }

    fn render_input_box(&self, input: &Entity<ComposerInput>, theme: &Theme) -> Div {
        div()
            .flex()
            .w_full()
            .min_h(px(32.0))
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(8.0))
            .bg(theme.surface_2)
            .border_1()
            .border_color(theme.border_strong)
            .child(input.clone())
    }

    fn submit_button(
        &self,
        label: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("{}-submit", self.question_id)))
            .px(px(12.0))
            .py(px(5.0))
            .rounded_full()
            .bg(theme.text)
            .text_color(theme.bg)
            .text_size(px(12.0))
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .child(label.to_string())
            .on_click(cx.listener(|this, _, _, cx| this.submit(cx)))
    }

    fn render_answered(&self, view: &PartView, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let answer = view.answer.clone().unwrap_or(Value::Null);
        match &view.widget {
            Widget::Choice { options } | Widget::MultiSelect { options } => {
                let picked: BTreeSet<String> = match &answer {
                    Value::Array(a) => a
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                    Value::String(s) => BTreeSet::from([s.clone()]),
                    _ => BTreeSet::new(),
                };
                self.render_options(options, false, &picked, false, theme, cx)
            }
            Widget::Text { .. } | Widget::Date { .. } | Widget::Form { .. } => {
                let text = view.widget.answer_text(&answer);
                let text = text
                    .strip_prefix("Answer:")
                    .unwrap_or(&text)
                    .trim()
                    .to_string();
                div()
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(8.0))
                    .bg(theme.surface_2)
                    .text_size(px(13.0))
                    .text_color(theme.text)
                    .child(text)
            }
        }
    }

    fn render_open(&self, view: &PartView, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let body = div().flex().flex_col().gap(px(8.0));
        match &view.widget {
            Widget::Choice { options } => body
                .child(self.render_options(options, false, &BTreeSet::new(), true, theme, cx))
                .child(self.hint("↑↓ move · Enter picks", theme)),
            Widget::MultiSelect { options } => body
                .child(self.render_options(options, true, &self.selected, true, theme, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(self.submit_button("Send", theme, cx))
                        .child(self.hint("↑↓ move · Space toggles · Enter sends", theme)),
                ),
            Widget::Text { .. } => {
                let input = self.text_input.clone().expect("text widget has an input");
                body.child(self.render_input_box(&input, theme)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(self.submit_button("Send", theme, cx))
                        .child(self.hint("Enter sends · Shift+Enter newline", theme)),
                )
            }
            Widget::Date { min, max } => {
                let input = self.text_input.clone().expect("date widget has an input");
                let today = today_iso();
                let quick: Vec<(&str, Option<String>)> = vec![
                    ("Today", Some(today.clone())),
                    ("Tomorrow", add_days(&today, 1)),
                    ("In a week", add_days(&today, 7)),
                    ("In a month", add_days(&today, 30)),
                ];
                let mut chips = div().flex().flex_wrap().gap(px(6.0));
                for (label, iso) in quick {
                    let Some(iso) = iso else { continue };
                    let within = check_date(&iso, min.as_deref(), max.as_deref()).is_ok();
                    let chip = self
                        .chip(
                            format!("{}-quick-{label}", self.question_id),
                            label,
                            false,
                            false,
                            within,
                            theme,
                        )
                        .when(within, |el| {
                            el.on_click(
                                cx.listener(move |this, _, _, cx| this.set_date(iso.clone(), cx)),
                            )
                        });
                    chips = chips.child(chip);
                }
                let bounds = match (min, max) {
                    (Some(a), Some(b)) => format!("{a} … {b}"),
                    (Some(a), None) => format!("from {a}"),
                    (None, Some(b)) => format!("until {b}"),
                    (None, None) => String::new(),
                };
                body.child(chips).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .w(px(160.0))
                                .child(self.render_input_box(&input, theme)),
                        )
                        .child(self.submit_button("Send", theme, cx))
                        .child(self.hint(&bounds, theme)),
                )
            }
            Widget::Form { fields } => {
                let mut rows = div().flex().flex_col().gap(px(8.0));
                for f in fields {
                    let label = div()
                        .w(px(120.0))
                        .flex_shrink_0()
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child(if f.required {
                            format!("{} *", f.label)
                        } else {
                            f.label.clone()
                        });
                    let control: Div = match f.kind {
                        FieldKind::Text | FieldKind::Number | FieldKind::Date => {
                            match self.form_inputs.get(&f.id) {
                                Some(input) => {
                                    div().flex_1().child(self.render_input_box(input, theme))
                                }
                                None => div(),
                            }
                        }
                        FieldKind::Choice => {
                            let picked: BTreeSet<String> =
                                self.form_choice.get(&f.id).cloned().into_iter().collect();
                            let mut row = div().flex().flex_wrap().gap(px(6.0));
                            for o in &f.options {
                                let fid = f.id.clone();
                                let oid = o.id.clone();
                                row = row.child(
                                    self.chip(
                                        format!("{}-{}-{}", self.question_id, f.id, o.id),
                                        &o.label,
                                        picked.contains(&o.id),
                                        false,
                                        true,
                                        theme,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.form_choice.insert(fid.clone(), oid.clone());
                                            this.error = None;
                                            cx.notify();
                                        },
                                    )),
                                );
                            }
                            row
                        }
                        FieldKind::Bool => {
                            let on = self.form_bool.get(&f.id).copied().unwrap_or(false);
                            let fid = f.id.clone();
                            div().child(
                                self.chip(
                                    format!("{}-{}-bool", self.question_id, f.id),
                                    if on { "Yes" } else { "No" },
                                    on,
                                    false,
                                    true,
                                    theme,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let v = this.form_bool.entry(fid.clone()).or_insert(false);
                                        *v = !*v;
                                        cx.notify();
                                    },
                                )),
                            )
                        }
                    };
                    rows = rows.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(label)
                            .child(control),
                    );
                }
                body.child(rows).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(self.submit_button("Send", theme, cx))
                        .child(self.hint("Enter in any field sends", theme)),
                )
            }
        }
    }

    fn hint(&self, text: &str, theme: &Theme) -> Div {
        div()
            .text_size(px(11.0))
            .text_color(theme.text_faint)
            .child(text.to_string())
    }
}

impl Focusable for QuestionCard {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for QuestionCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let Some(view) = self.part(cx) else {
            return div().into_any_element();
        };
        let open = view.state == QuestionState::Open;
        // The newest open card takes the keyboard once, so Enter/arrows work at once.
        if open && !self.focused_once {
            self.focused_once = true;
            match &self.text_input {
                Some(input) => {
                    let h = input.read(cx).focus_handle(cx);
                    window.focus(&h, cx);
                }
                None => {
                    if let Some(first) = self.form_inputs.values().next() {
                        let h = first.read(cx).focus_handle(cx);
                        window.focus(&h, cx);
                    } else {
                        window.focus(&self.focus_handle, cx);
                    }
                }
            }
        }
        let focused = self.focus_handle.is_focused(window);
        let label = if open { "QUESTION" } else { "ANSWERED" };
        let accent = if open { theme.warn } else { theme.accent };

        let header = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(accent)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(label),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child(view.widget.type_name().replace('_', " ")),
            );

        let body = if open {
            self.render_open(&view, &theme, cx)
        } else {
            self.render_answered(&view, &theme, cx)
        };

        div()
            .id(SharedString::from(format!("question-{}", self.question_id)))
            .key_context("QuestionCard")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::submit_action))
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::prev))
            .on_action(cx.listener(Self::toggle))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if this.text_input.is_none() && this.form_inputs.is_empty() {
                        window.focus(&this.focus_handle, cx);
                        cx.notify();
                    }
                }),
            )
            .w_full()
            .max_w(px(ASSISTANT_MAX_WIDTH))
            .my(px(4.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(PANEL_RADIUS))
            .bg(theme.card())
            .border_l_2()
            .border_color(accent)
            .when(focused && open, |el| {
                el.border_1()
                    .border_l_2()
                    .border_color(Theme::with_alpha(accent, 0.8))
            })
            .child(header)
            .child(
                div()
                    .text_size(px(13.5))
                    .text_color(theme.text)
                    .child(view.question.clone()),
            )
            .child(body)
            .when_some(self.error.clone(), |el, e| {
                el.child(div().text_size(px(11.5)).text_color(theme.danger).child(e))
            })
            .into_any_element()
    }
}
