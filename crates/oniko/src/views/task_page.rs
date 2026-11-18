//! One task, opened full-size: editable title and description, properties on the right.

use std::time::Duration;

use gpui::{AnyElement, App, AsyncApp, Context, Corner, Entity, EventEmitter, FontWeight, IntoElement, SharedString, Task as GpuiTask, WeakEntity, Window, div, prelude::*, px};
use oniko_core::model::{StatusKind, Task, TaskId, TaskRun, TaskStatus, ThreadStatus};
use oniko_core::stores::tasks::TaskPatch;

use crate::icons::{Icon, icon};
use crate::state::{run, store};
use crate::theme::theme;
use crate::ui::text_field::{EnterKey, TextField, TextFieldEvent};
use crate::ui::{MenuEntry, Variant, button, dropdown, item};
use crate::views::tasks::status_menu;
use crate::views::{Nav, account_color, ago, shorten_home, status_icon};

const AUTOSAVE: Duration = Duration::from_millis(600);

pub struct TaskPage {
    pub task_id: TaskId,
    project: String,
    title: Entity<TextField>,
    description: Entity<TextField>,
    open_menu: Option<SharedString>,
    /// A save scheduled while typing pauses; dropped (and so cancelled) when a newer edit arrives.
    pending_title: Option<GpuiTask<()>>,
    pending_description: Option<GpuiTask<()>>,
}

impl EventEmitter<Nav> for TaskPage {}

impl TaskPage {
    pub fn new(task_id: TaskId, project: String, window: &mut Window, cx: &mut Context<Self>) -> TaskPage {
        let task = store(cx).read(cx).state.tasks.task(&task_id).cloned();
        let title_text = task.as_ref().map(|t| t.title.clone()).unwrap_or_default();
        let description_text = task.as_ref().map(|t| t.description.clone()).unwrap_or_default();
        let title = cx.new(|cx| TextField::new(window, cx).with_text(title_text).placeholder("Task title"));
        let description = cx.new(|cx| TextField::new(window, cx).multiline(4, 40, EnterKey::Breaks).with_text(description_text).placeholder("Add description…"));
        cx.subscribe_in(&title, window, |this, _, event, window, cx| match event {
            TextFieldEvent::Changed => this.pending_title = Some(this.schedule(true, cx)),
            TextFieldEvent::Submit | TextFieldEvent::Escape => window.blur(),
            TextFieldEvent::Blurred => this.save_title(cx),
            TextFieldEvent::Focused => {}
        })
        .detach();
        cx.subscribe_in(&description, window, |this, _, event, window, cx| match event {
            TextFieldEvent::Changed => this.pending_description = Some(this.schedule(false, cx)),
            TextFieldEvent::Escape | TextFieldEvent::Submit => window.blur(),
            TextFieldEvent::Blurred => this.save_description(cx),
            TextFieldEvent::Focused => {}
        })
        .detach();
        // A change that arrives from elsewhere shows up unless the field is being edited.
        cx.observe_in(&store(cx), window, |this, _, window, cx| {
            if let Some(task) = this.task(cx).cloned() {
                if !this.title.read(cx).is_focused(window) && this.title.read(cx).text() != task.title {
                    this.title.update(cx, |f, cx| f.set_text(task.title.clone(), cx));
                }
                if !this.description.read(cx).is_focused(window) && this.description.read(cx).text() != task.description {
                    this.description.update(cx, |f, cx| f.set_text(task.description.clone(), cx));
                }
            }
            cx.notify();
        })
        .detach();
        TaskPage { task_id, project, title, description, open_menu: None, pending_title: None, pending_description: None }
    }

    fn task<'a>(&self, cx: &'a App) -> Option<&'a Task> {
        store(cx).read(cx).state.tasks.task(&self.task_id)
    }

    fn schedule(&self, title: bool, cx: &mut Context<Self>) -> GpuiTask<()> {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(AUTOSAVE).await;
            let _ = this.update(cx, |this, cx| if title { this.save_title(cx) } else { this.save_description(cx) });
        })
    }

    fn save_title(&mut self, cx: &mut Context<Self>) {
        self.pending_title = None;
        let Some(task) = self.task(cx) else { return };
        let value = self.title.read(cx).text().trim().to_owned();
        if !value.is_empty() && value != task.title {
            run(cx, |c| c.update_task(self.task_id.clone(), TaskPatch { title: Some(value), ..Default::default() }));
        }
    }

    fn save_description(&mut self, cx: &mut Context<Self>) {
        self.pending_description = None;
        let Some(task) = self.task(cx) else { return };
        let value = self.description.read(cx).text().trim().to_owned();
        if value != task.description {
            run(cx, |c| c.update_task(self.task_id.clone(), TaskPatch { description: Some(value), ..Default::default() }));
        }
    }

    fn menu_setter(&self, key: &'static str, cx: &Context<Self>) -> impl Fn(bool, &mut Window, &mut App) + 'static {
        let entity = cx.entity();
        move |open, _, cx| entity.update(cx, |this, cx| {
            this.open_menu = open.then(|| key.into());
            cx.notify();
        })
    }

    fn subscription_property(&self, task: &Task, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let state = &store(cx).read(cx).state;
        let thread_id = task.thread_id().cloned();
        let assigned = task.subscription_id.as_deref().and_then(|id| state.subscriptions.get(id)).cloned();
        let running = thread_id.as_ref().and_then(|id| state.threads.get(id)).and_then(|th| state.subscriptions.get(&th.subscription_id)).cloned();
        let active = state.subscriptions.active().cloned();
        let shown = running.clone().or(assigned.clone()).or(active.clone());
        let label = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.))
            .px_2()
            .rounded_md()
            .text_sm()
            .child(icon(Icon::Claude, t.muted_foreground))
            .child(div().truncate().child(shown.as_ref().map_or("No subscription".into(), |s| s.label.clone())))
            .when(thread_id.is_none() && assigned.is_none() && shown.is_some(), |d| d.child(div().text_color(t.muted_foreground).child("· active")));
        if thread_id.is_some() {
            return div()
                .id("task-subscription")
                .child(label.child(icon(Icon::Lock, t.muted_foreground).size(px(12.))))
                .tooltip(|_, cx| crate::ui::tooltip_view("The agent stays on the subscription it started with".into(), cx))
                .into_any_element();
        }
        let subs = state.subscriptions.subscriptions.clone();
        let current = task.subscription_id.clone();
        let task_id = task.id.clone();
        dropdown(
            "task-subscription",
            crate::ui::is_open(&self.open_menu, "subscription"),
            Corner::TopLeft,
            label.hover(|s| s.bg(t.accent)).child(icon(Icon::ChevronDown, t.muted_foreground).size(px(12.))),
            move || {
                let clear = task_id.clone();
                let mut rows = vec![
                    item(format!("Active subscription{}", active.as_ref().map_or(String::new(), |a| format!(" ({})", a.label))), move |_, cx| {
                        run(cx, |c| c.update_task(clear.clone(), TaskPatch { subscription_id: Some(None), ..Default::default() }))
                    })
                    .checked(current.is_none())
                    .entry(),
                    MenuEntry::Separator,
                ];
                rows.extend(subs.iter().map(|sub| {
                    let (task_id, sub_id) = (task_id.clone(), sub.id.clone());
                    item(sub.label.clone(), move |_, cx| {
                        run(cx, |c| c.update_task(task_id.clone(), TaskPatch { subscription_id: Some(Some(sub_id.clone())), ..Default::default() }))
                    })
                    .dot(account_color(&sub.account, &t))
                    .checked(current.as_ref() == Some(&sub.id))
                    .entry()
                }));
                rows
            },
            self.menu_setter("subscription", cx),
            cx,
        )
    }

    /// Where the task's agent is: how to start it, what went wrong, or its thread, branch and worktree.
    fn agent_property(&self, task: &Task, statuses: &[TaskStatus], cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let home = store(cx).read(cx).home.clone();
        let hint = |text: String| div().px_2().text_xs().text_color(t.muted_foreground).line_height(px(18.)).child(text).into_any_element();
        let queue = statuses.iter().find(|s| s.kind == StatusKind::Unstarted).map_or("a Not started status".to_owned(), |s| s.name.clone());
        match &task.run {
            None => hint(format!("Move the task to {queue} to start an agent on it in its own worktree.")),
            Some(TaskRun::Starting) => div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(icon(Icon::Loader, t.muted_foreground).size(px(12.)))
                .child("Creating the worktree…")
                .into_any_element(),
            Some(TaskRun::Failed { error }) => div()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .text_xs()
                .child(div().text_color(t.destructive).child(error.clone()))
                .child(div().text_color(t.muted_foreground).child(format!("Move the task to {queue} again to retry.")))
                .into_any_element(),
            Some(run) => {
                let Some(info) = run.info().cloned() else { return div().into_any_element() };
                let status = store(cx).read(cx).state.threads.get(&info.thread_id).map(|th| th.status.clone());
                let stage = match (&status, run) {
                    (Some(ThreadStatus::AwaitingPermission { .. }), _) => "Needs your approval",
                    (Some(ThreadStatus::Running { .. }), TaskRun::Finished(_)) => "Working on it",
                    (_, TaskRun::Working(_)) => "Working on it",
                    (_, TaskRun::PullRequest(_)) => "Opening a pull request",
                    _ => "Finished",
                };
                let thread_id = info.thread_id.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        button("task-open-thread", stage)
                            .icon(Icon::MessageSquare)
                            .full_width()
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(Nav::OpenThread(thread_id.clone())))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(icon(Icon::GitBranch, t.muted_foreground).size(px(12.)))
                            .child(div().truncate().child(info.branch.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(icon(Icon::FolderGit, t.muted_foreground).size(px(12.)))
                            .child(div().truncate().child(shorten_home(&info.worktree, &home))),
                    )
                    .into_any_element()
            }
        }
    }
}

fn property(label: &str, body: impl IntoElement, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().px_2().text_xs().font_weight(FontWeight::MEDIUM).text_color(t.muted_foreground).child(label.to_owned()))
        .child(body)
}

impl Render for TaskPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let Some(task) = self.task(cx).cloned() else { return div().into_any_element() };
        let statuses: Vec<TaskStatus> = store(cx).read(cx).state.tasks.project_statuses(&self.project).into_iter().cloned().collect();
        let status = statuses.iter().find(|s| s.id == task.status_id).cloned();
        let now = oniko_core::util::now_ms();
        let task_id = task.id.clone();
        let status_picker = status.as_ref().map(|status| {
            let move_id = task_id.clone();
            status_menu(
                "task-status",
                crate::ui::is_open(&self.open_menu, "status"),
                statuses.clone(),
                task.status_id.clone(),
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(30.))
                    .px_2()
                    .rounded_md()
                    .text_sm()
                    .hover(|s| s.bg(t.accent))
                    .child(status_icon(status, cx))
                    .child(status.name.clone()),
                move |status_id, _, cx| run(cx, |c| c.move_task(move_id.clone(), status_id, None)),
                self.menu_setter("status", cx),
                cx,
            )
        });
        let pull_request_locked = matches!(task.run, Some(TaskRun::PullRequest(_) | TaskRun::Finished(_)));
        let toggle_id = task_id.clone();
        let pull_request = task.pull_request;
        let delete_id = task_id.clone();
        let project = self.project.clone();
        let _ = window;
        div()
            .id("task-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .gap_10()
                    .w_full()
                    .max_w(px(1100.))
                    .mx_auto()
                    .px_10()
                    .py_8()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).line_height(px(32.)).child(self.title.clone()))
                            .child(div().text_sm().line_height(px(22.)).child(self.description.clone()))
                            .child(
                                div()
                                    .mt_6()
                                    .pt_4()
                                    .border_t_1()
                                    .border_color(t.border)
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Activity"))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .text_xs()
                                            .text_color(t.muted_foreground)
                                            .child(icon(Icon::CircleDot, t.muted_foreground).size(px(12.)))
                                            .child(format!("Task created · {}", ago(task.created_at, now))),
                                    )
                                    .when(task.updated_at > task.created_at, |d| {
                                        d.child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .text_xs()
                                                .text_color(t.muted_foreground)
                                                .child(icon(Icon::PenLine, t.muted_foreground).size(px(12.)))
                                                .child(format!("Last updated · {}", ago(task.updated_at, now))),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .w(px(260.))
                            .flex_none()
                            .child(property(
                                "Properties",
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .children(status_picker)
                                    .child(self.subscription_property(&task, cx))
                                    .child(
                                        button("task-pull-request", "Open a pull request")
                                            .icon(Icon::GitPullRequest)
                                            .full_width()
                                            .disabled(pull_request_locked)
                                            .trailing(if pull_request { Icon::Check } else { Icon::Circle })
                                            .on_click(move |_, _, cx| {
                                                run(cx, |c| c.update_task(toggle_id.clone(), TaskPatch { pull_request: Some(!pull_request), ..Default::default() }))
                                            }),
                                    ),
                                cx,
                            ))
                            .child(property("Agent", self.agent_property(&task, &statuses, cx), cx))
                            .child(property("Task", div().px_2().text_sm().text_color(t.muted_foreground).child(task.key()), cx))
                            .child(
                                button("task-delete", "Delete task")
                                    .icon(Icon::Trash)
                                    .variant(Variant::Destructive)
                                    .full_width()
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        run(cx, |c| c.remove_task(delete_id.clone()));
                                        cx.emit(Nav::OpenTasks { project: project.clone(), task: None });
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
