//! The composer pill (§3.6): attach, hand-rolled multi-line text input with
//! IME (`EntityInputHandler`, zeron-study §4.6), model and reasoning pickers,
//! send/stop. Enter sends, Shift+Enter inserts a newline.

use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, Bounds, ClipboardItem, Context,
    CursorStyle, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render,
    SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::app::{AppState, ComposerMode};
use crate::model::Effort;
use crate::sync::fixture::MODELS;
use crate::ui::frost::{frosted, MENU_BLUR, PANEL_BLUR};
use crate::ui::theme::{Theme, COMPOSER_HEIGHT, PANEL_RADIUS};

actions!(
    composer,
    [
        Send,
        Newline,
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Copy,
        Cut,
        Escape,
    ]
);

pub const MAX_LINES: usize = 10;
pub const COMPOSER_RADIUS: f32 = 18.0;

pub type SubmitHandler = Rc<dyn Fn(&mut Window, &mut App)>;
const INPUT_LINE_HEIGHT: f32 = 20.0;
const INPUT_FONT_SIZE: f32 = 13.5;

#[derive(Clone, Debug)]
pub enum ComposerEvent {
    Send(String, Vec<PathBuf>),
    Stop,
}

struct Layout {
    lines: Vec<(usize, WrappedLine)>,
    bounds: Bounds<Pixels>,
}

pub struct ComposerInput {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    layout: Option<Layout>,
    is_selecting: bool,
    /// Vertical scroll inside the box once it reaches `MAX_LINES`.
    scroll_y: f32,
    /// Runs deferred, after this entity's update ends, so the owner may read
    /// and update the input freely.
    pub on_submit: Option<SubmitHandler>,
}

impl ComposerInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: "Do anything…".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            layout: None,
            is_selecting: false,
            scroll_y: 0.0,
            on_submit: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>) {
        self.placeholder = text.into();
    }

    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        let len = text.len();
        self.content = text;
        self.selected_range = len..len;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_y = 0.0;
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content.clear();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_y = 0.0;
        cx.notify();
    }

    pub fn line_count(&self) -> usize {
        self.layout
            .as_ref()
            .map(|l| {
                l.lines
                    .iter()
                    .map(|(_, line)| line.wrap_boundaries().len() + 1)
                    .sum::<usize>()
                    .max(1)
            })
            .unwrap_or(1)
    }

    fn send(&mut self, _: &Send, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cb) = self.on_submit.clone() {
            window.defer(cx, move |window, cx| cb(window, cx));
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let start = self.content[..self.cursor_offset()]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.move_to(start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let end = self.content[cursor..]
            .find('\n')
            .map(|i| cursor + i)
            .unwrap_or(self.content.len());
        self.move_to(end, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        let ix = self.index_for_mouse_position(event.position);
        if event.modifiers.shift {
            self.select_to(ix, cx);
        } else {
            self.move_to(ix, cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            let ix = self.index_for_mouse_position(event.position);
            self.select_to(ix, cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(layout) = self.layout.as_ref() else {
            return 0;
        };
        index_for_position(&layout.lines, layout.bounds, position, self.content.len())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    fn position_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        let layout = self.layout.as_ref()?;
        position_for_index(&layout.lines, index)
    }
}

/// Pixel position (relative to the text origin) of a byte index.
fn position_for_index(lines: &[(usize, WrappedLine)], index: usize) -> Option<Point<Pixels>> {
    let lh = px(INPUT_LINE_HEIGHT);
    let mut y = px(0.0);
    for (start, line) in lines {
        let end = start + line.len();
        if index >= *start && index <= end {
            let p = line.position_for_index(index - start, lh)?;
            return Some(point(p.x, p.y + y));
        }
        y += line.size(lh).height;
    }
    None
}

/// Byte index closest to a window position.
fn index_for_position(
    lines: &[(usize, WrappedLine)],
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
    content_len: usize,
) -> usize {
    let lh = px(INPUT_LINE_HEIGHT);
    if position.y < bounds.top() {
        return 0;
    }
    let mut y = bounds.top();
    let count = lines.len();
    for (i, (start, line)) in lines.iter().enumerate() {
        let h = line.size(lh).height;
        if position.y < y + h || i + 1 == count {
            let local = point(position.x - bounds.left(), (position.y - y).max(px(0.0)));
            let ix = match line.closest_index_for_position(local, lh) {
                Ok(ix) | Err(ix) => ix,
            };
            return (start + ix.min(line.len())).min(content_len);
        }
        y += h;
    }
    content_len
}

impl EntityInputHandler for ComposerInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            self.content[0..range.start].to_owned() + new_text + &self.content[range.end..];
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            self.content[0..range.start].to_owned() + new_text + &self.content[range.end..];
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let start = self.position_for_index(range.start)?;
        let end = self.position_for_index(range.end).unwrap_or(start);
        let dy = px(self.scroll_y);
        Some(Bounds::from_corners(
            point(bounds.left() + start.x, bounds.top() + start.y - dy),
            point(
                bounds.left() + end.x,
                bounds.top() + end.y + px(INPUT_LINE_HEIGHT) - dy,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let ix = self.index_for_mouse_position(point);
        Some(self.offset_to_utf16(ix))
    }
}

struct InputElement {
    input: Entity<ComposerInput>,
}

struct Prepaint {
    lines: Vec<(usize, WrappedLine)>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    scroll_y: f32,
}

impl IntoElement for InputElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InputElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let lines = self.input.read(cx).line_count().min(MAX_LINES);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(INPUT_LINE_HEIGHT * lines as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = Theme::of(cx).clone();
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let marked = input.marked_range.clone();
        let style = window.text_style();
        let lh = px(INPUT_LINE_HEIGHT);

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.to_string(), theme.text_faint)
        } else {
            (content.clone(), theme.text)
        };

        let mut font = style.font();
        font.family = theme.font_sans.into();
        let base_run = TextRun {
            len: 0,
            font,
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        // One WrappedLine per '\n'-separated paragraph.
        let mut lines = Vec::new();
        let mut start = 0usize;
        for para in display_text.split('\n') {
            let runs = runs_for(para, start, marked.as_ref(), &base_run);
            let shaped = window
                .text_system()
                .shape_text(
                    SharedString::from(para.to_string()),
                    px(INPUT_FONT_SIZE),
                    &runs,
                    Some(bounds.size.width),
                    None,
                )
                .ok()
                .and_then(|mut v| v.pop());
            if let Some(line) = shaped {
                lines.push((start, line));
            }
            start += para.len() + 1;
        }

        // Keep the caret inside the visible box once the text is taller than
        // MAX_LINES; everything below is painted shifted by -scroll_y.
        let total_h: f32 = lines
            .iter()
            .map(|(_, l)| f32::from(l.size(lh).height))
            .sum();
        let visible_h = f32::from(bounds.size.height);
        let mut scroll_y = input.scroll_y;
        let cursor_pos = if content.is_empty() {
            Some(point(px(0.0), px(0.0)))
        } else {
            position_for_index(&lines, cursor)
        };
        if let Some(p) = cursor_pos {
            let cy = f32::from(p.y);
            if cy < scroll_y {
                scroll_y = cy;
            } else if cy + f32::from(lh) > scroll_y + visible_h {
                scroll_y = cy + f32::from(lh) - visible_h;
            }
        }
        scroll_y = scroll_y.clamp(0.0, (total_h - visible_h).max(0.0));
        let dy = px(scroll_y);

        let mut cursor_quad = None;
        let mut selections = Vec::new();
        if content.is_empty() || selected_range.is_empty() {
            if let Some(pos) = cursor_pos {
                cursor_quad = Some(fill(
                    Bounds::new(
                        point(bounds.left() + pos.x, bounds.top() + pos.y - dy + px(1.0)),
                        size(px(1.5), lh - px(2.0)),
                    ),
                    theme.accent,
                ));
            }
        } else {
            let a = position_for_index(&lines, selected_range.start);
            let b = position_for_index(&lines, selected_range.end);
            if let (Some(a), Some(b)) = (a, b) {
                if a.y == b.y {
                    selections.push(fill(
                        Bounds::from_corners(
                            point(bounds.left() + a.x, bounds.top() + a.y - dy),
                            point(bounds.left() + b.x, bounds.top() + b.y - dy + lh),
                        ),
                        theme.accent_soft,
                    ));
                } else {
                    selections.push(fill(
                        Bounds::from_corners(
                            point(bounds.left() + a.x, bounds.top() + a.y - dy),
                            point(bounds.right(), bounds.top() + a.y - dy + lh),
                        ),
                        theme.accent_soft,
                    ));
                    if b.y > a.y + lh {
                        selections.push(fill(
                            Bounds::from_corners(
                                point(bounds.left(), bounds.top() + a.y - dy + lh),
                                point(bounds.right(), bounds.top() + b.y - dy),
                            ),
                            theme.accent_soft,
                        ));
                    }
                    selections.push(fill(
                        Bounds::from_corners(
                            point(bounds.left(), bounds.top() + b.y - dy),
                            point(bounds.left() + b.x, bounds.top() + b.y - dy + lh),
                        ),
                        theme.accent_soft,
                    ));
                }
            }
        }

        Prepaint {
            lines,
            cursor: cursor_quad,
            selections,
            scroll_y,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let lh = px(INPUT_LINE_HEIGHT);
        let scroll_y = prepaint.scroll_y;
        let focused = focus_handle.is_focused(window);
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for sel in prepaint.selections.drain(..) {
                window.paint_quad(sel);
            }
            let mut y = bounds.top() - px(scroll_y);
            for (_, line) in &prepaint.lines {
                let h = line.size(lh).height;
                if y + h >= bounds.top() && y <= bounds.bottom() {
                    let _ = line.paint(
                        point(bounds.left(), y),
                        lh,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
                y += h;
            }
            if focused {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
        });
        let lines = std::mem::take(&mut prepaint.lines);
        self.input.update(cx, |input, _| {
            input.layout = Some(Layout { lines, bounds });
            input.scroll_y = scroll_y;
        });
    }
}

fn runs_for(
    para: &str,
    para_start: usize,
    marked: Option<&Range<usize>>,
    base: &TextRun,
) -> Vec<TextRun> {
    let len = para.len();
    match marked {
        Some(m) if m.start < para_start + len && m.end > para_start => {
            let s = m.start.saturating_sub(para_start).min(len);
            let e = (m.end - para_start).min(len);
            vec![
                TextRun {
                    len: s,
                    ..base.clone()
                },
                TextRun {
                    len: e - s,
                    underline: Some(UnderlineStyle {
                        color: Some(base.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..base.clone()
                },
                TextRun {
                    len: len - e,
                    ..base.clone()
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect()
        }
        Some(_) | None => vec![TextRun {
            len,
            ..base.clone()
        }],
    }
}

impl Render for ComposerInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .key_context("ComposerInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .line_height(px(INPUT_LINE_HEIGHT))
            .text_size(px(INPUT_FONT_SIZE))
            .child(InputElement { input: cx.entity() })
    }
}

impl Focusable for ComposerInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Picker {
    Model,
    Effort,
}

pub struct Composer {
    state: Entity<AppState>,
    pub input: Entity<ComposerInput>,
    attachments: Vec<PathBuf>,
    /// Which chat the input text belongs to; drafts are kept per chat.
    draft_chat: Option<crate::model::Id>,
    drafts: std::collections::HashMap<crate::model::Id, String>,
    open_picker: Option<Picker>,
    /// S8 shake when the user hits Cmd+W on the pinned tab; also reused for
    /// the empty-send nudge.
    pub nudge_at: Option<u64>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(ComposerInput::new);
        let weak = cx.weak_entity();
        input.update(cx, |input, _| {
            input.on_submit = Some(Rc::new(move |window, cx| {
                let _ = weak.update(cx, |composer, cx| composer.submit(window, cx));
            }));
        });
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            input,
            attachments: Vec::new(),
            draft_chat: None,
            drafts: std::collections::HashMap::new(),
            open_picker: None,
            nudge_at: None,
        }
    }

    /// Swap the draft when the active chat changes.
    fn sync_draft(&mut self, cx: &mut Context<Self>) {
        let active = self.state.read(cx).active_chat_id();
        if active == self.draft_chat {
            return;
        }
        let text = self.input.read(cx).text().to_string();
        if let Some(prev) = self.draft_chat.take() {
            if text.is_empty() {
                self.drafts.remove(&prev);
            } else {
                self.drafts.insert(prev, text);
            }
        }
        let next = active
            .as_ref()
            .and_then(|id| self.drafts.get(id).cloned())
            .unwrap_or_default();
        self.input.update(cx, |input, cx| input.set_text(next, cx));
        self.draft_chat = active;
    }

    pub fn focus(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let handle = this.read(cx).input.read(cx).focus_handle.clone();
        window.focus(&handle, cx);
    }

    fn mode(&self, cx: &App) -> ComposerMode {
        let state = self.state.read(cx);
        state
            .active_chat_id()
            .map(|id| state.composer_mode(&id))
            .unwrap_or(ComposerMode::Send)
    }

    fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        match self.mode(cx) {
            ComposerMode::Send | ComposerMode::Answer => {
                let text = self.input.read(cx).text().trim().to_string();
                if text.is_empty() && self.attachments.is_empty() {
                    self.nudge_at = Some(crate::model::now_ms());
                    cx.notify();
                    return;
                }
                let attachments = std::mem::take(&mut self.attachments);
                self.input.update(cx, |input, cx| input.clear(cx));
                cx.emit(ComposerEvent::Send(text, attachments));
            }
        }
        self.open_picker = None;
        cx.notify();
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |composer, cx| {
                    composer.attachments.extend(paths);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn render_picker_menu(&self, picker: Picker, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let state = self.state.read(cx);
        let (model, effort) = state
            .selected()
            .map(|p| (p.model.clone(), p.effort))
            .unwrap_or((MODELS[0].to_string(), Effort::High));
        let mut menu = div()
            .id("picker-menu")
            .flex()
            .flex_col()
            .min_w(px(220.0))
            .max_h(px(320.0))
            .overflow_y_scroll()
            .p(px(4.0))
            .rounded(px(PANEL_RADIUS))
            .bg(theme.card())
            .border_1()
            .border_color(theme.border_strong)
            .shadow_lg();
        match picker {
            Picker::Model => {
                let mut names: Vec<String> = if state.models.is_empty() {
                    MODELS.iter().map(|m| m.to_string()).collect()
                } else {
                    state.models.clone()
                };
                if !names.contains(&model) {
                    names.insert(0, model.clone());
                }
                for (i, name) in names.iter().enumerate() {
                    let selected = *name == model;
                    let name_owned = name.to_string();
                    menu = menu.child(
                        div()
                            .id(ElementId::NamedInteger("model-item".into(), i as u64))
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(6.0))
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .hover(|s| s.bg(theme.surface_2))
                            .cursor_pointer()
                            .child(name.clone())
                            .when(selected, |el| {
                                el.child(div().text_color(theme.accent).child("✓"))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state
                                    .update(cx, |s, cx| s.set_model(name_owned.clone(), cx));
                                this.open_picker = None;
                                cx.notify();
                            })),
                    );
                }
            }
            Picker::Effort => {
                for (i, level) in Effort::ALL.iter().enumerate() {
                    let selected = *level == effort;
                    let level = *level;
                    menu = menu.child(
                        div()
                            .id(ElementId::NamedInteger("effort-item".into(), i as u64))
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(6.0))
                            .text_size(px(13.0))
                            .text_color(theme.text)
                            .hover(|s| s.bg(theme.surface_2))
                            .cursor_pointer()
                            .child(level.label())
                            .when(selected, |el| {
                                el.child(div().text_color(theme.accent).child("✓"))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.state.update(cx, |s, cx| s.set_effort(level, cx));
                                this.open_picker = None;
                                cx.notify();
                            })),
                    );
                }
            }
        }
        // Opens upward from the picker row.
        div()
            .absolute()
            .bottom(px(COMPOSER_HEIGHT + 44.0))
            .right(px(56.0))
            .child(frosted(PANEL_RADIUS, MENU_BLUR, menu))
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_draft(cx);
        let theme = Theme::of(cx).clone();
        let mode = self.mode(cx);
        let state = self.state.read(cx);
        let (model, effort) = state
            .selected()
            .map(|p| (p.model.clone(), p.effort))
            .unwrap_or((MODELS[0].to_string(), Effort::High));
        let link_full = state.link == crate::model::Link::Reconnecting;

        self.input.update(cx, |input, _| {
            input.set_placeholder(match mode {
                ComposerMode::Answer => "Answer the agent…",
                ComposerMode::Send => "Do anything…",
            });
        });

        let nudge = self
            .nudge_at
            .map(|t| crate::model::now_ms().saturating_sub(t) < 300)
            .unwrap_or(false);
        if nudge {
            crate::ui::motion::PulseClock::request(cx);
        }

        let border_color = match mode {
            ComposerMode::Answer => theme.warn,
            ComposerMode::Send => {
                if nudge {
                    theme.danger
                } else {
                    theme.border_strong
                }
            }
        };

        let send_button = div()
            .id("composer-send")
            .size(px(30.0))
            .rounded_full()
            .bg(theme.text)
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_color(theme.bg)
            .text_size(px(15.0))
            .font_weight(gpui::FontWeight::BOLD)
            .child("↑")
            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)));

        // Running tasks: small indicators, never a blocking banner (multitask).
        let tasks = {
            let state = self.state.read(cx);
            state
                .active_chat_id()
                .map(|id| state.running_tasks(&id))
                .unwrap_or_default()
        };
        // Turns streaming in this chat already show their working line in the
        // transcript; the strip lists the other running work (project
        // subagents) and offers Stop for the chat's own turns.
        let streaming_here = tasks.iter().filter(|t| t.stoppable).count();
        let others: Vec<&crate::app::RunningTask> = tasks.iter().filter(|t| !t.stoppable).collect();
        let tasks_strip = if tasks.is_empty() {
            None
        } else {
            crate::ui::motion::PulseClock::request(cx);
            let phase = crate::ui::motion::PulseClock::phase(cx, 900);
            let mut strip = div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .px(px(14.0))
                .pb(px(6.0));
            for (i, task) in others.iter().enumerate() {
                strip = strip.child(
                    div()
                        .id(ElementId::NamedInteger("running-task".into(), i as u64))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .size(px(6.0))
                                .rounded_full()
                                .bg(Theme::with_alpha(theme.accent, 0.35 + 0.65 * phase)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(11.5))
                                .text_color(theme.text_muted)
                                .child(task.label.clone()),
                        ),
                );
            }
            if streaming_here > 0 {
                strip = strip.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap(px(8.0))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.text_faint)
                                .child(if streaming_here == 1 {
                                    "1 turn running".to_string()
                                } else {
                                    format!("{streaming_here} turns running")
                                }),
                        )
                        .child(
                            div()
                                .id("tasks-stop")
                                .text_size(px(11.5))
                                .text_color(theme.text_faint)
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme.danger))
                                .child("■ Stop")
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(ComposerEvent::Stop);
                                })),
                        ),
                );
            }
            Some(strip)
        };

        let mut pill = div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(COMPOSER_RADIUS))
            .bg(theme.card())
            .border_1()
            .border_color(border_color)
            .when(mode == ComposerMode::Answer, |el| el.border_t_2())
            .shadow_sm();

        if !self.attachments.is_empty() {
            let mut chips = div()
                .flex()
                .flex_wrap()
                .gap(px(6.0))
                .px(px(14.0))
                .pt(px(10.0));
            for (i, path) in self.attachments.iter().enumerate() {
                let name_owned = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("attachment")
                    .to_string();
                chips = chips.child(
                    div()
                        .id(ElementId::NamedInteger("attachment".into(), i as u64))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(6.0))
                        .bg(theme.surface_2)
                        .text_size(px(11.5))
                        .text_color(theme.text_muted)
                        .child(name_owned)
                        .child(
                            div()
                                .text_color(theme.text_faint)
                                .child("×")
                                .cursor_pointer(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if i < this.attachments.len() {
                                this.attachments.remove(i);
                            }
                            cx.notify();
                        })),
                );
            }
            pill = pill.child(chips);
        }

        // Text uses the full width; the controls live in their own bottom
        // row, so growing text never moves them (Cursor composer).
        let text_row = div()
            .w_full()
            .flex()
            .px(px(14.0))
            .pt(px(11.0))
            .pb(px(4.0))
            .child(self.input.clone());

        let controls = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .pl(px(9.0))
            .pr(px(9.0))
            .pb(px(8.0))
            .pt(px(2.0))
            .child(
                div()
                    .id("composer-attach")
                    .size(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border_strong)
                    .text_color(theme.text_muted)
                    .text_size(px(15.0))
                    .hover(|s| s.bg(theme.surface_2))
                    .cursor_pointer()
                    .child("+")
                    .on_click(cx.listener(|this, _, _, cx| this.attach(cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("composer-model")
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text)
                    .hover(|s| s.bg(theme.surface_2))
                    .cursor_pointer()
                    .child(model)
                    .child(div().text_color(theme.text_faint).child("⌄"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_picker = match this.open_picker {
                            Some(Picker::Model) => None,
                            Some(Picker::Effort) | None => Some(Picker::Model),
                        };
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("composer-effort")
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded(px(6.0))
                    .text_size(px(12.5))
                    .text_color(theme.text_muted)
                    .hover(|s| s.bg(theme.surface_2))
                    .cursor_pointer()
                    .child(effort.label())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_picker = match this.open_picker {
                            Some(Picker::Effort) => None,
                            Some(Picker::Model) | None => Some(Picker::Effort),
                        };
                        cx.notify();
                    })),
            )
            .child(send_button);

        pill = pill.child(text_row).child(controls);

        if link_full {
            pill = pill.child(
                div()
                    .px(px(16.0))
                    .pb(px(8.0))
                    .text_size(px(11.0))
                    .text_color(theme.text_faint)
                    .child("Reconnecting… messages queue locally and sync when the relay is back."),
            );
        }

        let mut root = div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .when_some(tasks_strip, |el, strip| el.child(strip))
            .child(frosted(COMPOSER_RADIUS, PANEL_BLUR, pill));
        if let Some(picker) = self.open_picker {
            root = root.child(self.render_picker_menu(picker, cx));
        }
        root
    }
}
