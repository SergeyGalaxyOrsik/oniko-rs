//! One task, opened full-size: editable title and description, properties on the right.

use std::time::Duration;

use gpui::{AnyElement, App, AsyncApp, Context, Corner, Entity, EventEmitter, FontWeight, IntoElement, SharedString, Task as GpuiTask, WeakEntity, Window, div, prelude::*, px};
use oniko_core::model::config::new_thread_config;
use oniko_core::model::reducer::AppState;
use oniko_core::model::{ConfigChoice, StatusKind, Subscription, Task, TaskId, TaskRun, TaskStatus, ThreadStatus};
use oniko_core::stores::tasks::TaskPatch;

use crate::icons::{Icon, icon};
use crate::state::{run, store};
use crate::theme::theme;
use crate::ui::text_field::{EnterKey, TextField, TextFieldEvent};
use crate::ui::{ButtonSize, MenuEntry, Variant, button, dropdown, icon_button, item};
use crate::views::tasks::status_menu;
use crate::views::{Nav, Progress, account_color, ago, shorten_home, status_icon};

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

    /// The model the agent starts on. The choices are the ones the agent offered on its newest thread.
    fn model_property(&self, task: &Task, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let state = &store(cx).read(cx).state;
        let thread = task.thread_id().and_then(|id| state.threads.get(id));
        let choices = model_choices(task, state);
        let shown = match thread.and_then(|th| th.config.iter().find(|c| c.id == "model")) {
            Some(control) => control.options.iter().find(|o| o.value == control.value).map_or(control.value.clone(), |o| o.label.clone()),
            None => task.model.as_deref().map_or("Last used".to_owned(), |m| model_name(m, choices.as_deref())),
        };
        let label = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.))
            .px_2()
            .rounded_md()
            .text_sm()
            .child(icon(Icon::Brain, t.muted_foreground))
            .child(div().truncate().child(shown));
        if thread.is_some() {
            return div()
                .id("task-model")
                .child(label.child(icon(Icon::Lock, t.muted_foreground).size(px(12.))))
                .tooltip(|_, cx| crate::ui::tooltip_view("The agent stays on the model it started with".into(), cx))
                .into_any_element();
        }
        let current = task.model.clone();
        let task_id = task.id.clone();
        let agent = task_subscription(task, state).map(|s| s.agent_id);
        let auto_modes = state.auto_modes.clone();
        dropdown(
            "task-model",
            crate::ui::is_open(&self.open_menu, "model"),
            Corner::TopLeft,
            label.hover(|s| s.bg(t.accent)).child(icon(Icon::ChevronDown, t.muted_foreground).size(px(12.))),
            move || {
                let set = move |model: Option<String>| {
                    let task_id = task_id.clone();
                    move |_: &mut Window, cx: &mut App| run(cx, |c| c.update_task(task_id.clone(), TaskPatch { model: Some(model.clone()), ..Default::default() }))
                };
                let mut rows = vec![item("Last used", set(None)).detail("The model of the agent's last thread").checked(current.is_none()).entry()];
                match choices {
                    Some(choices) => {
                        rows.push(MenuEntry::Separator);
                        rows.extend(choices.into_iter().map(|o| {
                            let checked = current.as_ref() == Some(&o.value);
                            let row = item(o.label, set(Some(o.value.clone()))).checked(checked);
                            let no_auto = agent.is_some_and(|agent| auto_modes.get(agent, &o.value) == Some(false));
                            if no_auto { row.detail("Asks before commands") } else { row }.entry()
                        }));
                    }
                    None => rows.push(item("Models appear after the agent has run once", |_, _| {}).disabled(true).entry()),
                }
                rows
            },
            self.menu_setter("model", cx),
            cx,
        )
    }

    /// One linked task; a click opens it, and `remove` drops the link.
    fn linked_task(&self, other: &Task, remove: Option<TaskId>, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let status = store(cx).read(cx).state.tasks.status(&other.status_id).cloned();
        let open = other.id.clone();
        let project = self.project.clone();
        let task_id = self.task_id.clone();
        div()
            .id(SharedString::from(format!("linked-{}", other.id)))
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.))
            .pl_2()
            .pr_0p5()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(t.accent))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(Nav::OpenTasks { project: project.clone(), task: Some(open.clone()) })))
            .children(status.map(|s| status_icon(&s, cx).into_any_element()))
            .child(div().flex_none().text_xs().text_color(t.muted_foreground).child(other.key()))
            .child(div().flex_1().min_w_0().truncate().child(other.title.clone()))
            .children(remove.map(|blocker| {
                icon_button(SharedString::from(format!("unlink-{blocker}")), Icon::X)
                    .muted()
                    .size(ButtonSize::Icon)
                    .tooltip("Stop waiting for this task")
                    .on_click(move |_, _, cx| run(cx, |c| c.remove_dependency(task_id.clone(), blocker.clone())))
            }))
            .into_any_element()
    }

    /// The tasks this one waits for, and a menu of the ones it could wait for without a cycle.
    fn blocked_by_property(&self, task: &Task, cx: &mut Context<Self>) -> AnyElement {
        let tasks = store(cx).read(cx).state.tasks.clone();
        let blockers: Vec<Task> = task.blocked_by.iter().filter_map(|id| tasks.task(id)).cloned().collect();
        let candidates: Vec<(Task, Option<TaskStatus>)> = tasks
            .project_tasks(&self.project)
            .into_iter()
            .filter(|other| other.id != task.id && !task.blocked_by.contains(&other.id) && !tasks.depends_on(&other.id, &task.id))
            .map(|other| (other.clone(), tasks.status(&other.status_id).cloned()))
            .collect();
        let t = theme(cx);
        let task_id = task.id.clone();
        let add = dropdown(
            "task-add-blocker",
            crate::ui::is_open(&self.open_menu, "blocker"),
            Corner::TopLeft,
            div()
                .flex()
                .items_center()
                .gap_2()
                .h(px(30.))
                .px_2()
                .rounded_md()
                .text_sm()
                .text_color(t.muted_foreground)
                .hover(|s| s.bg(t.accent).text_color(t.foreground))
                .child(icon(Icon::Plus, t.muted_foreground).size(px(14.)))
                .child("Add a task to wait for"),
            move || {
                if candidates.is_empty() {
                    return vec![item("No other task can come first", |_, _| {}).disabled(true).entry()];
                }
                candidates
                    .into_iter()
                    .map(|(other, status)| {
                        let (task_id, blocker) = (task_id.clone(), other.id.clone());
                        let row = item(format!("{} {}", other.key(), other.title), move |_, cx| run(cx, |c| c.add_dependency(task_id.clone(), blocker.clone())));
                        match status {
                            Some(status) => row.dot(t.status(status.color)).detail(status.name),
                            None => row,
                        }
                        .entry()
                    })
                    .collect()
            },
            self.menu_setter("blocker", cx),
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .children(blockers.iter().map(|b| self.linked_task(b, Some(b.id.clone()), cx)).collect::<Vec<_>>())
            .child(add)
            .into_any_element()
    }

    /// Where the task's agent is: how to start it, what went wrong, or its thread, branch and worktree.
    fn agent_property(&self, task: &Task, statuses: &[TaskStatus], cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let home = store(cx).read(cx).home.clone();
        let hint = |text: String| div().px_2().text_xs().text_color(t.muted_foreground).line_height(px(18.)).child(text).into_any_element();
        let queue = statuses.iter().find(|s| s.kind == StatusKind::Unstarted).map_or("a Not started status".to_owned(), |s| s.name.clone());
        let waiting = store(cx).read(cx).state.tasks.waiting_reason(task);
        let queued = statuses.iter().any(|s| s.id == task.status_id && s.kind == StatusKind::Unstarted);
        match &task.run {
            None => match waiting {
                Some(reason) => div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().px_2().text_sm().child(reason))
                    .child(hint(if queued {
                        "It starts on its own once every task it waits for is done.".to_owned()
                    } else {
                        format!("Move it to {queue}, and it starts once every task it waits for is done.")
                    }))
                    .into_any_element(),
                None => hint(format!("Move the task to {queue} to start an agent on it in its own worktree.")),
            },
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
                let (Some(thread_id), Some(branch)) = (run.thread_id().cloned(), run.branch().map(str::to_owned)) else { return div().into_any_element() };
                let worktree = run.worktree().map(str::to_owned);
                let pull_request = match Progress::of(task, &store(cx).read(cx).state.tasks) {
                    Some(progress @ (Progress::Review(_) | Progress::Merged(_))) => Some(div().px_2().child(progress.render("task-page", cx))),
                    _ => None,
                };
                let status = store(cx).read(cx).state.threads.get(&thread_id).map(|th| th.status.clone());
                let stage = match (&status, run) {
                    (Some(ThreadStatus::AwaitingPermission { .. }), _) => "Needs your approval",
                    (Some(ThreadStatus::Running { .. }), TaskRun::Finished(_)) => "Working on it",
                    (_, TaskRun::Working(_)) => "Working on it",
                    (_, TaskRun::PullRequest(_)) => "Opening a pull request",
                    (_, TaskRun::Review(_)) => "In review",
                    (_, TaskRun::Merged(_)) => "Merged",
                    _ => "Finished",
                };
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
                    .children(pull_request)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(icon(Icon::GitBranch, t.muted_foreground).size(px(12.)))
                            .child(div().truncate().child(branch)),
                    )
                    .children(worktree.map(|worktree| {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(icon(Icon::FolderGit, t.muted_foreground).size(px(12.)))
                            .child(div().truncate().child(shorten_home(&worktree, &home)))
                    }))
                    .into_any_element()
            }
        }
    }
}

/// The models the task's agent offered on its newest thread; None until that agent has run once.
pub fn model_choices(task: &Task, state: &AppState) -> Option<Vec<ConfigChoice>> {
    let subscription = task_subscription(task, state)?;
    new_thread_config(state.threads.values(), subscription.agent_id, &subscription.id).into_iter().find(|c| c.id == "model").map(|c| c.options)
}

/// The subscription the task's agent runs on, or would start on.
fn task_subscription<'a>(task: &Task, state: &'a AppState) -> Option<&'a Subscription> {
    task.thread_id()
        .and_then(|id| state.threads.get(id))
        .and_then(|th| state.subscriptions.get(&th.subscription_id))
        .or_else(|| task.subscription_id.as_deref().and_then(|id| state.subscriptions.get(id)))
        .or_else(|| state.subscriptions.active())
}

pub fn model_name(value: &str, choices: Option<&[ConfigChoice]>) -> String {
    choices.into_iter().flatten().find(|o| o.value == value).map_or(value.to_owned(), |o| o.label.clone())
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
        let dependents: Vec<Task> = store(cx).read(cx).state.tasks.dependents(&task.id).into_iter().cloned().collect();
        let pull_request_locked = !dependents.is_empty() || matches!(task.run, Some(TaskRun::PullRequest(_) | TaskRun::Finished(_) | TaskRun::Review(_) | TaskRun::Merged(_)));
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
                                    .child(self.model_property(&task, cx))
                                    .child(
                                        button("task-pull-request", "Open a pull request")
                                            .icon(Icon::GitPullRequest)
                                            .full_width()
                                            .disabled(pull_request_locked)
                                            .trailing(if pull_request { Icon::Check } else { Icon::Circle })
                                            .on_click(move |_, _, cx| {
                                                run(cx, |c| c.update_task(toggle_id.clone(), TaskPatch { pull_request: Some(!pull_request), ..Default::default() }))
                                            }),
                                    )
                                    .when(!dependents.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .px_2()
                                                .text_xs()
                                                .text_color(t.muted_foreground)
                                                .line_height(px(18.))
                                                .child("Always on: the tasks below wait for this pull request to be merged."),
                                        )
                                    }),
                                cx,
                            ))
                            .child(property("Blocked by", self.blocked_by_property(&task, cx), cx))
                            .when(!dependents.is_empty(), |d| {
                                let rows: Vec<AnyElement> = dependents.iter().map(|other| self.linked_task(other, None, cx)).collect();
                                d.child(property("Blocks", div().flex().flex_col().gap_0p5().children(rows), cx))
                            })
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
