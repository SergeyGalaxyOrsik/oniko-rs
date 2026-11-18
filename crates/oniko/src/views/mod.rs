//! The screens, and the bits they share.

pub mod composer;
pub mod dialogs;
pub mod new_thread;
pub mod settings;
pub mod sidebar;
pub mod task_page;
pub mod tasks;
pub mod thread;

use gpui::{AnyElement, App, Hsla, IntoElement, SharedString, div, prelude::*, px};
use oniko_core::model::{AccountStatus, PrState, PullRequest, StatusKind, Task, TaskId, TaskRun, TaskStatus, TaskStatusId, TasksState, ThreadId};

use crate::icons::{Icon, icon};
use crate::state::run;
use crate::theme::{Theme, theme};
use crate::ui::{ButtonSize, button};

/// What a screen asks the workspace to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Nav {
    OpenThread(ThreadId),
    OpenTasks { project: String, task: Option<TaskId> },
    OpenSettings,
    AddSubscription,
    NewTask { project: String, status: TaskStatusId },
    Back,
}

pub fn basename(path: &str) -> String {
    oniko_core::util::basename(path).to_owned()
}

pub fn format_duration(ms: i64) -> String {
    let seconds = ((ms as f64) / 1000.).round().max(0.) as i64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if seconds % 60 == 0 { format!("{minutes}m") } else { format!("{minutes}m {}s", seconds % 60) }
}

pub fn relative_time(at: i64, now: i64) -> String {
    let minutes = (now - at) / 60_000;
    if minutes < 1 {
        return "now".into();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

pub fn ago(at: i64, now: i64) -> String {
    match relative_time(at, now).as_str() {
        "now" => "just now".into(),
        other => format!("{other} ago"),
    }
}

pub fn shorten_home(path: &str, home: &str) -> String {
    if home.is_empty() {
        return path.to_owned();
    }
    if path == home {
        return "~".into();
    }
    match path.strip_prefix(home).filter(|rest| rest.starts_with('/')) {
        Some(rest) => format!("~{rest}"),
        None => path.to_owned(),
    }
}

/// "Jan 4", as the task list shows dates.
pub fn short_date(at_ms: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = at_ms.div_euclid(86_400_000);
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{} {day}", MONTHS[(month - 1) as usize])
}

pub fn account_label(account: &AccountStatus) -> &'static str {
    match account {
        AccountStatus::Unknown => "Not checked",
        AccountStatus::Checking => "Checking",
        AccountStatus::LoggedIn { .. } => "Logged in",
        AccountStatus::LoggedOut { .. } => "Logged out",
        AccountStatus::Error { .. } => "Error",
    }
}

pub fn account_color(account: &AccountStatus, t: &Theme) -> Hsla {
    match account {
        AccountStatus::Unknown => t.muted_foreground.opacity(0.5),
        AccountStatus::Checking => t.sky,
        AccountStatus::LoggedIn { .. } => t.emerald,
        AccountStatus::LoggedOut { .. } => t.amber,
        AccountStatus::Error { .. } => t.red,
    }
}

pub const LOCKED_HINT: &str = "Threads stay on the subscription they started with";

/// Where a task is in the plan, as its cards and rows show it in one line.
pub enum Progress {
    Waiting(String),
    Stage(&'static str),
    Failed,
    Review(PullRequest),
    Merged(PullRequest),
}

impl Progress {
    /// None for a task that is not queued behind anything and has not started.
    pub fn of(task: &Task, tasks: &TasksState) -> Option<Progress> {
        Some(match &task.run {
            None => Progress::Waiting(tasks.waiting_reason(task)?),
            Some(TaskRun::Starting) => Progress::Stage("Starting"),
            Some(TaskRun::Working(_)) => Progress::Stage("Agent working"),
            Some(TaskRun::PullRequest(_)) => Progress::Stage("Opening a pull request"),
            Some(TaskRun::Finished(_)) => Progress::Stage("Finished"),
            Some(TaskRun::Failed { .. }) => Progress::Failed,
            Some(TaskRun::Review(review)) => Progress::Review(review.pr.clone()),
            Some(TaskRun::Merged(merged)) => Progress::Merged(merged.pr.clone()),
        })
    }

    /// One muted line; a pull request links to GitHub, and one under review can be checked now.
    pub fn render(self, id: &str, cx: &App) -> AnyElement {
        let t = theme(cx);
        let line = div().flex().items_center().gap_1p5().min_w_0().text_xs().text_color(t.muted_foreground);
        match self {
            Progress::Waiting(reason) => line.child(div().truncate().child(reason)).into_any_element(),
            Progress::Stage(stage) => line.child(div().truncate().child(stage)).into_any_element(),
            Progress::Failed => line.text_color(t.destructive).child("Failed").into_any_element(),
            Progress::Review(pr) => {
                let label = match pr.state {
                    PrState::Closed => format!("PR #{} closed", pr.number),
                    _ => format!("PR #{} in review", pr.number),
                };
                line.child(pr_link(id, &pr, label, cx))
                    .when(pr.state == PrState::Open, |d| {
                        d.child(
                            button(SharedString::from(format!("{id}-check-pr")), "Check now")
                                .size(ButtonSize::Xs)
                                .muted()
                                .tooltip("Ask GitHub whether the pull request was merged")
                                .on_click(|_, _, cx| run(cx, |c| c.check_pull_requests())),
                        )
                    })
                    .into_any_element()
            }
            Progress::Merged(pr) => line.child(pr_link(id, &pr, format!("PR #{} merged", pr.number), cx)).into_any_element(),
        }
    }
}

fn pr_link(id: &str, pr: &PullRequest, label: String, cx: &App) -> AnyElement {
    let t = theme(cx);
    let url = pr.url.clone();
    div()
        .id(SharedString::from(format!("{id}-pr-link")))
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .hover(|s| s.text_color(t.foreground).underline())
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            cx.open_url(&url);
        })
        .child(icon(Icon::GitPullRequest, t.muted_foreground).size(px(12.)))
        .child(label)
        .into_any_element()
}

/// Linear's workflow icons: a dashed ring for backlog, an empty ring before work starts, a ring with a
/// dot as work moves along, a filled check when done and a filled cross when canceled.
pub fn status_icon(status: &TaskStatus, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    let color = t.status(status.color);
    let size = px(14.);
    match status.kind {
        StatusKind::Backlog => div().size(size).flex_none().rounded_full().border_1().border_dashed().border_color(color).into_any_element(),
        StatusKind::Unstarted => div().size(size).flex_none().rounded_full().border(px(1.5)).border_color(color).into_any_element(),
        StatusKind::Started => div()
            .size(size)
            .flex_none()
            .rounded_full()
            .border(px(1.5))
            .border_color(color)
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(6.)).rounded_full().bg(color))
            .into_any_element(),
        StatusKind::Completed | StatusKind::Canceled => div()
            .size(size)
            .flex_none()
            .rounded_full()
            .bg(color)
            .flex()
            .items_center()
            .justify_center()
            .child(icon(if status.kind == StatusKind::Completed { Icon::Check } else { Icon::X }, t.background).size(px(10.)))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_the_electron_app() {
        assert_eq!(format_duration(4_400), "4s");
        assert_eq!(format_duration(125_000), "2m 5s");
        assert_eq!(format_duration(120_000), "2m");
        assert_eq!(relative_time(0, 30_000), "now");
        assert_eq!(relative_time(0, 5 * 60_000), "5m");
        assert_eq!(relative_time(0, 3 * 3_600_000), "3h");
        assert_eq!(relative_time(0, 50 * 3_600_000), "2d");
        assert_eq!(shorten_home("/Users/a/x", "/Users/a"), "~/x");
        assert_eq!(shorten_home("/Users/ab", "/Users/a"), "/Users/ab");
        assert_eq!(short_date(1_793_791_240_865), "Nov 4");
        assert_eq!(short_date(0), "Jan 1");
    }
}
