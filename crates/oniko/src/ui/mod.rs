//! Small building blocks drawn with plain gpui. They stand in for the component library being built
//! separately; each keeps its call site simple so swapping it later touches little.

pub mod text_field;

use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, ClickEvent, Context, Corner, ElementId, Hsla, IntoElement, MouseButton, ParentElement, Render, SharedString,
    Styled, Window, anchored, deferred, div, prelude::*, px,
};

use crate::icons::{Icon, icon};
use crate::theme::{MONO, theme};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type OpenSetter = Rc<dyn Fn(bool, &mut Window, &mut App)>;
type SelectHandler = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Outline,
    Ghost,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    Xs,
    Sm,
    Icon,
}

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<Icon>,
    trailing: Option<Icon>,
    variant: Variant,
    size: ButtonSize,
    disabled: bool,
    selected: bool,
    full_width: bool,
    muted: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button {
        id: id.into(),
        label: Some(label.into()),
        icon: None,
        trailing: None,
        variant: Variant::Ghost,
        size: ButtonSize::Sm,
        disabled: false,
        selected: false,
        full_width: false,
        muted: false,
        tooltip: None,
        on_click: None,
    }
}

pub fn icon_button(id: impl Into<ElementId>, glyph: Icon) -> Button {
    Button { label: None, icon: Some(glyph), size: ButtonSize::Icon, ..button(id, "") }
}

impl Button {
    pub fn icon(mut self, glyph: Icon) -> Self {
        self.icon = Some(glyph);
        self
    }
    pub fn trailing(mut self, glyph: Icon) -> Self {
        self.trailing = Some(glyph);
        self
    }
    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    /// Ghost text in the muted color until hovered.
    pub fn muted(mut self) -> Self {
        self.muted = true;
        self
    }
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }
    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let (bg, fg, border, hover): (Option<Hsla>, Hsla, Option<Hsla>, Hsla) = match self.variant {
            Variant::Primary => (Some(t.primary), t.primary_foreground, None, t.primary.opacity(0.88)),
            Variant::Secondary => (Some(t.muted), t.foreground, None, t.accent),
            Variant::Outline => (Some(t.background), t.foreground, Some(t.border), t.muted),
            Variant::Ghost => (None, if self.muted { t.muted_foreground } else { t.foreground }, None, t.accent),
            Variant::Destructive => (None, t.destructive, None, t.destructive.opacity(0.1)),
        };
        let (h, px_x, text) = match self.size {
            ButtonSize::Xs => (px(24.), px(8.), px(12.)),
            ButtonSize::Sm => (px(30.), px(10.), px(13.)),
            ButtonSize::Icon => (px(26.), px(0.), px(13.)),
        };
        let selected_bg = self.selected.then_some(t.accent);
        let disabled = self.disabled;
        let on_click = self.on_click.clone();
        let tooltip = self.tooltip.clone();
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap_1p5()
            .h(h)
            .when(self.size == ButtonSize::Icon, |d| d.w(h))
            .px(px_x)
            .rounded_md()
            .text_size(text)
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(fg)
            .when_some(selected_bg.or(bg), |d, bg| d.bg(bg))
            .when_some(border, |d, b| d.border_1().border_color(b))
            .when(self.full_width, |d| d.w_full().justify_start())
            .when(disabled, |d| d.opacity(0.45).cursor_default())
            .when(!disabled, |d| d.cursor_pointer().hover(move |s| s.bg(hover).text_color(if matches!(self.variant, Variant::Ghost) { t.foreground } else { fg })))
            .when_some(self.icon, |d, glyph| d.child(icon(glyph, fg).size(px(15.))))
            .when_some(self.label.filter(|l| !l.is_empty()), |d, label| d.child(div().truncate().child(label)))
            .when_some(self.trailing, |d, glyph| d.child(icon(glyph, t.muted_foreground).size(px(13.))))
            .when_some(tooltip, |d, text| d.tooltip(move |_, cx| tooltip_view(text.clone(), cx)))
            .when_some(on_click.filter(|_| !disabled), |d, handler| {
                d.on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        handler(event, window, cx)
                    })
            })
    }
}

/// A hover tooltip with one line of text.
pub struct Tooltip {
    text: SharedString,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .max_w(px(320.))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(t.primary)
            .text_color(t.primary_foreground)
            .text_xs()
            .child(self.text.clone())
    }
}

pub fn tooltip_view(text: SharedString, cx: &mut App) -> AnyView {
    cx.new(|_| Tooltip { text }).into()
}

#[derive(IntoElement)]
pub struct Badge {
    label: SharedString,
    outline: bool,
    dot: Option<Hsla>,
}

pub fn badge(label: impl Into<SharedString>) -> Badge {
    Badge { label: label.into(), outline: true, dot: None }
}

impl Badge {
    pub fn filled(mut self) -> Self {
        self.outline = false;
        self
    }
    pub fn dot(mut self, color: Hsla) -> Self {
        self.dot = Some(color);
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let color = t.muted_foreground;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .h(px(20.))
            .px_2()
            .rounded_md()
            .text_xs()
            .text_color(color)
            .when(self.outline, |d| d.border_1().border_color(t.border))
            .when(!self.outline, |d| d.bg(t.muted))
            .when_some(self.dot, |d, dot| d.child(status_dot(dot)))
            .child(self.label)
    }
}

pub fn status_dot(color: Hsla) -> impl IntoElement {
    div().size(px(7.)).flex_none().rounded_full().bg(color)
}

pub fn code(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    div().px_1p5().py_0p5().rounded_sm().bg(t.code_background).font_family(MONO).text_xs().truncate().child(text.into())
}

/// An on/off pill.
pub fn switch(id: impl Into<ElementId>, on: bool, disabled: bool, cx: &App, toggle: impl Fn(bool, &mut Window, &mut App) + 'static) -> impl IntoElement {
    let t = theme(cx);
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .w(px(34.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .bg(if on { t.primary } else { t.input })
        .when(on, |d| d.justify_end())
        .when(disabled, |d| d.opacity(0.45))
        .when(!disabled, |d| d.cursor_pointer().on_click(move |_, window, cx| toggle(!on, window, cx)))
        .child(div().size(px(16.)).rounded_full().bg(t.background).shadow_sm())
}

/// Whether the menu named `key` is the open one.
pub fn is_open(menu: &Option<SharedString>, key: &str) -> bool {
    menu.as_ref().is_some_and(|m| m.as_ref() == key)
}

/// One row of a dropdown menu.
pub enum MenuEntry {
    Item(MenuItem),
    Label(SharedString),
    Separator,
}

pub struct MenuItem {
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub icon: Option<Icon>,
    pub dot: Option<Hsla>,
    pub checked: bool,
    pub disabled: bool,
    pub destructive: bool,
    pub on_select: SelectHandler,
}

pub fn item(label: impl Into<SharedString>, on_select: impl Fn(&mut Window, &mut App) + 'static) -> MenuItem {
    MenuItem {
        label: label.into(),
        detail: None,
        icon: None,
        dot: None,
        checked: false,
        disabled: false,
        destructive: false,
        on_select: Rc::new(on_select),
    }
}

impl MenuItem {
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }
    pub fn icon(mut self, glyph: Icon) -> Self {
        self.icon = Some(glyph);
        self
    }
    pub fn dot(mut self, color: Hsla) -> Self {
        self.dot = Some(color);
        self
    }
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
    pub fn entry(self) -> MenuEntry {
        MenuEntry::Item(self)
    }
}

/// A trigger and, while `open`, its menu below it. The owner keeps the open state: `set_open` is
/// called on a trigger click, after a pick, and on a click anywhere else.
pub fn dropdown(
    id: impl Into<ElementId>,
    open: bool,
    corner: Corner,
    trigger: impl IntoElement,
    entries: impl FnOnce() -> Vec<MenuEntry>,
    set_open: impl Fn(bool, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let set_open: OpenSetter = Rc::new(set_open);
    let toggle = set_open.clone();
    let id = id.into();
    let mut root = div().relative().flex_none().child(
        div().id(id.clone()).cursor_pointer().on_click(move |_, window, cx| {
            cx.stop_propagation();
            toggle(!open, window, cx)
        }).child(trigger),
    );
    if open {
        let t = theme(cx);
        let close = set_open.clone();
        let rows = entries().into_iter().enumerate().map(|(i, entry)| match entry {
            MenuEntry::Separator => div().my_1().h(px(1.)).bg(t.border).into_any_element(),
            MenuEntry::Label(text) => div().px_2().pt_1p5().pb_1().text_xs().text_color(t.muted_foreground).child(text).into_any_element(),
            MenuEntry::Item(item) => {
                let pick = item.on_select.clone();
                let close = set_open.clone();
                let fg = if item.destructive { t.destructive } else { t.foreground };
                div()
                    .id(("menu-item", i))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded_sm()
                    .text_sm()
                    .text_color(fg)
                    .when(item.disabled, |d| d.opacity(0.45))
                    .when(!item.disabled, |d| {
                        d.cursor_pointer().hover(|s| s.bg(t.accent)).on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            close(false, window, cx);
                            pick(window, cx);
                        })
                    })
                    .when_some(item.dot, |d, c| d.child(status_dot(c)))
                    .when_some(item.icon, |d, glyph| d.child(icon(glyph, if item.destructive { t.destructive } else { t.muted_foreground })))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(item.label))
                            .when_some(item.detail, |d, detail| d.child(div().truncate().text_xs().text_color(t.muted_foreground).child(detail))),
                    )
                    .child(div().w(px(16.)).flex_none().when(item.checked, |d| d.child(icon(Icon::Check, t.foreground))))
                    .into_any_element()
            }
        });
        let panel = div()
            .id(id.clone())
            .occlude()
            .min_w(px(200.))
            .max_w(px(380.))
            .max_h(px(420.))
            .overflow_y_scroll()
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(t.border)
            .bg(t.background)
            .shadow_lg()
            .text_color(t.foreground)
            .on_mouse_down_out(move |_, window, cx| close(false, window, cx))
            .children(rows);
        // A zero-size box on the trigger's edge carries the anchor, so the menu opens below the trigger
        // (or above it, for a Bottom corner) and lines up with its left or right side.
        let up = matches!(corner, Corner::BottomLeft | Corner::BottomRight);
        let right = matches!(corner, Corner::TopRight | Corner::BottomRight);
        let panel = if up { panel.mb_1() } else { panel.mt_1() };
        let edge = div()
            .absolute()
            .size_0()
            .when(up, |d| d.top_0())
            .when(!up, |d| d.bottom_0())
            .when(right, |d| d.right_0())
            .when(!right, |d| d.left_0())
            .child(deferred(anchored().anchor(corner).snap_to_window_with_margin(px(8.)).child(panel)).with_priority(1));
        root = root.child(edge);
    }
    root.into_any_element()
}

/// A dimmed backdrop with a centered panel; a click on the backdrop calls `on_dismiss`.
pub fn modal(id: impl Into<ElementId>, width: f32, body: impl IntoElement, on_dismiss: impl Fn(&mut Window, &mut App) + 'static, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    let dismiss = Rc::new(on_dismiss);
    deferred(
        div()
            .id(id)
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(120.))
            .bg(gpui::black().opacity(if t.dark { 0.6 } else { 0.35 }))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| dismiss(window, cx))
            .child(
                div()
                    .id("modal-panel")
                    .w(px(width))
                    .max_w_full()
                    .rounded_xl()
                    .border_1()
                    .border_color(t.border)
                    .bg(t.background)
                    .text_color(t.foreground)
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(body),
            ),
    )
    .with_priority(2)
}

/// A bordered box around a text field.
pub fn field_frame(focused: bool, invalid: bool, cx: &App) -> gpui::Div {
    let t = theme(cx);
    div()
        .w_full()
        .px_2p5()
        .py_1p5()
        .rounded_md()
        .border_1()
        .border_color(if invalid { t.destructive } else if focused { t.ring } else { t.input })
        .bg(t.background)
        .text_sm()
}
