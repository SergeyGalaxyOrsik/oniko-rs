//! One thread: its timeline, what it is waiting on, and the composer.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FontWeight, IntoElement, ScrollHandle, SharedString,
    Transformation, Window, div, percentage, prelude::*, px,
};
use oniko_core::acp;
use oniko_core::model::{
    AgentId, NoticeLevel, PermissionOutcome, Subscription, Thread, ThreadId, ThreadStatus, TimelineItem, ToolItem,
};

use crate::icons::{Icon, icon};
use crate::markdown::{self, Block};
use crate::state::{run, store};
use crate::theme::{MONO, theme};
use crate::ui::text_field::{EnterKey, TextField, TextFieldEvent};
use crate::ui::{ButtonSize, Variant, button, code, icon_button};
use crate::views::composer::{Composer, subscription_chip};
use crate::views::{Nav, basename, format_duration};

/// Parsed Markdown of an item, keyed by its text length so a streaming item is parsed again as it grows.
type Parsed = (usize, Rc<Vec<Block>>);

pub struct ThreadView {
    pub thread_id: ThreadId,
    composer: Entity<TextField>,
    open_menu: Option<SharedString>,
    /// Groups, tools and thoughts whose default open state the user flipped.
    toggled: HashSet<String>,
    scroll: ScrollHandle,
    /// Timeline length and last item size; a change while at the bottom keeps the view pinned there.
    signature: (usize, usize),
    markdown: RefCell<HashMap<String, Parsed>>,
}

impl EventEmitter<Nav> for ThreadView {}

impl ThreadView {
    pub fn new(thread_id: ThreadId, window: &mut Window, cx: &mut Context<Self>) -> ThreadView {
        let composer = cx.new(|cx| TextField::new(window, cx).multiline(2, 10, EnterKey::Submits).placeholder("Ask for changes, send follow-ups…"));
        cx.subscribe_in(&composer, window, |this, _, event, window, cx| match event {
            TextFieldEvent::Submit => this.send(window, cx),
            TextFieldEvent::Escape => this.stop(cx),
            TextFieldEvent::Changed => cx.notify(),
            _ => {}
        })
        .detach();
        cx.observe(&store(cx), |_, _, cx| cx.notify()).detach();
        let scroll = ScrollHandle::new();
        scroll.scroll_to_bottom();
        ThreadView { thread_id, composer, open_menu: None, toggled: HashSet::new(), scroll, signature: (0, 0), markdown: RefCell::default() }
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        self.composer.read(cx).focus(window);
    }

    fn thread<'a>(&self, cx: &'a App) -> Option<&'a Thread> {
        store(cx).read(cx).state.threads.get(&self.thread_id)
    }

    fn busy(&self, cx: &App) -> bool {
        self.thread(cx).is_some_and(|t| t.status.is_busy())
    }

    fn send(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().trim().to_owned();
        if text.is_empty() || self.busy(cx) {
            return;
        }
        run(cx, |c| c.prompt(self.thread_id.clone(), text));
        self.composer.update(cx, |field, cx| field.clear(cx));
        self.scroll.scroll_to_bottom();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.busy(cx) {
            run(cx, |c| c.cancel(self.thread_id.clone()));
        }
    }

    fn toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.toggled.remove(key) {
            self.toggled.insert(key.to_owned());
        }
        cx.notify();
    }

    fn is_open(&self, key: &str, default: bool) -> bool {
        default != self.toggled.contains(key)
    }

    fn markdown_for(&self, id: &str, text: &str) -> Rc<Vec<Block>> {
        let mut cache = self.markdown.borrow_mut();
        if let Some((len, blocks)) = cache.get(id)
            && *len == text.len()
        {
            return blocks.clone();
        }
        let blocks = Rc::new(markdown::parse(text));
        cache.insert(id.to_owned(), (text.len(), blocks.clone()));
        blocks
    }
}

enum Row<'a> {
    Item(&'a TimelineItem),
    Activity { id: String, tools: Vec<&'a ToolItem>, live: bool },
}

/// Folds each run of consecutive tool calls into one activity group; the last group of a running turn is live.
fn rows(timeline: &[TimelineItem], running: bool) -> Vec<Row<'_>> {
    let mut rows: Vec<Row> = Vec::new();
    for item in timeline {
        match item {
            TimelineItem::Tool(tool) => match rows.last_mut() {
                Some(Row::Activity { tools, .. }) => tools.push(tool),
                _ => rows.push(Row::Activity { id: format!("activity-{}", tool.id), tools: vec![tool], live: false }),
            },
            other => rows.push(Row::Item(other)),
        }
    }
    if running && let Some(Row::Activity { live, .. }) = rows.last_mut() {
        *live = true;
    }
    rows
}

fn spinner(color: gpui::Hsla) -> AnyElement {
    icon(Icon::Loader, color)
        .size(px(14.))
        .with_animation("spin", Animation::new(Duration::from_millis(900)).repeat(), |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))))
        .into_any_element()
}

fn status_mark(status: acp::ToolCallStatus, cx: &App) -> AnyElement {
    let t = theme(cx);
    match status {
        acp::ToolCallStatus::Completed => icon(Icon::Check, t.emerald).size(px(14.)).into_any_element(),
        acp::ToolCallStatus::Failed => icon(Icon::CircleAlert, t.red).size(px(14.)).into_any_element(),
        _ => spinner(t.muted_foreground),
    }
}

fn diff_view(diff: &acp::Diff, cx: &App) -> AnyElement {
    let t = theme(cx);
    let removed = diff.old_text.as_deref().map(|old| old.split('\n').collect::<Vec<_>>()).unwrap_or_default();
    let added: Vec<&str> = diff.new_text.split('\n').collect();
    let line = |sign: &'static str, text: &str, bg: gpui::Hsla, fg: gpui::Hsla| {
        div().flex().gap_2().px_2().bg(bg).child(div().flex_none().w(px(10.)).text_color(fg).child(sign)).child(div().flex_1().min_w_0().child(text.to_owned()))
    };
    div()
        .flex()
        .flex_col()
        .rounded_md()
        .border_1()
        .border_color(t.border)
        .overflow_hidden()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .bg(t.muted)
                .text_xs()
                .child(icon(Icon::FileText, t.muted_foreground).size(px(12.)))
                .child(basename(&diff.path.to_string_lossy()))
                .child(div().text_color(t.emerald).child(format!("+{}", added.len())))
                .when(!removed.is_empty(), |d| d.child(div().text_color(t.red).child(format!("-{}", removed.len())))),
        )
        .child(
            div()
                .id("diff-lines")
                .max_h(px(360.))
                .overflow_y_scroll()
                .font_family(MONO)
                .text_xs()
                .line_height(px(18.))
                .children(removed.iter().map(|l| line("-", l, t.diff_removed, t.red)))
                .children(added.iter().map(|l| line("+", l, t.diff_added, t.emerald))),
        )
        .into_any_element()
}

fn tool_content(content: &acp::ToolCallContent, cx: &App) -> AnyElement {
    let t = theme(cx);
    match content {
        acp::ToolCallContent::Diff(diff) => diff_view(diff, cx),
        acp::ToolCallContent::Content(content) => match &content.content {
            // Adapters often wrap output in a fenced block; render that as Markdown.
            acp::ContentBlock::Text(text) if text.text.trim_start().starts_with("```") => {
                div().text_sm().child(markdown::render(&markdown::parse(&text.text), cx)).into_any_element()
            }
            acp::ContentBlock::Text(text) => {
                // Long output keeps its end, which is usually what matters.
                let lines: Vec<&str> = text.text.lines().collect();
                let shown = if lines.len() > 60 { format!("… {} lines above\n{}", lines.len() - 60, lines[lines.len() - 60..].join("\n")) } else { text.text.clone() };
                div()
                    .id("tool-output")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded_md()
                    .bg(t.code_background)
                    .font_family(MONO)
                    .text_xs()
                    .line_height(px(18.))
                    .text_color(t.muted_foreground)
                    .child(shown)
                    .into_any_element()
            }
            _ => div().text_xs().text_color(t.muted_foreground).child("Non-text content").into_any_element(),
        },
        acp::ToolCallContent::Terminal(terminal) => {
            div().text_xs().text_color(t.muted_foreground).child(format!("Terminal {}", terminal.terminal_id.0)).into_any_element()
        }
        _ => div().into_any_element(),
    }
}

fn subscription_of<'a>(thread: &Thread, cx: &'a App) -> Option<&'a Subscription> {
    store(cx).read(cx).state.subscriptions.get(&thread.subscription_id)
}

/// The exact login command for a config dir, with copy and a re-check once the user ran it.
pub fn login_command(subscription: &Subscription, cx: &App) -> AnyElement {
    let t = theme(cx);
    let command = subscription.login_command.clone();
    let id = subscription.id.clone();
    let checking = subscription.account == oniko_core::model::AccountStatus::Checking;
    div()
        .flex()
        .items_center()
        .gap_1()
        .min_w_0()
        .child(div().flex_1().min_w_0().child(code(command.clone(), cx)))
        .child(
            icon_button("copy-login", Icon::Copy)
                .muted()
                .tooltip("Copy login command")
                .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(command.clone()))),
        )
        .child(
            button("refresh-login", "Refresh")
                .icon(Icon::RefreshCw)
                .size(ButtonSize::Xs)
                .muted()
                .disabled(checking)
                .on_click(move |_, _, cx| run(cx, |c| c.refresh_subscriptions(Some(id.clone())))),
        )
        .text_color(t.foreground)
        .into_any_element()
}

pub fn login_hint(subscription: &Subscription, cx: &App) -> AnyElement {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_xl()
        .border_1()
        .border_color(t.amber.opacity(0.4))
        .bg(t.amber.opacity(0.08))
        .text_sm()
        .child(
            div()
                .flex()
                .gap_1()
                .child(div().font_weight(FontWeight::MEDIUM).child(subscription.label.clone()))
                .child("is logged out. Run this in a terminal, then refresh."),
        )
        .child(login_command(subscription, cx))
        .into_any_element()
}

impl ThreadView {
    fn status_banner(&self, thread: &Thread, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx);
        let label = thread.agent_id.label();
        if let Some(sub) = subscription_of(thread, cx)
            && matches!(sub.account, oniko_core::model::AccountStatus::LoggedOut { .. })
        {
            return Some(login_hint(sub, cx));
        }
        match &thread.status {
            ThreadStatus::AuthRequired { methods } => {
                let buttons = methods.iter().enumerate().map(|(i, method)| {
                    let (method_id, name) = match method {
                        acp::AuthMethod::Agent(m) => (m.id.0.to_string(), m.name.clone()),
                        acp::AuthMethod::Terminal(m) => (m.id.0.to_string(), m.name.clone()),
                        _ => (String::new(), "Sign in".to_owned()),
                    };
                    let id = thread.id.clone();
                    button(("auth-method", i), name)
                        .variant(Variant::Secondary)
                        .on_click(move |_, _, cx| run(cx, |c| c.authenticate(id.clone(), method_id.clone())))
                });
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_3()
                        .rounded_xl()
                        .border_1()
                        .border_color(t.border)
                        .bg(t.muted)
                        .text_sm()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .font_weight(FontWeight::MEDIUM)
                                .child(icon(Icon::KeyRound, t.foreground))
                                .child(format!("{label} needs you to sign in")),
                        )
                        .child(
                            div()
                                .text_color(t.muted_foreground)
                                .child(format!("Pick a sign-in method below, or log in with the {label} CLI in a terminal and send your message again.")),
                        )
                        .child(div().flex().flex_wrap().gap_2().children(buttons))
                        .into_any_element(),
                )
            }
            ThreadStatus::Error { message } => {
                Some(div().text_sm().text_color(t.destructive).child(format!("{label} failed to start: {message}")).into_any_element())
            }
            ThreadStatus::Exited { code } => Some(
                div()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child(format!(
                        "{label} stopped (exit code {}). Send a message to reconnect.",
                        code.map_or("none".to_owned(), |c| c.to_string())
                    ))
                    .into_any_element(),
            ),
            _ => None,
        }
    }

    fn tool_row(&self, tool: &ToolItem, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let key = format!("tool-{}", tool.id);
        let open = self.is_open(&key, false);
        let meta = tool.locations.first().map(|l| match l.line {
            Some(line) => format!("{}:{line}", basename(&l.path)),
            None => basename(&l.path),
        });
        let toggle_key = key.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id(SharedString::from(key.clone()))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(28.))
                    .px_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.accent))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(&toggle_key, cx)))
                    .child(icon(Icon::for_tool(&tool.tool_kind), t.muted_foreground).size(px(14.)))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(tool.title.clone()))
                    .when_some(meta, |d, meta| d.child(div().flex_none().text_xs().text_color(t.muted_foreground).child(meta)))
                    .child(status_mark(tool.status, cx)),
            )
            .when(open, |d| {
                d.child(
                    div()
                        .pl_6()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(tool.content.is_empty(), |d| d.child(div().text_xs().text_color(t.muted_foreground).child("No output")))
                        .children(tool.content.iter().map(|c| tool_content(c, cx))),
                )
            })
            .into_any_element()
    }

    fn activity(&self, id: &str, tools: &[&ToolItem], live: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let open = self.is_open(id, live);
        let count = tools.len();
        let label = if live {
            tools.last().map_or("Working".to_owned(), |tool| tool.title.clone())
        } else {
            format!("Ran {count} {}", if count == 1 { "tool" } else { "tools" })
        };
        let key = id.to_owned();
        let body: Vec<AnyElement> = if open { tools.iter().map(|tool| self.tool_row(tool, cx)).collect() } else { vec![] };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id(SharedString::from(id.to_owned()))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.foreground))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(&key, cx)))
                    .when(live, |d| d.child(spinner(t.muted_foreground)))
                    .when(!live, |d| d.child(icon(Icon::Wrench, t.muted_foreground).size(px(14.))))
                    .child(div().truncate().child(label))
                    .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.muted_foreground).size(px(14.))),
            )
            .when(open, |d| d.child(div().ml(px(6.)).pl_3().border_l_1().border_color(t.border).flex().flex_col().gap_0p5().children(body)))
            .into_any_element()
    }

    fn permission(&self, thread: &Thread, item: &TimelineItem, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let TimelineItem::Permission { id, title, options, outcome, .. } = item else { return div().into_any_element() };
        let state = match outcome {
            None => None,
            Some(PermissionOutcome::Cancelled) => Some(false),
            Some(PermissionOutcome::Selected { option_id }) => {
                Some(options.iter().find(|o| o.option_id.0.as_ref() == option_id).is_some_and(|o| is_allow(&o.kind)))
            }
        };
        let buttons = options.iter().enumerate().map(|(i, option)| {
            let allow = is_allow(&option.kind);
            let variant = if i == 0 && allow { Variant::Primary } else if allow { Variant::Outline } else { Variant::Ghost };
            let (thread_id, request_id, option_id) = (thread.id.clone(), id.clone(), option.option_id.0.to_string());
            button(SharedString::from(format!("perm-{id}-{i}")), option.name.clone())
                .variant(variant)
                .size(ButtonSize::Xs)
                .on_click(move |_, _, cx| run(cx, |c| c.respond_permission(thread_id.clone(), request_id.clone(), Some(option_id.clone()))))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_xl()
            .border_1()
            .border_color(if state.is_none() { t.amber.opacity(0.5) } else { t.border })
            .bg(t.background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(icon(Icon::KeyRound, if state.is_none() { t.amber } else { t.muted_foreground }).size(px(14.)))
                    .child(div().font_weight(FontWeight::MEDIUM).child(match state {
                        None => "Allow this action?",
                        Some(true) => "Approved",
                        Some(false) => "Denied",
                    })),
            )
            .child(div().text_sm().text_color(t.muted_foreground).font_family(MONO).child(title.clone()))
            .when(state.is_none(), |d| d.child(div().flex().flex_wrap().gap_2().children(buttons)))
            .into_any_element()
    }

    fn item(&self, thread: &Thread, item: &TimelineItem, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        match item {
            TimelineItem::User { text, .. } => div()
                .flex()
                .justify_end()
                .child(div().max_w(px(560.)).px_4().py_2p5().rounded_2xl().bg(t.muted).text_sm().line_height(px(21.)).child(text.clone()))
                .into_any_element(),
            TimelineItem::Assistant { id, text } => {
                let blocks = self.markdown_for(id, text);
                div().text_sm().line_height(px(22.)).child(markdown::render(&blocks, cx)).into_any_element()
            }
            TimelineItem::Thought { id, text } => {
                let key = format!("thought-{id}");
                let open = self.is_open(&key, false);
                let toggle = key.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .id(SharedString::from(key))
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_sm()
                            .text_color(t.muted_foreground)
                            .cursor_pointer()
                            .hover(|s| s.text_color(t.foreground))
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle(&toggle, cx)))
                            .child(icon(Icon::Brain, t.muted_foreground).size(px(14.)))
                            .child("Thought")
                            .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.muted_foreground).size(px(14.))),
                    )
                    .when(open, |d| d.child(div().pl_5().text_sm().text_color(t.muted_foreground).line_height(px(21.)).child(text.clone())))
                    .into_any_element()
            }
            TimelineItem::Plan { entries, .. } => div()
                .flex()
                .flex_col()
                .gap_1p5()
                .p_3()
                .rounded_xl()
                .border_1()
                .border_color(t.border)
                .child(div().flex().items_center().gap_2().text_sm().font_weight(FontWeight::MEDIUM).child(icon(Icon::ListTodo, t.foreground)).child("Plan"))
                .children(entries.iter().map(|entry| {
                    let (mark, done) = match entry.status {
                        acp::PlanEntryStatus::Completed => (icon(Icon::Check, t.emerald).size(px(14.)).into_any_element(), true),
                        acp::PlanEntryStatus::InProgress => (spinner(t.sky), false),
                        _ => (icon(Icon::Circle, t.muted_foreground).size(px(14.)).into_any_element(), false),
                    };
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_sm()
                        .child(div().pt(px(3.)).child(mark))
                        .child(div().flex_1().when(done, |d| d.line_through().text_color(t.muted_foreground)).child(entry.content.clone()))
                }))
                .into_any_element(),
            TimelineItem::Permission { .. } => self.permission(thread, item, cx),
            TimelineItem::TurnEnd { stop_reason, duration_ms, .. } => div()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(if stop_reason == "end_turn" {
                    format!("Worked for {}", format_duration(*duration_ms))
                } else {
                    format!("Stopped ({stop_reason}) after {}", format_duration(*duration_ms))
                })
                .into_any_element(),
            TimelineItem::Notice { level, text, .. } => {
                let color = if *level == NoticeLevel::Error { t.destructive } else { t.muted_foreground };
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_sm()
                    .text_color(color)
                    .child(div().pt(px(2.)).child(icon(if *level == NoticeLevel::Error { Icon::CircleAlert } else { Icon::Info }, color).size(px(14.))))
                    .child(div().flex_1().child(text.clone()))
                    .into_any_element()
            }
            TimelineItem::Tool(tool) => self.tool_row(tool, cx),
        }
    }
}

fn is_allow(kind: &acp::PermissionOptionKind) -> bool {
    matches!(kind, acp::PermissionOptionKind::AllowOnce | acp::PermissionOptionKind::AllowAlways)
}

fn waiting_label(thread: &Thread) -> Option<String> {
    let last = thread.timeline.last();
    match thread.status {
        ThreadStatus::Connecting => Some(format!("Connecting to {}…", AgentId::label(thread.agent_id))),
        ThreadStatus::Running { .. } if matches!(last, None | Some(TimelineItem::User { .. })) => Some("Thinking…".into()),
        _ => None,
    }
}

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let Some(thread) = self.thread(cx).cloned() else {
            return div().size_full().into_any_element();
        };
        // Stay pinned to the bottom while new output arrives, unless the user scrolled up.
        let last_size = thread.timeline.last().map_or(0, |item| match item {
            TimelineItem::Assistant { text, .. } | TimelineItem::Thought { text, .. } => text.len(),
            TimelineItem::Tool(tool) => tool.content.len() + 1,
            _ => 1,
        });
        let signature = (thread.timeline.len(), last_size);
        if signature != self.signature {
            let offset = self.scroll.offset();
            let max = self.scroll.max_offset();
            if max.height - (-offset.y) < px(64.) || self.signature == (0, 0) {
                self.scroll.scroll_to_bottom();
            }
            self.signature = signature;
        }
        let running = matches!(thread.status, ThreadStatus::Running { .. } | ThreadStatus::AwaitingPermission { .. });
        let rows: Vec<AnyElement> = rows(&thread.timeline, running)
            .into_iter()
            .map(|row| match row {
                Row::Item(item) => self.item(&thread, item, cx),
                Row::Activity { id, tools, live } => self.activity(&id, &tools, live, cx),
            })
            .collect();
        let waiting = waiting_label(&thread).map(|label| {
            div()
                .text_sm()
                .text_color(t.muted_foreground)
                .child(label)
                .with_animation("waiting", Animation::new(Duration::from_millis(1400)).repeat().with_easing(gpui::pulsating_between(0.35, 1.)), |d, delta| d.opacity(delta))
        });
        let banner = self.status_banner(&thread, cx);
        let busy = thread.status.is_busy();
        let subscriptions = store(cx).read(cx).state.subscriptions.subscriptions.clone();
        let current = subscription_of(&thread, cx).cloned();
        let leading = subscription_chip(current.as_ref(), &subscriptions, true, false, |_, _, _| {}, |_, _, _| {}, cx);
        let entity = cx.entity();
        let set_menu = {
            let entity = entity.clone();
            Rc::new(move |menu: Option<SharedString>, _: &mut Window, cx: &mut App| entity.update(cx, |this, cx| {
                this.open_menu = menu;
                cx.notify();
            }))
        };
        let thread_id = thread.id.clone();
        let on_config = Rc::new(move |(config_id, value): (String, String), _: &mut Window, cx: &mut App| {
            run(cx, |c| c.set_config(thread_id.clone(), config_id, value))
        });
        let on_send = {
            let entity = entity.clone();
            Rc::new(move |_: (), window: &mut Window, cx: &mut App| entity.update(cx, |this, cx| this.send(window, cx)))
        };
        let on_stop = Rc::new(move |_: (), _: &mut Window, cx: &mut App| entity.update(cx, |this, cx| this.stop(cx)));
        let _ = window;
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(
                div().id("timeline").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).child(
                    div()
                        .w_full()
                        .max_w(px(768.))
                        .mx_auto()
                        .px_6()
                        .pt_6()
                        .pb_10()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .children(rows)
                        .children(waiting),
                ),
            )
            .child(
                div()
                    .w_full()
                    .max_w(px(768.))
                    .mx_auto()
                    .px_6()
                    .pb_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(banner)
                    .child(Composer {
                        field: self.composer.clone(),
                        config: thread.config.clone(),
                        busy,
                        disabled: false,
                        leading,
                        open_menu: self.open_menu.clone(),
                        set_menu,
                        on_config,
                        on_send,
                        on_stop,
                    }),
            )
            .into_any_element()
    }
}
