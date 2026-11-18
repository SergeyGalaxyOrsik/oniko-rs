//! A project's tasks, as Linear's list or board, with drag to move and custom statuses.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{AnyElement, App, Context, Corner, Entity, EventEmitter, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px};
use oniko_core::model::{StatusKind, Task, TaskId, TaskStatus, TaskStatusId};
use oniko_core::stores::tasks::StatusPatch;

use crate::icons::{Icon, icon};
use crate::state::{run, store};
use crate::theme::theme;
use crate::ui::text_field::{TextField, TextFieldEvent};
use crate::ui::{ButtonSize, MenuEntry, button, dropdown, field_frame, icon_button, item};
use crate::views::task_page::TaskPage;
use crate::views::{Nav, short_date, status_icon};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    List,
    Board,
}

/// What a dragged task carries, and what it looks like under the pointer.
#[derive(Clone)]
pub struct DraggedTask {
    id: TaskId,
    label: SharedString,
}

impl Render for DraggedTask {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div().px_3().py_1p5().rounded_md().border_1().border_color(t.border).bg(t.background).shadow_lg().text_sm().child(self.label.clone())
    }
}

pub struct TasksView {
    project: String,
    layout: Layout,
    collapsed: HashSet<TaskStatusId>,
    renaming: Option<(TaskStatusId, Entity<TextField>)>,
    adding_status: Option<Entity<TextField>>,
    open_menu: Option<SharedString>,
    page: Option<Entity<TaskPage>>,
}

impl EventEmitter<Nav> for TasksView {}

impl TasksView {
    pub fn new(project: String, _: &mut Window, cx: &mut Context<Self>) -> TasksView {
        cx.observe(&store(cx), |this, _, cx| {
            // A task deleted while open, here or elsewhere, sends the page back to the list.
            if let Some(page) = &this.page {
                let id = page.read(cx).task_id.clone();
                if store(cx).read(cx).state.tasks.task(&id).is_none() && !store(cx).read(cx).state.tasks.statuses.is_empty() {
                    let project = this.project.clone();
                    cx.emit(Nav::OpenTasks { project, task: None });
                }
            }
            cx.notify();
        })
        .detach();
        run(cx, |c| c.open_project(project.clone()));
        TasksView { project, layout: Layout::List, collapsed: HashSet::new(), renaming: None, adding_status: None, open_menu: None, page: None }
    }

    pub fn open_task(&mut self, task: Option<TaskId>, window: &mut Window, cx: &mut Context<Self>) {
        match task {
            None => self.page = None,
            Some(id) => {
                if self.page.as_ref().is_some_and(|p| p.read(cx).task_id == id) {
                    return;
                }
                let page = cx.new(|cx| TaskPage::new(id, self.project.clone(), window, cx));
                cx.subscribe_in(&page, window, |_, _, nav, _, cx| cx.emit(nav.clone())).detach();
                self.page = Some(page);
            }
        }
        cx.notify();
    }

    fn set_menu(&mut self, menu: Option<SharedString>, cx: &mut Context<Self>) {
        self.open_menu = menu;
        cx.notify();
    }

    fn menu_setter(&self, key: String, cx: &Context<Self>) -> impl Fn(bool, &mut Window, &mut App) + 'static {
        let entity = cx.entity();
        move |open, _, cx| entity.update(cx, |this, cx| this.set_menu(open.then(|| key.clone().into()), cx))
    }

    fn start_rename(&mut self, status: &TaskStatus, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| TextField::new(window, cx).with_text(status.name.clone()));
        let id = status.id.clone();
        let original = status.name.clone();
        cx.subscribe_in(&field, window, move |this, field, event, _, cx| match event {
            TextFieldEvent::Submit | TextFieldEvent::Blurred => {
                let name = field.read(cx).text().trim().to_owned();
                if !name.is_empty() && name != original {
                    run(cx, |c| c.update_status(id.clone(), StatusPatch { name: Some(name), kind: None }));
                }
                this.renaming = None;
                cx.notify();
            }
            TextFieldEvent::Escape => {
                this.renaming = None;
                cx.notify();
            }
            _ => {}
        })
        .detach();
        field.update(cx, |f, cx| f.select_all_text(cx));
        field.read(cx).focus(window);
        self.renaming = Some((status.id.clone(), field));
        cx.notify();
    }

    fn start_add_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| TextField::new(window, cx).placeholder("Status name"));
        let project = self.project.clone();
        cx.subscribe_in(&field, window, move |this, field, event, _, cx| match event {
            TextFieldEvent::Submit => {
                let name = field.read(cx).text().trim().to_owned();
                if !name.is_empty() {
                    run(cx, |c| c.add_status(project.clone(), name));
                }
                this.adding_status = None;
                cx.notify();
            }
            TextFieldEvent::Escape => {
                this.adding_status = None;
                cx.notify();
            }
            TextFieldEvent::Blurred if field.read(cx).text().trim().is_empty() => {
                this.adding_status = None;
                cx.notify();
            }
            _ => {}
        })
        .detach();
        field.read(cx).focus(window);
        self.adding_status = Some(field);
        cx.notify();
    }

    fn status_name(&self, status: &TaskStatus, cx: &App) -> AnyElement {
        match &self.renaming {
            Some((id, field)) if *id == status.id => div().w(px(180.)).child(field_frame(true, false, cx).py_0p5().child(field.clone())).into_any_element(),
            _ => div().font_weight(FontWeight::MEDIUM).child(status.name.clone()).into_any_element(),
        }
    }

    /// Rename, change the workflow type, or delete a status; a deleted status's tasks move to the first one left.
    fn status_options(&self, status: &TaskStatus, statuses: &[TaskStatus], count: usize, cx: &mut Context<Self>) -> AnyElement {
        let key = format!("status-options-{}", status.id);
        let fallback = statuses.iter().find(|s| s.id != status.id).map(|s| s.name.clone());
        let entity = cx.entity();
        let status_for_rename = status.clone();
        let status_id = status.id.clone();
        let kind = status.kind;
        dropdown(
            SharedString::from(key.clone()),
            crate::ui::is_open(&self.open_menu, key.as_str()),
            Corner::TopRight,
            icon_button(SharedString::from(format!("{key}-trigger")), Icon::Ellipsis).muted().size(ButtonSize::Icon),
            move || {
                let rename_entity = entity.clone();
                let mut rows = vec![
                    item("Rename", move |window, cx| {
                        let status = status_for_rename.clone();
                        rename_entity.update(cx, |this, cx| this.start_rename(&status, window, cx));
                    })
                    .icon(Icon::Pencil)
                    .entry(),
                    MenuEntry::Separator,
                    MenuEntry::Label("Type".into()),
                ];
                rows.extend(StatusKind::ALL.iter().map(|k| {
                    let id = status_id.clone();
                    let k = *k;
                    item(k.label(), move |_, cx| run(cx, |c| c.update_status(id.clone(), StatusPatch { name: None, kind: Some(k) })))
                        .checked(k == kind)
                        .entry()
                }));
                rows.push(MenuEntry::Separator);
                let remove_id = status_id.clone();
                let mut delete = item("Delete status", move |_, cx| run(cx, |c| c.remove_status(remove_id.clone())))
                    .icon(Icon::Trash)
                    .destructive()
                    .disabled(fallback.is_none());
                if let Some(fallback) = &fallback
                    && count > 0
                {
                    delete = delete.detail(format!("{count} task{} move{} to {fallback}", if count == 1 { "" } else { "s" }, if count == 1 { "s" } else { "" }));
                }
                rows.push(delete.entry());
                rows
            },
            self.menu_setter(key, cx),
            cx,
        )
    }

    fn task_status_picker(&self, task: &Task, status: &TaskStatus, statuses: &[TaskStatus], cx: &mut Context<Self>) -> AnyElement {
        let key = format!("task-status-{}", task.id);
        let task_id = task.id.clone();
        let t = theme(cx);
        status_menu(
            key.clone(),
            crate::ui::is_open(&self.open_menu, key.as_str()),
            statuses.to_vec(),
            task.status_id.clone(),
            div().p_0p5().rounded_sm().hover(|s| s.bg(t.accent)).child(status_icon(status, cx)),
            move |status_id, _, cx| run(cx, |c| c.move_task(task_id.clone(), status_id, None)),
            self.menu_setter(key, cx),
            cx,
        )
    }

    fn header_buttons(&self, status: &TaskStatus, statuses: &[TaskStatus], count: usize, cx: &mut Context<Self>) -> AnyElement {
        let project = self.project.clone();
        let status_id = status.id.clone();
        div()
            .flex()
            .items_center()
            .gap_0p5()
            .child(self.status_options(status, statuses, count, cx))
            .child(
                icon_button(SharedString::from(format!("add-to-{}", status.id)), Icon::Plus)
                    .muted()
                    .size(ButtonSize::Icon)
                    .tooltip(format!("Add task to {}", status.name))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(Nav::NewTask { project: project.clone(), status: status_id.clone() }))),
            )
            .into_any_element()
    }

    fn drop_handler(&self, status_id: TaskStatusId, before: Option<TaskId>) -> impl Fn(&DraggedTask, &mut Window, &mut App) + 'static {
        move |dragged, _, cx| {
            if before.as_ref() != Some(&dragged.id) {
                run(cx, |c| c.move_task(dragged.id.clone(), status_id.clone(), before.clone()));
            }
        }
    }

    fn list_view(&self, statuses: &[TaskStatus], cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let state = store(cx).read(cx).state.tasks.clone();
        let sections = statuses.iter().map(|status| {
            let tasks: Vec<Task> = state.status_tasks(&status.id).into_iter().cloned().collect();
            let open = !self.collapsed.contains(&status.id);
            let toggle = status.id.clone();
            let rows = tasks.iter().map(|task| {
                let open_id = task.id.clone();
                let project = self.project.clone();
                let dragged = DraggedTask { id: task.id.clone(), label: format!("{} {}", task.key(), task.title).into() };
                div()
                    .id(SharedString::from(format!("task-row-{}", task.id)))
                    .flex()
                    .items_center()
                    .gap_3()
                    .h(px(42.))
                    .pl_2()
                    .pr_3()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.accent.opacity(0.6)))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(Nav::OpenTasks { project: project.clone(), task: Some(open_id.clone()) })))
                    .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                    .drag_over::<DraggedTask>(move |style, _, _, _| style.border_t_2().border_color(t.primary))
                    .on_drop(self.drop_handler(status.id.clone(), Some(task.id.clone())))
                    .child(icon(Icon::GripVertical, t.muted_foreground.opacity(0.5)).size(px(14.)))
                    .child(div().flex_none().w(px(64.)).text_xs().text_color(t.muted_foreground).child(task.key()))
                    .child(self.task_status_picker(task, status, statuses, cx))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_1()
                            .min_w_0()
                            .child(div().flex_none().max_w(px(420.)).truncate().text_sm().child(task.title.clone()))
                            .when(!task.description.is_empty(), |d| {
                                d.child(div().truncate().text_sm().text_color(t.muted_foreground).child(task.description.lines().next().unwrap_or("").to_owned()))
                            }),
                    )
                    .child(div().flex_none().text_xs().text_color(t.muted_foreground).child(short_date(task.created_at)))
            }).collect::<Vec<_>>();
            let count = tasks.len();
            div()
                .id(SharedString::from(format!("status-section-{}", status.id)))
                .flex()
                .flex_col()
                .gap_0p5()
                .on_drop(self.drop_handler(status.id.clone(), None))
                .child(
                    div()
                        .id(SharedString::from(format!("status-header-{}", status.id)))
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(36.))
                        .pl_2()
                        .pr_1p5()
                        .rounded_md()
                        .bg(t.muted.opacity(0.7))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.collapsed.remove(&toggle) {
                                this.collapsed.insert(toggle.clone());
                            }
                            cx.notify();
                        }))
                        .drag_over::<DraggedTask>(move |style, _, _, _| style.bg(t.muted))
                        .on_drop(self.drop_handler(status.id.clone(), tasks.first().map(|t| t.id.clone())))
                        .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.muted_foreground).size(px(14.)))
                        .child(status_icon(status, cx))
                        .child(self.status_name(status, cx))
                        .child(div().text_xs().text_color(t.muted_foreground).child(count.to_string()))
                        .child(div().flex_1())
                        .child(self.header_buttons(status, statuses, count, cx)),
                )
                .when(open, |d| d.children(rows))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .children(sections.collect::<Vec<_>>())
            .child(self.add_status(cx))
            .into_any_element()
    }

    fn board_view(&self, statuses: &[TaskStatus], cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let state = store(cx).read(cx).state.tasks.clone();
        let columns = statuses.iter().map(|status| {
            let tasks: Vec<Task> = state.status_tasks(&status.id).into_iter().cloned().collect();
            let count = tasks.len();
            let cards = tasks.iter().map(|task| {
                let open_id = task.id.clone();
                let project = self.project.clone();
                let dragged = DraggedTask { id: task.id.clone(), label: format!("{} {}", task.key(), task.title).into() };
                div()
                    .id(SharedString::from(format!("task-card-{}", task.id)))
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .px_3()
                    .py_2p5()
                    .rounded_md()
                    .border_1()
                    .border_color(t.border.opacity(0.7))
                    .bg(t.background)
                    .shadow_sm()
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.border))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(Nav::OpenTasks { project: project.clone(), task: Some(open_id.clone()) })))
                    .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                    .drag_over::<DraggedTask>(move |style, _, _, _| style.border_t_2().border_color(t.primary))
                    .on_drop(self.drop_handler(status.id.clone(), Some(task.id.clone())))
                    .child(div().text_xs().text_color(t.muted_foreground).child(task.key()))
                    .child(div().flex().items_start().gap_2().child(self.task_status_picker(task, status, statuses, cx)).child(div().flex_1().min_w_0().text_sm().child(task.title.clone())))
                    .when(!task.description.is_empty(), |d| d.child(div().text_xs().text_color(t.muted_foreground).line_clamp(2).child(task.description.clone())))
                    .child(div().text_xs().text_color(t.muted_foreground).child(short_date(task.created_at)))
            }).collect::<Vec<_>>();
            div()
                .id(SharedString::from(format!("status-column-{}", status.id)))
                .flex()
                .flex_col()
                .flex_none()
                .w(px(300.))
                .gap_2()
                .p_2()
                .rounded_lg()
                .bg(t.muted.opacity(0.5))
                .drag_over::<DraggedTask>(move |style, _, _, _| style.bg(t.muted))
                .on_drop(self.drop_handler(status.id.clone(), None))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(30.))
                        .px_1()
                        .child(status_icon(status, cx))
                        .child(self.status_name(status, cx))
                        .child(div().text_xs().text_color(t.muted_foreground).child(count.to_string()))
                        .child(div().flex_1())
                        .child(self.header_buttons(status, statuses, count, cx)),
                )
                .children(cards)
        });
        div()
            .id("tasks-board")
            .flex()
            .gap_3()
            .p_4()
            .size_full()
            .overflow_x_scroll()
            .children(columns.collect::<Vec<_>>())
            .child(div().flex_none().w(px(240.)).child(self.add_status(cx)))
            .into_any_element()
    }

    fn add_status(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.adding_status {
            Some(field) => div().flex().items_center().gap_2().w(px(260.)).child(field_frame(true, false, cx).child(field.clone())).into_any_element(),
            None => div()
                .flex()
                .child(
                    button("add-status", "Add status")
                        .icon(Icon::Plus)
                        .muted()
                        .on_click(cx.listener(|this, _, window, cx| this.start_add_status(window, cx))),
                )
                .into_any_element(),
        }
    }
}

/// Linear's "Change status" menu for a task; `trigger` opens it. The theme is read up front because
/// menu rows are built after the App borrow ends.
#[allow(clippy::too_many_arguments, reason = "a menu takes its state, its trigger and two callbacks")]
pub fn status_menu(
    id: impl Into<SharedString>,
    open: bool,
    statuses: Vec<TaskStatus>,
    current: TaskStatusId,
    trigger: impl IntoElement,
    on_pick: impl Fn(TaskStatusId, &mut Window, &mut App) + 'static,
    set_open: impl Fn(bool, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let t = theme(cx);
    let on_pick = Rc::new(on_pick);
    dropdown(
        id.into(),
        open,
        Corner::TopLeft,
        trigger,
        move || {
            let mut rows = vec![MenuEntry::Label("Change status".into())];
            rows.extend(statuses.iter().enumerate().map(|(i, status)| {
                let on_pick = on_pick.clone();
                let status_id = status.id.clone();
                let changed = status.id != current;
                let mut row = item(status.name.clone(), move |window, cx| {
                    if changed {
                        on_pick(status_id.clone(), window, cx)
                    }
                })
                .dot(t.status(status.color))
                .checked(status.id == current);
                if i < 9 {
                    row = row.detail(format!("{}", i + 1));
                }
                row.entry()
            }));
            rows
        },
        set_open,
        cx,
    )
}

impl Render for TasksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        if let Some(page) = &self.page {
            return page.clone().into_any_element();
        }
        let state = store(cx).read(cx).state.tasks.clone();
        let statuses: Vec<TaskStatus> = state.project_statuses(&self.project).into_iter().cloned().collect();
        let count = state.project_tasks(&self.project).len();
        let first = statuses.first().map(|s| s.id.clone());
        let project = self.project.clone();
        let layout_toggle = div()
            .flex()
            .items_center()
            .gap_0p5()
            .p_0p5()
            .rounded_md()
            .border_1()
            .border_color(t.border)
            .child(icon_button("layout-list", Icon::List).size(ButtonSize::Icon).selected(self.layout == Layout::List).tooltip("List").on_click(cx.listener(|this, _, _, cx| {
                this.layout = Layout::List;
                cx.notify();
            })))
            .child(icon_button("layout-board", Icon::SquareKanban).size(ButtonSize::Icon).selected(self.layout == Layout::Board).tooltip("Board").on_click(cx.listener(|this, _, _, cx| {
                this.layout = Layout::Board;
                cx.notify();
            })));
        let body = match self.layout {
            Layout::List => div().id("tasks-list").flex_1().min_h_0().overflow_y_scroll().child(self.list_view(&statuses, cx)).into_any_element(),
            Layout::Board => div().flex_1().min_h_0().child(self.board_view(&statuses, cx)).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(44.))
                    .px_4()
                    .border_b_1()
                    .border_color(t.border)
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("All tasks"))
                    .child(div().text_xs().text_color(t.muted_foreground).child(count.to_string()))
                    .child(div().flex_1())
                    .child(layout_toggle)
                    .child(
                        button("new-task", "New task")
                            .icon(Icon::Plus)
                            .disabled(first.is_none())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(status) = first.clone() {
                                    cx.emit(Nav::NewTask { project: project.clone(), status });
                                }
                            })),
                    ),
            )
            .child(body)
            .into_any_element()
    }
}
