//! A plain-text field, single or multi-line, with wrapping, selection, clipboard and IME.
//! Grown from gpui's input example; replace it with the component library's field when it lands.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, Hsla, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PaintQuad, Pixels, Point, SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div,
    fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::theme;

actions!(
    text_field,
    [
        Backspace,
        Delete,
        DeleteWordLeft,
        DeleteToLineStart,
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
        DocStart,
        DocEnd,
        Paste,
        Cut,
        Copy,
        Submit,
        SubmitAlways,
        Newline,
        Escape,
        ShowCharacterPalette,
    ]
);

pub const CONTEXT: &str = "TextField";

pub fn bind_keys(cx: &mut App) {
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("alt-backspace", DeleteWordLeft, c),
        KeyBinding::new("cmd-backspace", DeleteToLineStart, c),
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("alt-left", WordLeft, c),
        KeyBinding::new("alt-right", WordRight, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("alt-shift-left", SelectWordLeft, c),
        KeyBinding::new("alt-shift-right", SelectWordRight, c),
        KeyBinding::new("cmd-a", SelectAll, c),
        KeyBinding::new("home", Home, c),
        KeyBinding::new("end", End, c),
        KeyBinding::new("cmd-left", Home, c),
        KeyBinding::new("cmd-right", End, c),
        KeyBinding::new("cmd-shift-left", SelectHome, c),
        KeyBinding::new("cmd-shift-right", SelectEnd, c),
        KeyBinding::new("cmd-up", DocStart, c),
        KeyBinding::new("cmd-down", DocEnd, c),
        KeyBinding::new("cmd-v", Paste, c),
        KeyBinding::new("cmd-c", Copy, c),
        KeyBinding::new("cmd-x", Cut, c),
        KeyBinding::new("enter", Submit, c),
        KeyBinding::new("shift-enter", Newline, c),
        KeyBinding::new("alt-enter", Newline, c),
        KeyBinding::new("cmd-enter", SubmitAlways, c),
        KeyBinding::new("escape", Escape, c),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, c),
    ]);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFieldEvent {
    Changed,
    /// Enter; in a field where Enter makes a new line, Cmd-Enter.
    Submit,
    Escape,
    Blurred,
    Focused,
}

/// Where a multi-line field puts Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterKey {
    /// Enter submits and Shift-Enter breaks the line, as in a chat composer.
    Submits,
    /// Enter breaks the line and Cmd-Enter submits, as in a description.
    Breaks,
}

struct Layout {
    lines: Vec<WrappedLine>,
    /// Byte offset where each paragraph starts.
    starts: Vec<usize>,
    line_height: Pixels,
    bounds: Bounds<Pixels>,
}

pub struct TextField {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    multiline: bool,
    enter: EnterKey,
    min_lines: usize,
    max_lines: usize,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    layout: Option<Layout>,
    /// Keeps the cursor in view when the text is taller than the field.
    scroll_top: Pixels,
    is_selecting: bool,
    disabled: bool,
    /// The column to return to when moving up or down past shorter lines.
    goal_x: Option<Pixels>,
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl TextField {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> TextField {
        let focus_handle = cx.focus_handle();
        cx.on_blur(&focus_handle, window, |_, _, cx| cx.emit(TextFieldEvent::Blurred)).detach();
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.emit(TextFieldEvent::Focused)).detach();
        TextField {
            focus_handle,
            content: String::new(),
            placeholder: SharedString::default(),
            multiline: false,
            enter: EnterKey::Submits,
            min_lines: 1,
            max_lines: 1,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            layout: None,
            scroll_top: px(0.),
            is_selecting: false,
            disabled: false,
            goal_x: None,
        }
    }

    pub fn multiline(mut self, min_lines: usize, max_lines: usize, enter: EnterKey) -> Self {
        self.multiline = true;
        self.min_lines = min_lines.max(1);
        self.max_lines = max_lines.max(self.min_lines);
        self.enter = enter;
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.content = self.clean(&text.into());
        self.selected_range = self.content.len()..self.content.len();
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    /// Replaces the text without emitting Changed: for values arriving from elsewhere.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.content = self.clean(&text.into());
        self.selected_range = self.content.len()..self.content.len();
        self.marked_range = None;
        self.scroll_top = px(0.);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
        cx.emit(TextFieldEvent::Changed);
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus_handle);
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn clean(&self, text: &str) -> String {
        if self.multiline { text.replace("\r\n", "\n") } else { text.replace(['\r', '\n'], " ") }
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).rev().find_map(|(i, _)| (i < offset).then_some(i)).unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).find_map(|(i, _)| (i > offset).then_some(i)).unwrap_or(self.content.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        self.content.unicode_word_indices().rev().find_map(|(i, _)| (i < offset).then_some(i)).unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .find_map(|(i, word)| (i + word.len() > offset).then_some(i + word.len()))
            .unwrap_or(self.content.len())
    }

    fn line_start(&self, offset: usize) -> usize {
        self.content[..offset].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.content[offset..].find('\n').map_or(self.content.len(), |i| offset + i)
    }

    fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, text, window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.insert("", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.insert("", window, cx);
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_word(self.cursor_offset()), cx);
        }
        self.insert("", window, cx);
    }

    fn delete_to_line_start(&mut self, _: &DeleteToLineStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.line_start(self.cursor_offset()), cx);
        }
        self.insert("", window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_text(cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end(self.cursor_offset()), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start(self.cursor_offset()), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end(self.cursor_offset()), cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    /// The offset one visual line above or below the cursor, keeping its column.
    fn vertical(&mut self, lines: f32) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let position = position_for(layout, self.cursor_offset())?;
        let x = *self.goal_x.get_or_insert(position.x);
        let target = point(x, position.y + layout.line_height * lines + layout.line_height / 2.);
        if target.y < px(0.) {
            return Some(0);
        }
        Some(index_for(layout, target, self.content.len()))
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(offset) = self.vertical(-1.) {
            let goal = self.goal_x;
            self.move_to(offset, cx);
            self.goal_x = goal;
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(offset) = self.vertical(1.) {
            let goal = self.goal_x;
            self.move_to(offset, cx);
            self.goal_x = goal;
        }
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(offset) = self.vertical(-1.) {
            self.select_to(offset, cx);
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(offset) = self.vertical(1.) {
            self.select_to(offset, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = self.clean(&text);
            self.insert(&text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.insert("", window, cx);
        }
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline && self.enter == EnterKey::Breaks {
            self.insert("\n", window, cx);
        } else {
            cx.emit(TextFieldEvent::Submit);
        }
    }

    fn submit_always(&mut self, _: &SubmitAlways, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Submit);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            return cx.emit(TextFieldEvent::Submit);
        }
        if self.enter == EnterKey::Breaks {
            // Cmd/Alt-Enter in a description field submits instead.
            return cx.emit(TextFieldEvent::Submit);
        }
        self.insert("\n", window, cx);
    }

    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TextFieldEvent::Escape);
    }

    fn show_character_palette(&mut self, _: &ShowCharacterPalette, window: &mut Window, _: &mut Context<Self>) {
        window.show_character_palette();
    }

    fn index_for_mouse(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = self.layout.as_ref() else { return 0 };
        if self.content.is_empty() {
            return 0;
        }
        let local = point(position.x - layout.bounds.left(), position.y - layout.bounds.top() + self.scroll_top);
        if local.y < px(0.) {
            return 0;
        }
        index_for(layout, local, self.content.len())
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        window.focus(&self.focus_handle);
        self.goal_x = None;
        let index = self.index_for_mouse(event.position);
        if event.click_count >= 3 {
            let start = self.line_start(index);
            let end = self.line_end(index);
            self.selected_range = start..end;
            self.selection_reversed = false;
            return cx.notify();
        }
        if event.click_count == 2 {
            let start = self.previous_word(self.next_boundary(index).min(self.content.len()));
            let end = self.next_word(start);
            self.selected_range = start..end;
            self.selection_reversed = false;
            return cx.notify();
        }
        self.is_selecting = true;
        if event.modifiers.shift { self.select_to(index, cx) } else { self.move_to(index, cx) }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse(event.position), cx);
        }
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
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }
}

/// The paragraph holding `offset`, and the offset within it.
fn paragraph_of(layout: &Layout, offset: usize) -> (usize, usize) {
    let index = layout.starts.iter().rposition(|start| *start <= offset).unwrap_or(0);
    (index, offset - layout.starts[index])
}

fn paragraph_top(layout: &Layout, index: usize) -> Pixels {
    layout.lines[..index].iter().map(|line| line.size(layout.line_height).height).fold(px(0.), |a, b| a + b)
}

/// The cursor position for `offset`, relative to the text's top-left.
fn position_for(layout: &Layout, offset: usize) -> Option<Point<Pixels>> {
    let (index, local) = paragraph_of(layout, offset);
    let line = layout.lines.get(index)?;
    let p = line.position_for_index(local.min(line.len()), layout.line_height)?;
    Some(point(p.x, p.y + paragraph_top(layout, index)))
}

/// The offset nearest a point relative to the text's top-left.
fn index_for(layout: &Layout, position: Point<Pixels>, len: usize) -> usize {
    let mut top = px(0.);
    for (i, line) in layout.lines.iter().enumerate() {
        let height = line.size(layout.line_height).height;
        if position.y < top + height {
            let local = point(position.x.max(px(0.)), position.y - top);
            let within = match line.closest_index_for_position(local, layout.line_height) {
                Ok(index) | Err(index) => index,
            };
            return (layout.starts[i] + within).min(len);
        }
        top += height;
    }
    len
}

impl EntityInputHandler for TextField {
    fn text_for_range(&mut self, range_utf16: Range<usize>, actual: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore_disabled_input: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if self.disabled {
            return;
        }
        let new_text = self.clean(new_text);
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content = format!("{}{}{}", &self.content[..range.start], new_text, &self.content[range.end..]);
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.goal_x = None;
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content = format!("{}{}{}", &self.content[..range.start], new_text, &self.content[range.end..]);
        self.marked_range = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|r| r.start + range.start..r.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.emit(TextFieldEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = position_for(layout, range.start)?;
        let end = position_for(layout, range.end)?;
        let origin = point(bounds.left(), bounds.top() - self.scroll_top);
        Some(Bounds::from_corners(
            point(origin.x + start.x, origin.y + start.y),
            point(origin.x + end.x, origin.y + end.y + layout.line_height),
        ))
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let local = point(p.x - layout.bounds.left(), p.y - layout.bounds.top() + self.scroll_top);
        Some(self.offset_to_utf16(index_for(layout, local, self.content.len())))
    }
}

struct TextElement {
    field: Entity<TextField>,
}

struct Prepaint {
    lines: Vec<WrappedLine>,
    starts: Vec<usize>,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    line_height: Pixels,
    scroll_top: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn runs_for(text: &str, marked: Option<&Range<usize>>, color: Hsla, window: &Window) -> Vec<TextRun> {
    let style = window.text_style();
    let run = TextRun { len: text.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
    match marked {
        Some(marked) if marked.end <= text.len() => [
            TextRun { len: marked.start, ..run.clone() },
            TextRun {
                len: marked.end - marked.start,
                underline: Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false }),
                ..run.clone()
            },
            TextRun { len: text.len() - marked.end, ..run },
        ]
        .into_iter()
        .filter(|r| r.len > 0)
        .collect(),
        _ => vec![run],
    }
}

fn shape(field: &TextField, width: Option<Pixels>, window: &mut Window, cx: &App) -> (Vec<WrappedLine>, Vec<usize>) {
    let t = theme(cx);
    let (text, color, marked) = if field.content.is_empty() {
        (field.placeholder.clone(), t.muted_foreground, None)
    } else {
        (SharedString::from(field.content.clone()), window.text_style().color, field.marked_range.as_ref())
    };
    let runs = runs_for(&text, marked, color, window);
    let font_size = window.text_style().font_size.to_pixels(window.rem_size());
    let wrap = if field.multiline { width } else { None };
    let lines: Vec<WrappedLine> = window.text_system().shape_text(text.clone(), font_size, &runs, wrap, None).map(|l| l.into_vec()).unwrap_or_default();
    let mut starts = Vec::with_capacity(lines.len());
    let mut at = 0;
    for line in &lines {
        starts.push(at);
        at += line.len() + 1;
    }
    (lines, starts)
}

impl Element for TextElement {
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
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let field = self.field.clone();
        let line_height = window.line_height();
        let layout = window.request_measured_layout(style, move |known, available, window, cx| {
            let width = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let field = field.read(cx);
            let (min, max) = (field.min_lines, field.max_lines);
            let (lines, _) = shape(field, width, window, cx);
            let rows: usize = lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum::<usize>().clamp(min, max);
            size(width.unwrap_or(px(0.)), line_height * rows as f32)
        });
        (layout, ())
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
        let t = theme(cx);
        let line_height = window.line_height();
        let field = self.field.read(cx);
        let (lines, starts) = shape(field, Some(bounds.size.width), window, cx);
        let layout = Layout { lines, starts, line_height, bounds };
        let empty = field.content.is_empty();
        let range = field.selected_range.clone();
        let cursor_at = if empty { Some(point(px(0.), px(0.))) } else { position_for(&layout, field.cursor_offset()) };

        // Scroll so the cursor stays inside the visible rows.
        let mut scroll_top = field.scroll_top;
        let total = layout.lines.iter().map(|l| l.size(line_height).height).fold(px(0.), |a, b| a + b);
        if let Some(cursor) = cursor_at {
            if cursor.y < scroll_top {
                scroll_top = cursor.y;
            } else if cursor.y + line_height > scroll_top + bounds.size.height {
                scroll_top = cursor.y + line_height - bounds.size.height;
            }
        }
        scroll_top = scroll_top.min((total - bounds.size.height).max(px(0.))).max(px(0.));
        let origin = point(bounds.left(), bounds.top() - scroll_top);

        let mut selections = Vec::new();
        if !range.is_empty() && !empty {
            let mut at = range.start;
            while at < range.end {
                let (index, _) = paragraph_of(&layout, at);
                let paragraph_end = (layout.starts[index] + layout.lines[index].len()).min(range.end);
                let (Some(a), Some(b)) = (position_for(&layout, at), position_for(&layout, paragraph_end.max(at))) else { break };
                // Each visual row of the range gets its own quad.
                let mut row_y = a.y;
                while row_y <= b.y {
                    let start_x = if row_y == a.y { a.x } else { px(0.) };
                    let end_x = if row_y == b.y { b.x.max(start_x + px(4.)) } else { bounds.size.width };
                    selections.push(fill(
                        Bounds::from_corners(point(origin.x + start_x, origin.y + row_y), point(origin.x + end_x, origin.y + row_y + line_height)),
                        t.selection,
                    ));
                    row_y += line_height;
                }
                at = paragraph_end + 1;
            }
        }
        let cursor = (range.is_empty() && !field.disabled).then_some(cursor_at).flatten().map(|p| {
            fill(Bounds::new(point(origin.x + p.x, origin.y + p.y), size(px(1.5), line_height)), t.foreground)
        });
        let Layout { lines, starts, .. } = layout;
        Prepaint { lines, starts, cursor, selections, line_height, scroll_top }
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
        let focus_handle = self.field.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.field.clone()), cx);
        let line_height = prepaint.line_height;
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for selection in prepaint.selections.drain(..) {
                window.paint_quad(selection);
            }
            let mut y = bounds.top() - prepaint.scroll_top;
            for line in &prepaint.lines {
                let _ = line.paint(point(bounds.left(), y), line_height, TextAlign::Left, Some(bounds), window, cx);
                y += line.size(line_height).height;
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        let lines = std::mem::take(&mut prepaint.lines);
        let starts = std::mem::take(&mut prepaint.starts);
        let scroll_top = prepaint.scroll_top;
        self.field.update(cx, |field, _| {
            field.layout = Some(Layout { lines, starts, line_height, bounds });
            field.scroll_top = scroll_top;
        });
    }
}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(if self.disabled { CursorStyle::Arrow } else { CursorStyle::IBeam })
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_line_start))
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
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::submit_always))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::show_character_palette))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { field: cx.entity() })
    }
}
