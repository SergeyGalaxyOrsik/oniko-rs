//! Projects with their threads and task boards, settings, and the subscription switcher.

use gpui::{AnyElement, Context, Corner, FontWeight, ImageSource, IntoElement, SharedString, div, img, prelude::*, px};
use oniko_core::model::{AccountStatus, Thread, ThreadStatus};

use crate::icons::{Icon, icon};
use crate::state::{run, store};
use crate::theme::theme;
use crate::ui::{MenuEntry, dropdown, item, status_dot};
use crate::views::{Nav, account_color, account_label, basename, relative_time};
use crate::workspace::{MainView, View, Workspace};

fn dot(status: &ThreadStatus, t: &crate::theme::Theme) -> Option<gpui::Hsla> {
    match status {
        ThreadStatus::Connecting | ThreadStatus::Running { .. } => Some(t.sky),
        ThreadStatus::AwaitingPermission { .. } | ThreadStatus::AuthRequired { .. } => Some(t.amber),
        ThreadStatus::Error { .. } => Some(t.red),
        _ => None,
    }
}

fn nav_row(id: impl Into<SharedString>, active: bool, cx: &Context<Workspace>) -> gpui::Stateful<gpui::Div> {
    let t = theme(cx);
    div()
        .id(id.into())
        .flex()
        .items_center()
        .gap_2()
        .h(px(30.))
        .px_2()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .when(active, |d| d.bg(t.accent).font_weight(FontWeight::MEDIUM))
        .hover(|s| s.bg(t.accent))
}

pub fn render(ws: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let t = theme(cx);
    let store = store(cx);
    let state = &store.read(cx).state;
    let threads: Vec<&Thread> = state.thread_list();
    // With one subscription the label says nothing, so rows only carry it once there is a choice.
    let labels = state.subscriptions.subscriptions.len() > 1;
    let mut projects: Vec<(String, Vec<&Thread>)> = Vec::new();
    for thread in &threads {
        let project = state.tasks.thread_project(thread).to_owned();
        match projects.iter_mut().find(|(p, _)| *p == project) {
            Some((_, list)) => list.push(thread),
            None => projects.push((project, vec![thread])),
        }
    }
    // A project whose threads are all gone still has its board.
    for project in state.tasks.projects() {
        if !projects.iter().any(|(p, _)| *p == project) {
            projects.push((project, vec![]));
        }
    }
    let selected = match ws.main() {
        MainView::Thread(id) => Some(id.clone()),
        _ => None,
    };
    let tasks_project = match ws.main() {
        MainView::Tasks { project, .. } => Some(project.clone()),
        _ => None,
    };
    let settings_open = matches!(ws.view, View::Settings { .. });
    let now = ws.now;

    let project_groups = projects.into_iter().enumerate().map(|(pi, (project, list))| {
        let open = !ws.collapsed.contains(&project);
        let toggle = project.clone();
        let task_count = state.tasks.tasks.iter().filter(|task| task.project == project).count();
        let open_tasks = project.clone();
        let tasks_row = nav_row(format!("tasks-{pi}"), tasks_project.as_deref() == Some(project.as_str()), cx)
            .pl(px(28.))
            .on_click(cx.listener(move |this, _, window, cx| this.navigate(Nav::OpenTasks { project: open_tasks.clone(), task: None }, window, cx)))
            .child(icon(crate::icons::Icon::ListTodo, t.muted_foreground).size(px(14.)))
            .child(div().flex_1().child("Tasks"))
            .when(task_count > 0, |d| d.child(div().text_xs().text_color(t.muted_foreground).child(task_count.to_string())));
        let thread_rows = list.into_iter().map(|thread| {
            let id = thread.id.clone();
            let label = labels.then(|| state.subscriptions.get(&thread.subscription_id).map(|s| s.label.clone())).flatten();
            nav_row(format!("thread-{}", thread.id), selected.as_deref() == Some(thread.id.as_str()), cx)
                .pl(px(28.))
                .on_click(cx.listener(move |this, _, window, cx| this.navigate(Nav::OpenThread(id.clone()), window, cx)))
                .child(div().flex_1().min_w_0().truncate().child(if thread.title.is_empty() { "New thread".to_owned() } else { thread.title.clone() }))
                .when_some(label, |d, label| d.child(div().flex_none().max_w(px(72.)).truncate().text_xs().text_color(t.muted_foreground).child(label)))
                .when_some(dot(&thread.status, &t), |d, color| d.child(status_dot(color)))
                .child(div().flex_none().text_xs().text_color(t.muted_foreground).child(relative_time(thread.created_at, now)))
        });
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                nav_row(format!("project-{pi}"), false, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&toggle) {
                            this.collapsed.insert(toggle.clone());
                        }
                        cx.notify();
                    }))
                    .child(icon(Icon::Folder, t.muted_foreground).size(px(15.)))
                    .child(div().flex_1().min_w_0().truncate().child(basename(&project)))
                    .child(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.muted_foreground).size(px(14.))),
            )
            .when(open, |d| d.child(tasks_row).children(thread_rows))
    });

    let empty = threads.is_empty() && state.tasks.projects().is_empty();
    let active = state.subscriptions.active().cloned();
    let detail = match &active {
        Some(sub) => match &sub.account {
            AccountStatus::LoggedIn { email, .. } => email.clone().unwrap_or_else(|| "Logged in".into()),
            other => account_label(other).to_owned(),
        },
        None => "Add one in Settings".into(),
    };
    let subs = state.subscriptions.subscriptions.clone();
    let active_id = state.subscriptions.active_subscription_id.clone();
    let switcher_trigger = div()
        .flex()
        .items_center()
        .gap_2()
        .w_full()
        .p_2()
        .rounded_lg()
        .hover(|s| s.bg(t.accent))
        .child(
            div()
                .size(px(32.))
                .flex_none()
                .rounded_lg()
                .bg(t.muted)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(if active.is_some() { Icon::Claude } else { Icon::Settings }, t.foreground)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().truncate().font_weight(FontWeight::MEDIUM).child(active.as_ref().map_or("No subscription".to_owned(), |s| s.label.clone())))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .when_some(active.as_ref(), |d, sub| d.child(status_dot(account_color(&sub.account, &t))))
                        .child(div().truncate().child(detail)),
                ),
        )
        .child(icon(Icon::ChevronsUpDown, t.muted_foreground).size(px(14.)));
    let workspace = cx.entity();
    let manage = cx.entity();
    let switcher = dropdown(
        "subscription-switcher",
        ws.switcher_open,
        Corner::BottomLeft,
        switcher_trigger,
        move || {
            let mut rows = vec![MenuEntry::Label("Subscriptions".into())];
            rows.extend(subs.iter().map(|sub| {
                let id = sub.id.clone();
                let account = match &sub.account {
                    AccountStatus::LoggedIn { email, plan, .. } => {
                        let parts: Vec<String> = [email.clone(), oniko_core::model::plan_label(plan.as_deref())].into_iter().flatten().collect();
                        (!parts.is_empty()).then(|| parts.join(" · "))
                    }
                    _ => None,
                };
                let mut row = item(sub.label.clone(), move |_, cx| run(cx, |c| c.set_active_subscription(id.clone())))
                    .dot(account_color(&sub.account, &t))
                    .checked(Some(&sub.id) == active_id.as_ref());
                if let Some(account) = account {
                    row = row.detail(account);
                }
                row.entry()
            }));
            if !subs.is_empty() {
                rows.push(MenuEntry::Separator);
            }
            rows.push(
                item("Manage subscriptions…", move |window, cx| manage.update(cx, |this, cx| this.navigate(Nav::OpenSettings, window, cx)))
                    .icon(Icon::Settings)
                    .entry(),
            );
            rows
        },
        move |open, _, cx| workspace.update(cx, |this, cx| {
            this.switcher_open = open;
            cx.notify();
        }),
        cx,
    );

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(272.))
        .h_full()
        .bg(t.sidebar)
        // The top strip clears the macOS traffic lights.
        .child(div().h(px(38.)))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .px_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(32.))
                        .px_2()
                        .child(img(ImageSource::Resource(gpui::Resource::Embedded("icon.png".into()))).size(px(20.)).rounded_md())
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Oniko")),
                )
                .child(
                    nav_row("new-thread", matches!(ws.view, View::Main(MainView::New)), cx)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.view = View::Main(MainView::New);
                            this.new_thread.read(cx).focus(window, cx);
                            cx.notify();
                        }))
                        .child(icon(Icon::SquarePen, t.foreground).size(px(15.)))
                        .child("New thread"),
                ),
        )
        .child(
            div()
                .id("projects")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_2()
                .pt_3()
                .gap_1()
                .child(div().px_2().pb_1().text_xs().font_weight(FontWeight::MEDIUM).text_color(t.muted_foreground).child("Projects"))
                .when(empty, |d| d.child(div().px_2().text_xs().text_color(t.muted_foreground).child("No threads yet.")))
                .children(project_groups),
        )
        .child(
            div().px_2().pb_1().child(
                nav_row("open-settings", settings_open, cx)
                    .on_click(cx.listener(|this, _, window, cx| this.navigate(Nav::OpenSettings, window, cx)))
                    .child(icon(Icon::Settings, t.muted_foreground).size(px(15.)))
                    .child("Settings"),
            ),
        )
        .child(div().px_2().pb_2().child(switcher))
        .into_any_element()
}
