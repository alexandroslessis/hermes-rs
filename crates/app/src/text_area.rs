//! A multi-line, word-wrapping text input for the composer.
//!
//! Extended from GPUI's `examples/input.rs` (Apache-2.0): same IME handling (EntityInputHandler),
//! selection and clipboard, plus multi-line layout with wrapping, vertical movement that keeps
//! the column, word movement, and a height that grows to `max_lines` and then scrolls.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollEvent, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
    actions, div, fill, point, prelude::*, px, relative, size,
};
use theme::ActiveTheme;
use unicode_segmentation::UnicodeSegmentation;

actions!(
    text_area,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        Left,
        Right,
        Up,
        Down,
        WordLeft,
        WordRight,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectWordLeft,
        SelectWordRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        Newline,
        Submit,
        Paste,
        Cut,
        Copy,
    ]
);

const CONTEXT: &str = "TextArea";

pub fn bind_keys(cx: &mut App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("ctrl-left", WordLeft, c),
        KeyBinding::new("ctrl-right", WordRight, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, c),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, c),
        KeyBinding::new("ctrl-a", SelectAll, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("shift-home", SelectHome, c),
        KeyBinding::new("shift-end", SelectEnd, c),
        KeyBinding::new("shift-enter", Newline, c),
        KeyBinding::new("enter", Submit, c),
        KeyBinding::new("ctrl-v", Paste, c),
        KeyBinding::new("ctrl-c", Copy, c),
        KeyBinding::new("ctrl-x", Cut, c),
    ]);
}

pub enum TextAreaEvent {
    Submit,
    Changed,
    /// An image was pasted (the composer attaches it).
    PastedImage(gpui::Image),
    /// Files were pasted (copied in Explorer).
    PastedPaths(Vec<std::path::PathBuf>),
}

struct LayoutLine {
    wrapped: WrappedLine,
    start: usize,
    top: Pixels,
    height: Pixels,
}

#[derive(Default)]
struct TextLayout {
    lines: Vec<LayoutLine>,
    line_height: Pixels,
    content_height: Pixels,
}

impl TextLayout {
    fn line_for_index(&self, index: usize) -> Option<&LayoutLine> {
        self.lines.iter().rev().find(|line| line.start <= index)
    }

    /// Top-left of the caret for `index`, in content coordinates.
    fn position_for_index(&self, index: usize) -> Point<Pixels> {
        let Some(line) = self.line_for_index(index) else {
            return Point::default();
        };
        let local = index.saturating_sub(line.start).min(line.wrapped.len());
        let position = line
            .wrapped
            .position_for_index(local, self.line_height)
            .unwrap_or_default();
        point(position.x, line.top + position.y)
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let Some(line) = self
            .lines
            .iter()
            .find(|line| position.y < line.top + line.height)
            .or(self.lines.last())
        else {
            return 0;
        };
        let local = point(
            position.x.max(px(0.)),
            (position.y - line.top).clamp(px(0.), line.height - px(1.)),
        );
        let index = match line
            .wrapped
            .closest_index_for_position(local, self.line_height)
        {
            Ok(index) | Err(index) => index,
        };
        line.start + index.min(line.wrapped.len())
    }
}

pub struct TextArea {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    layout: Rc<RefCell<TextLayout>>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// The x position Up/Down try to keep, so moving through short lines doesn't drift.
    goal_x: Option<Pixels>,
    scroll_top: Pixels,
    max_lines: usize,
    /// Zed's cursor blink: whether the caret shows this half-period, the running blink's
    /// generation (a newer one stops older timers), and where the caret was last frame.
    cursor_visible: bool,
    blink_epoch: usize,
    blinking: bool,
    last_caret: Option<(usize, usize)>,
}

/// Zed's `CURSOR_BLINK_INTERVAL`.
const CURSOR_BLINK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

impl EventEmitter<TextAreaEvent> for TextArea {}

impl TextArea {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            layout: Rc::default(),
            last_bounds: None,
            is_selecting: false,
            goal_x: None,
            scroll_top: px(0.),
            max_lines: 12,
            cursor_visible: true,
            blink_epoch: 0,
            blinking: false,
            last_caret: None,
        }
    }

    /// Runs each frame while focused. Like Zed's `BlinkManager`, the caret blinks every 500ms,
    /// and shows steadily again (restarting the blink) whenever it moves or the text changes.
    fn blink_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let caret = (self.cursor_offset(), self.content.len());
        if self.blinking && self.last_caret == Some(caret) {
            return;
        }
        self.last_caret = Some(caret);
        self.cursor_visible = true;
        self.blinking = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CURSOR_BLINK_INTERVAL).await;
                let keep_blinking = this
                    .update_in(cx, |this, window, cx| {
                        if this.blink_epoch != epoch {
                            return false;
                        }
                        if !this.focus_handle.is_focused(window) {
                            this.blinking = false;
                            this.cursor_visible = true;
                            return false;
                        }
                        this.cursor_visible = !this.cursor_visible;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_blinking {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.content = text.into();
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.goal_x = None;
        cx.emit(TextAreaEvent::Changed);
        cx.notify();
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// A one-line field: Enter submits, long text scrolls instead of growing.
    pub fn single_line(mut self) -> Self {
        self.max_lines = 1;
        self
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    /// The nearest valid cursor position: within the text and on a character boundary. (An
    /// empty field lays out its placeholder, so a click can land past the real text.)
    fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.content.len());
        while !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn move_to(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        let offset = self.clamp(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        let offset = self.clamp(offset);
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace(
        &mut self,
        range: Range<usize>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.clamp(range.start)..self.clamp(range.end.max(range.start));
        self.content.replace_range(range.clone(), text);
        let end = range.start + text.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.goal_x = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(TextAreaEvent::Changed);
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        self.content[..offset]
            .unicode_word_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content[offset..]
            .unicode_word_indices()
            .next()
            .map(|(index, word)| offset + index + word.len())
            .unwrap_or(self.content.len())
    }

    fn line_start(&self, offset: usize) -> usize {
        self.content[..offset]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.content[offset..]
            .find('\n')
            .map(|i| offset + i)
            .unwrap_or(self.content.len())
    }

    /// The offset one visual row above or below the cursor.
    fn vertical_target(&mut self, rows: f32) -> usize {
        let layout = self.layout.borrow();
        let cursor = layout.position_for_index(self.cursor_offset());
        let x = *self.goal_x.get_or_insert(cursor.x);
        let y = cursor.y + layout.line_height * rows + layout.line_height / 2.;
        if y < px(0.) {
            return 0;
        }
        if y >= layout.content_height {
            return self.content.len();
        }
        layout.index_for_position(point(x, y))
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), window, cx);
        } else {
            self.move_to(self.selected_range.start, window, cx);
        }
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), window, cx);
        } else {
            self.move_to(self.selected_range.end, window, cx);
        }
    }

    fn up(&mut self, _: &Up, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical_target(-1.);
        self.move_to(target, window, cx);
    }

    fn down(&mut self, _: &Down, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical_target(1.);
        self.move_to(target, window, cx);
    }

    fn word_left(&mut self, _: &WordLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.previous_word(self.cursor_offset()), window, cx);
    }

    fn word_right(&mut self, _: &WordRight, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.next_word(self.cursor_offset()), window, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.select_to(self.previous_boundary(self.cursor_offset()), window, cx);
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.select_to(self.next_boundary(self.cursor_offset()), window, cx);
    }

    fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical_target(-1.);
        self.select_to(target, window, cx);
    }

    fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.vertical_target(1.);
        self.select_to(target, window, cx);
    }

    fn select_word_left(
        &mut self,
        _: &SelectWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.previous_word(self.cursor_offset()), window, cx);
    }

    fn select_word_right(
        &mut self,
        _: &SelectWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.next_word(self.cursor_offset()), window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.line_start(self.cursor_offset()), window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.line_end(self.cursor_offset()), window, cx);
    }

    fn select_home(&mut self, _: &SelectHome, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start(self.cursor_offset()), window, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end(self.cursor_offset()), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let start = self.previous_boundary(self.cursor_offset());
            self.selected_range = start..self.cursor_offset();
        }
        self.replace(self.selected_range.clone(), "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            let start = self.previous_word(self.cursor_offset());
            self.selected_range = start..self.cursor_offset();
        }
        self.replace(self.selected_range.clone(), "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let end = self.next_boundary(self.cursor_offset());
            self.selected_range = self.cursor_offset()..end;
        }
        self.replace(self.selected_range.clone(), "", window, cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace(self.selected_range.clone(), "\n", window, cx);
    }

    fn submit(&mut self, _: &Submit, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextAreaEvent::Submit);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        // Text wins (Excel cells come with a picture of themselves too); otherwise images and
        // copied files go to whoever owns this text area (the composer attaches them).
        if let Some(text) = item.text() {
            let text = text.replace("\r\n", "\n");
            self.replace(self.selected_range.clone(), &text, window, cx);
            return;
        }
        for entry in item.entries() {
            match entry {
                gpui::ClipboardEntry::Image(image) => {
                    cx.emit(TextAreaEvent::PastedImage(image.clone()));
                }
                gpui::ClipboardEntry::ExternalPaths(paths) => {
                    cx.emit(TextAreaEvent::PastedPaths(paths.paths().to_vec()));
                }
                gpui::ClipboardEntry::String(_) => {}
            }
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
            self.replace(self.selected_range.clone(), "", window, cx);
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        let local = point(
            position.x - bounds.left(),
            position.y - bounds.top() + self.scroll_top,
        );
        self.clamp(self.layout.borrow().index_for_position(local))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;
        self.goal_x = None;
        let index = self.index_for_mouse_position(event.position);
        if event.modifiers.shift {
            self.select_to(index, window, cx);
        } else if event.click_count == 2 {
            let start = self.previous_word(self.next_boundary(index).min(self.content.len()));
            let end = self.next_word(start);
            self.selected_range = start..end;
            cx.notify();
        } else {
            self.move_to(index, window, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout.borrow();
        let visible = self.last_bounds.map(|b| b.size.height).unwrap_or_default();
        let max_scroll = (layout.content_height - visible).max(px(0.));
        let delta = event.delta.pixel_delta(layout.line_height).y;
        drop(layout);
        self.scroll_top = (self.scroll_top - delta).clamp(px(0.), max_scroll);
        let _ = window;
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset.min(self.content.len())]
            .encode_utf16()
            .count()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let text = new_text.replace("\r\n", "\n").replace('\r', "\n");
        self.replace(range, &text, window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let range = self.clamp(range.start)..self.clamp(range.end.max(range.start));
        self.content.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        let start_utf16 = self.offset_to_utf16(range.start);
        let inserted_utf16 = new_text.encode_utf16().count();
        self.selected_range = new_selected_range_utf16
            .map(|selected| {
                self.range_from_utf16(
                    &(start_utf16 + selected.start.min(inserted_utf16)
                        ..start_utf16 + selected.end.min(inserted_utf16)),
                )
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(TextAreaEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let layout = self.layout.borrow();
        let start = layout.position_for_index(range.start);
        let end = layout.position_for_index(range.end);
        let offset = point(bounds.left(), bounds.top() - self.scroll_top);
        Some(Bounds::from_corners(
            start + offset,
            point(end.x, end.y + layout.line_height) + offset,
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.index_for_mouse_position(point);
        Some(self.offset_to_utf16(index))
    }
}

struct TextAreaElement {
    input: Entity<TextArea>,
}

struct PrepaintState {
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for TextAreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn runs_for(text_len: usize, marked: Option<&Range<usize>>, run: TextRun) -> Vec<TextRun> {
    match marked {
        Some(marked) => [
            TextRun {
                len: marked.start,
                ..run.clone()
            },
            TextRun {
                len: marked.end - marked.start,
                underline: Some(UnderlineStyle {
                    color: Some(run.color),
                    thickness: px(1.),
                    wavy: false,
                }),
                ..run.clone()
            },
            TextRun {
                len: text_len - marked.end,
                ..run
            },
        ]
        .into_iter()
        .filter(|run| run.len > 0)
        .collect(),
        None => vec![run],
    }
}

impl Element for TextAreaElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let (text, color): (SharedString, _) = if input.content.is_empty() {
            (
                input.placeholder.clone(),
                cx.theme().colors().text_placeholder,
            )
        } else {
            (input.content.clone().into(), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if input.content.is_empty() {
            vec![run]
        } else {
            runs_for(text.len(), input.marked_range.as_ref(), run)
        };
        let shared = input.layout.clone();
        let max_height = line_height * input.max_lines as f32;
        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.).into();

        let layout_id =
            window.request_measured_layout(layout_style, move |known, available, window, _cx| {
                let width = known.width.or(match available.width {
                    gpui::AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let lines = window
                    .text_system()
                    .shape_text(text.clone(), font_size, &runs, width, None)
                    .unwrap_or_default();
                let mut layout = TextLayout {
                    lines: Vec::new(),
                    line_height,
                    content_height: px(0.),
                };
                let mut start = 0;
                let mut top = px(0.);
                for wrapped in lines {
                    let height = wrapped.size(line_height).height.max(line_height);
                    let len = wrapped.len();
                    layout.lines.push(LayoutLine {
                        wrapped,
                        start,
                        top,
                        height,
                    });
                    start += len + 1;
                    top += height;
                }
                layout.content_height = top.max(line_height);
                let height = layout.content_height.min(max_height);
                *shared.borrow_mut() = layout;
                size(width.unwrap_or(px(0.)), height)
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let player = cx.theme().players().local();
        let (cursor_index, selection, layout, scroll_top, cursor_visible) =
            self.input.update(cx, |input, cx| {
                if input.focus_handle.is_focused(window) {
                    input.blink_frame(window, cx);
                }
                // Keep the caret visible inside the scrolled viewport.
                let layout = input.layout.clone();
                let caret = layout.borrow().position_for_index(input.cursor_offset());
                let line_height = layout.borrow().line_height;
                let visible = bounds.size.height;
                if caret.y < input.scroll_top {
                    input.scroll_top = caret.y;
                } else if caret.y + line_height > input.scroll_top + visible {
                    input.scroll_top = caret.y + line_height - visible;
                }
                let max_scroll = (layout.borrow().content_height - visible).max(px(0.));
                input.scroll_top = input.scroll_top.clamp(px(0.), max_scroll);
                input.last_bounds = Some(bounds);
                (
                    input.cursor_offset(),
                    input.selected_range.clone(),
                    layout,
                    input.scroll_top,
                    input.cursor_visible,
                )
            });
        let layout = layout.borrow();
        let origin = point(bounds.left(), bounds.top() - scroll_top);
        let line_height = layout.line_height;

        let mut selections = Vec::new();
        if !selection.is_empty() {
            let start = layout.position_for_index(selection.start);
            let end = layout.position_for_index(selection.end);
            let mut row_top = start.y;
            while row_top <= end.y {
                let left = if row_top == start.y { start.x } else { px(0.) };
                let right = if row_top == end.y {
                    end.x
                } else {
                    bounds.size.width
                };
                if right > left {
                    selections.push(fill(
                        Bounds::from_corners(
                            origin + point(left, row_top),
                            origin + point(right, row_top + line_height),
                        ),
                        player.selection,
                    ));
                }
                row_top += line_height;
            }
        }
        // The caret spans the text's height, centered in the line, not the full line spacing.
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let caret_height = (font_size * 1.3).min(line_height);
        let cursor = (selection.is_empty() && cursor_visible).then(|| {
            let caret = layout.position_for_index(cursor_index);
            let top = caret.y + (line_height - caret_height) / 2.;
            fill(
                Bounds::new(origin + point(caret.x, top), size(px(2.), caret_height)),
                player.cursor,
            )
        });
        let _ = window;
        PrepaintState { selections, cursor }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let input = self.input.read(cx);
        let focus_handle = input.focus_handle.clone();
        let layout = input.layout.clone();
        let scroll_top = input.scroll_top;
        // While a drag selects, follow the mouse anywhere in the window, as Zed's editor does: a
        // fast drag leaves the field between two move events, and the field's own move
        // handler only hears moves over it.
        if input.is_selecting {
            let entity = self.input.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble
                    || event.pressed_button != Some(MouseButton::Left)
                {
                    return;
                }
                entity.update(cx, |input, cx| {
                    if input.is_selecting {
                        let index = input.index_for_mouse_position(event.position);
                        input.select_to(index, window, cx);
                    }
                });
            });
        }
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in prepaint.selections.drain(..) {
                window.paint_quad(selection);
            }
            let layout = layout.borrow();
            for line in &layout.lines {
                let origin = point(bounds.left(), bounds.top() - scroll_top + line.top);
                let _ = line.wrapped.paint(
                    origin,
                    layout.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
        });
    }
}

impl Render for TextArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-area")
            .w_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(TextAreaElement { input: cx.entity() })
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
