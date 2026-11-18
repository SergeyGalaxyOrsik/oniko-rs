//! The window: sidebar, the inset card with its header, and whichever screen the card shows.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{
    AnyElement, App, AsyncWindowContext, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, KeyBinding, SharedString, WeakEntity,
    Window, actions, div, prelude::*, px,
};
use oniko_core::model::{TaskId, ThreadId};
use oniko_core::util::now_ms;

use crate::icons::{Icon, icon};
use crate::state::{StoreEvent, store};
use crate::theme::{SANS, sync_theme, theme};
use crate::ui::{ButtonSize, badge, button, icon_button, tooltip_view};
use crate::views::dialogs::{AddSubscriptionDialog, DialogEvent, NewTaskDialog};
use crate::views::new_thread::NewThreadView;
use crate::views::settings::SettingsView;
use crate::views::tasks::TasksView;
use crate::views::thread::ThreadView;
use crate::views::{LOCKED_HINT, Nav, basename, sidebar};

actions!(oniko, [NewThread, OpenSettings, ToggleSidebar, GoBack]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-n", NewThread, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("escape", GoBack, Some("Workspace")),
    ]);
}

/// Where the inset card points. Settings remembers what it covered so Esc and Back return there.
#[derive(Debug, Clone, PartialEq)]
pub enum MainView {
    New,
    Thread(ThreadId),
    Tasks { project: String, task: Option<TaskId> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Main(MainView),
    Settings { back: MainView },
}

pub enum Dialog {
    AddSubscription(Entity<AddSubscriptionDialog>),
    NewTask(Entity<NewTaskDialog>),
}

pub struct Workspace {
    pub view: View,
    pub new_thread: Entity<NewThreadView>,
    threads: HashMap<ThreadId, Entity<ThreadView>>,
    tasks: HashMap<String, Entity<TasksView>>,
    settings: Option<Entity<SettingsView>>,
    pub dialog: Option<Dialog>,
    pub collapsed: HashSet<String>,
    pub sidebar_open: bool,
    pub switcher_open: bool,
    /// For relative times, refreshed every 30 seconds.
    pub now: i64,
    focus: FocusHandle,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Workspace {
        let new_thread = cx.new(|cx| NewThreadView::new(window, cx));
        cx.subscribe_in(&new_thread, window, |this, _, nav, window, cx| this.navigate(nav.clone(), window, cx)).detach();
        let store = store(cx);
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&store, window, |this, _, event, window, cx| match event {
            StoreEvent::OpenThread(id) => {
                cx.activate(true);
                window.activate_window();
                this.navigate(Nav::OpenThread(id.clone()), window, cx);
            }
        })
        .detach();
        cx.observe_window_appearance(window, |_, window, cx| {
            sync_theme(window, cx);
            cx.notify();
        })
        .detach();
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx: &mut AsyncWindowContext| {
            loop {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                if this.update(cx, |this, cx| {
                    this.now = now_ms();
                    cx.notify();
                })
                .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let focus = cx.focus_handle();
        new_thread.read(cx).focus(window, cx);
        Workspace {
            view: View::Main(MainView::New),
            new_thread,
            threads: HashMap::new(),
            tasks: HashMap::new(),
            settings: None,
            dialog: None,
            collapsed: HashSet::new(),
            sidebar_open: true,
            switcher_open: false,
            now: now_ms(),
            focus,
        }
    }

    pub fn main(&self) -> &MainView {
        match &self.view {
            View::Main(main) | View::Settings { back: main } => main,
        }
    }

    pub fn navigate(&mut self, nav: Nav, window: &mut Window, cx: &mut Context<Self>) {
        match nav {
            Nav::OpenThread(id) => {
                let view = self.thread_view(&id, window, cx);
                self.view = View::Main(MainView::Thread(id));
                view.read(cx).focus(window, cx);
            }
            Nav::OpenTasks { project, task } => {
                self.tasks_view(&project, window, cx).update(cx, |view, cx| view.open_task(task.clone(), window, cx));
                self.view = View::Main(MainView::Tasks { project, task });
                window.focus(&self.focus);
            }
            Nav::OpenSettings => {
                if !matches!(self.view, View::Settings { .. }) {
                    if self.settings.is_none() {
                        let settings = cx.new(|cx| SettingsView::new(window, cx));
                        cx.subscribe_in(&settings, window, |this, _, nav, window, cx| this.navigate(nav.clone(), window, cx)).detach();
                        self.settings = Some(settings);
                    }
                    self.view = View::Settings { back: self.main().clone() };
                    window.focus(&self.focus);
                }
            }
            Nav::Back => {
                if let View::Settings { back } = &self.view {
                    self.view = View::Main(back.clone());
                } else if let View::Main(MainView::Tasks { project, task: Some(_) }) = &self.view {
                    let project = project.clone();
                    return self.navigate(Nav::OpenTasks { project, task: None }, window, cx);
                }
            }
            Nav::AddSubscription => {
                let dialog = cx.new(|cx| AddSubscriptionDialog::new(window, cx));
                cx.subscribe_in(&dialog, window, |this, _, DialogEvent::Close, window, cx| this.close_dialog(window, cx)).detach();
                self.dialog = Some(Dialog::AddSubscription(dialog));
            }
            Nav::NewTask { project, status } => {
                let dialog = cx.new(|cx| NewTaskDialog::new(project, status, window, cx));
                cx.subscribe_in(&dialog, window, |this, _, DialogEvent::Close, window, cx| this.close_dialog(window, cx)).detach();
                self.dialog = Some(Dialog::NewTask(dialog));
            }
        }
        cx.notify();
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn thread_view(&mut self, id: &ThreadId, window: &mut Window, cx: &mut Context<Self>) -> Entity<ThreadView> {
        if let Some(view) = self.threads.get(id) {
            return view.clone();
        }
        let view = cx.new(|cx| ThreadView::new(id.clone(), window, cx));
        cx.subscribe_in(&view, window, |this, _, nav, window, cx| this.navigate(nav.clone(), window, cx)).detach();
        self.threads.insert(id.clone(), view.clone());
        view
    }

    fn tasks_view(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<TasksView> {
        if let Some(view) = self.tasks.get(project) {
            return view.clone();
        }
        let view = cx.new(|cx| TasksView::new(project.to_owned(), window, cx));
        cx.subscribe_in(&view, window, |this, _, nav, window, cx| this.navigate(nav.clone(), window, cx)).detach();
        self.tasks.insert(project.to_owned(), view.clone());
        view
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let store = store(cx);
        let state = &store.read(cx).state;
        let crumb = |text: String, current: bool| {
            div().truncate().text_color(if current { t.foreground } else { t.muted_foreground }).child(text)
        };
        let sep = || icon(Icon::ChevronRight, t.muted_foreground).size(px(14.));
        let mut trail: Vec<AnyElement> = vec![];
        let mut trailing: Option<AnyElement> = None;
        let mut stepper: Option<(String, String)> = None;
        match &self.view {
            View::Settings { .. } => {
                trail.push(crumb("Settings".into(), true).into_any_element());
                trailing = Some(
                    button("settings-back", "Back")
                        .icon(Icon::ArrowLeft)
                        .on_click(cx.listener(|this, _, window, cx| this.navigate(Nav::Back, window, cx)))
                        .into_any_element(),
                );
            }
            View::Main(MainView::New) => trail.push(crumb("New thread".into(), true).into_any_element()),
            View::Main(MainView::Thread(id)) => {
                if let Some(thread) = state.threads.get(id) {
                    trail.push(crumb(basename(state.tasks.thread_project(thread)), false).into_any_element());
                    trail.push(sep().into_any_element());
                    trail.push(crumb(if thread.title.is_empty() { "New thread".into() } else { thread.title.clone() }, true).min_w_0().into_any_element());
                    if let Some(sub) = state.subscriptions.get(&thread.subscription_id) {
                        let tip: SharedString = LOCKED_HINT.into();
                        trailing = Some(
                            div()
                                .id("thread-subscription")
                                .child(badge(sub.label.clone()))
                                .tooltip(move |_, cx| tooltip_view(tip.clone(), cx))
                                .into_any_element(),
                        );
                    }
                }
            }
            View::Main(MainView::Tasks { project, task }) => {
                trail.push(crumb(basename(project), false).into_any_element());
                trail.push(sep().into_any_element());
                match task.as_ref().and_then(|id| state.tasks.task(id)) {
                    Some(open) => {
                        let back_project = project.clone();
                        trail.push(
                            div()
                                .id("crumb-tasks")
                                .cursor_pointer()
                                .text_color(t.muted_foreground)
                                .hover(|s| s.text_color(t.foreground))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.navigate(Nav::OpenTasks { project: back_project.clone(), task: None }, window, cx)
                                }))
                                .child("Tasks")
                                .into_any_element(),
                        );
                        trail.push(sep().into_any_element());
                        trail.push(
                            div()
                                .flex()
                                .gap_1p5()
                                .min_w_0()
                                .child(div().flex_none().text_color(t.muted_foreground).child(open.key()))
                                .child(div().truncate().child(open.title.clone()))
                                .into_any_element(),
                        );
                        stepper = Some((project.clone(), open.id.clone()));
                    }
                    None => trail.push(crumb("Tasks".into(), true).into_any_element()),
                }
            }
        }
        if let Some((project, id)) = stepper {
            trailing = Some(self.task_stepper(&project, &id, cx));
        }
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(48.))
            .px_4()
            .when(!self.sidebar_open, |d| d.pl(px(80.)))
            .border_b_1()
            .border_color(t.border)
            .text_sm()
            .child(icon_button("toggle-sidebar", Icon::PanelLeft).muted().tooltip("Toggle sidebar (⌘B)").on_click(cx.listener(|this, _, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            })))
            .child(div().w(px(1.)).h(px(16.)).bg(t.border).mr_1())
            .child(div().flex().items_center().gap_1p5().min_w_0().flex_1().children(trail))
            .children(trailing)
            .into_any_element()
    }

    /// Linear's "3 / 12" with up and down: steps through the project's tasks in list order.
    fn task_stepper(&self, project: &str, task_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let ordered: Vec<String> = store(cx).read(cx).state.tasks.project_tasks(project).iter().map(|t| t.id.clone()).collect();
        let Some(index) = ordered.iter().position(|id| id == task_id) else { return div().into_any_element() };
        let step = |by: isize, project: String, ordered: Vec<String>| {
            move |this: &mut Workspace, _: &gpui::ClickEvent, window: &mut Window, cx: &mut Context<Workspace>| {
                let next = index.checked_add_signed(by).and_then(|i| ordered.get(i)).cloned();
                if let Some(next) = next {
                    this.navigate(Nav::OpenTasks { project: project.clone(), task: Some(next) }, window, cx);
                }
            }
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(t.muted_foreground)
            .child(div().mr_1().child(format!("{} / {}", index + 1, ordered.len())))
            .child(
                icon_button("next-task", Icon::ArrowDown)
                    .muted()
                    .size(ButtonSize::Icon)
                    .disabled(index + 1 == ordered.len())
                    .on_click(cx.listener(step(1, project.to_owned(), ordered.clone()))),
            )
            .child(
                icon_button("previous-task", Icon::ArrowUp)
                    .muted()
                    .size(ButtonSize::Icon)
                    .disabled(index == 0)
                    .on_click(cx.listener(step(-1, project.to_owned(), ordered))),
            )
            .into_any_element()
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.view.clone() {
            View::Settings { .. } => self.settings.clone().map_or_else(|| div().into_any_element(), |s| s.into_any_element()),
            View::Main(MainView::New) => self.new_thread.clone().into_any_element(),
            View::Main(MainView::Thread(id)) => {
                if store(cx).read(cx).state.threads.contains_key(&id) {
                    self.thread_view(&id, window, cx).into_any_element()
                } else {
                    // The thread was removed; fall back to a new one.
                    self.view = View::Main(MainView::New);
                    self.new_thread.clone().into_any_element()
                }
            }
            View::Main(MainView::Tasks { project, .. }) => self.tasks_view(&project, window, cx).into_any_element(),
        }
    }

    fn errors(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx);
        let errors = store(cx).read(cx).errors.clone();
        (!errors.is_empty()).then(|| {
            div()
                .absolute()
                .bottom_4()
                .right_4()
                .flex()
                .flex_col()
                .gap_2()
                .max_w(px(420.))
                .children(errors.into_iter().enumerate().map(|(i, error)| {
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(t.destructive.opacity(0.4))
                        .bg(t.background)
                        .shadow_lg()
                        .text_sm()
                        .child(div().pt(px(2.)).child(icon(Icon::CircleAlert, t.destructive)))
                        .child(div().flex_1().min_w_0().child(error))
                        .child(icon_button(("dismiss-error", i), Icon::X).muted().on_click(move |_, _, cx| {
                            store(cx).update(cx, |store, cx| store.dismiss_error(i, cx));
                        }))
                }))
                .into_any_element()
        })
    }

}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let content = self.content(window, cx);
        let header = self.header(cx);
        let errors = self.errors(cx);
        let dialog = self.dialog.as_ref().map(|dialog| match dialog {
            Dialog::AddSubscription(d) => d.clone().into_any_element(),
            Dialog::NewTask(d) => d.clone().into_any_element(),
        });
        let loaded = store(cx).read(cx).loaded;
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NewThread, window, cx| {
                this.view = View::Main(MainView::New);
                this.new_thread.read(cx).focus(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.navigate(Nav::OpenSettings, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &GoBack, window, cx| {
                if this.dialog.is_some() {
                    return this.close_dialog(window, cx);
                }
                this.navigate(Nav::Back, window, cx)
            }))
            .relative()
            .flex()
            .size_full()
            .bg(t.sidebar)
            .text_color(t.foreground)
            .font_family(SANS)
            .text_sm()
            .when(self.sidebar_open, |d| d.child(sidebar::render(self, cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .m_2()
                    .when(self.sidebar_open, |d| d.ml_0())
                    .rounded_xl()
                    .border_1()
                    .border_color(t.border)
                    .bg(t.background)
                    .shadow_sm()
                    .overflow_hidden()
                    .child(header)
                    .child(div().flex().flex_col().flex_1().min_h_0().when(loaded, |d| d.child(content))),
            )
            .children(errors)
            .children(dialog)
            .font_weight(FontWeight::NORMAL)
    }
}
