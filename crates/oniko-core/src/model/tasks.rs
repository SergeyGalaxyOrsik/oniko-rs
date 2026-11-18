use serde::{Deserialize, Serialize};

use super::subscriptions::SubscriptionId;
use super::thread::{Thread, ThreadId};

pub type TaskId = String;
pub type TaskStatusId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusColor {
    Slate,
    Blue,
    Amber,
    Green,
    Violet,
    Rose,
    Cyan,
    Orange,
}

impl StatusColor {
    pub const ALL: [StatusColor; 8] = [
        StatusColor::Slate,
        StatusColor::Blue,
        StatusColor::Amber,
        StatusColor::Green,
        StatusColor::Violet,
        StatusColor::Rose,
        StatusColor::Cyan,
        StatusColor::Orange,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            StatusColor::Slate => "slate",
            StatusColor::Blue => "blue",
            StatusColor::Amber => "amber",
            StatusColor::Green => "green",
            StatusColor::Violet => "violet",
            StatusColor::Rose => "rose",
            StatusColor::Cyan => "cyan",
            StatusColor::Orange => "orange",
        }
    }

    pub fn parse(value: &str) -> Option<StatusColor> {
        Self::ALL.into_iter().find(|c| c.as_str() == value)
    }
}

/// Which part of the workflow a status stands for; it picks the icon, as in Linear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
}

impl StatusKind {
    pub const ALL: [StatusKind; 5] =
        [StatusKind::Backlog, StatusKind::Unstarted, StatusKind::Started, StatusKind::Completed, StatusKind::Canceled];

    pub fn as_str(self) -> &'static str {
        match self {
            StatusKind::Backlog => "backlog",
            StatusKind::Unstarted => "unstarted",
            StatusKind::Started => "started",
            StatusKind::Completed => "completed",
            StatusKind::Canceled => "canceled",
        }
    }

    pub fn parse(value: &str) -> Option<StatusKind> {
        Self::ALL.into_iter().find(|k| k.as_str() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            StatusKind::Backlog => "Backlog",
            StatusKind::Unstarted => "Not started",
            StatusKind::Started => "In progress",
            StatusKind::Completed => "Completed",
            StatusKind::Canceled => "Canceled",
        }
    }
}

/// Projects are keyed by their folder, the same `cwd` threads are grouped by.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskStatus {
    pub id: TaskStatusId,
    pub project: String,
    pub name: String,
    pub color: StatusColor,
    pub kind: StatusKind,
    /// Orders columns; fractional so a status can be placed between two others without renumbering.
    pub position: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub thread_id: ThreadId,
    pub worktree: String,
    pub branch: String,
}

/// The agent working a task, from the moment the task enters a "Not started" status. A failed run
/// is retried by moving the task into one again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum TaskRun {
    Starting,
    Failed { error: String },
    Working(RunInfo),
    PullRequest(RunInfo),
    Finished(RunInfo),
}

impl TaskRun {
    pub fn info(&self) -> Option<&RunInfo> {
        match self {
            TaskRun::Working(info) | TaskRun::PullRequest(info) | TaskRun::Finished(info) => Some(info),
            TaskRun::Starting | TaskRun::Failed { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub project: String,
    /// Per project, from 1, never reused while the task exists; shown as `ONI-12`.
    pub number: i64,
    pub title: String,
    pub description: String,
    pub status_id: TaskStatusId,
    /// Orders tasks inside their status.
    pub position: f64,
    pub created_at: i64,
    pub updated_at: i64,
    /// The subscription the agent runs on; None means whichever is active when it starts. Fixed once a thread exists.
    pub subscription_id: Option<SubscriptionId>,
    /// After the agent's first turn, ask it to push the branch and open a pull request.
    pub pull_request: bool,
    pub run: Option<TaskRun>,
}

impl Task {
    pub fn thread_id(&self) -> Option<&ThreadId> {
        self.run.as_ref()?.info().map(|info| &info.thread_id)
    }

    pub fn key(&self) -> String {
        task_key(&self.project, self.number)
    }

    pub fn branch(&self) -> String {
        task_branch(&self.project, self.number, &self.title)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TasksState {
    pub statuses: Vec<TaskStatus>,
    pub tasks: Vec<Task>,
}

/// Every project starts with these; the user can rename, remove or add to them.
pub const DEFAULT_STATUSES: [(&str, StatusColor, StatusKind); 4] = [
    ("Backlog", StatusColor::Slate, StatusKind::Backlog),
    ("To do", StatusColor::Slate, StatusKind::Unstarted),
    ("In progress", StatusColor::Amber, StatusKind::Started),
    ("Done", StatusColor::Violet, StatusKind::Completed),
];

/// A new custom status takes the first color the project is not using yet.
pub fn next_status_color(existing: &[&TaskStatus]) -> StatusColor {
    StatusColor::ALL
        .into_iter()
        .find(|c| !existing.iter().any(|s| s.color == *c))
        .unwrap_or(StatusColor::ALL[existing.len() % StatusColor::ALL.len()])
}

impl TasksState {
    pub fn project_statuses(&self, project: &str) -> Vec<&TaskStatus> {
        let mut out: Vec<&TaskStatus> = self.statuses.iter().filter(|s| s.project == project).collect();
        out.sort_by(|a, b| a.position.total_cmp(&b.position));
        out
    }

    pub fn status_tasks(&self, status_id: &str) -> Vec<&Task> {
        let mut out: Vec<&Task> = self.tasks.iter().filter(|t| t.status_id == status_id).collect();
        out.sort_by(|a, b| a.position.total_cmp(&b.position));
        out
    }

    /// Projects that have a board, so one outlives its last thread.
    pub fn projects(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for s in &self.statuses {
            if !seen.contains(&s.project) {
                seen.push(s.project.clone());
            }
        }
        seen
    }

    /// Tasks of a project in the order the list shows them: by status, then by position.
    pub fn project_tasks(&self, project: &str) -> Vec<&Task> {
        self.project_statuses(project).into_iter().flat_map(|s| self.status_tasks(&s.id)).collect()
    }

    pub fn status(&self, id: &str) -> Option<&TaskStatus> {
        self.statuses.iter().find(|s| s.id == id)
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_for_thread(&self, thread_id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.thread_id().is_some_and(|id| id == thread_id))
    }

    /// A task's thread runs in its worktree, but belongs under the task's project.
    pub fn thread_project<'a>(&'a self, thread: &'a Thread) -> &'a str {
        self.task_for_thread(&thread.id).map_or(thread.cwd.as_str(), |t| t.project.as_str())
    }
}

/// `ONI` for a folder named `oniko`: the first three letters or digits of the folder name.
pub fn project_key(project: &str) -> String {
    let name = project.split(['/', '\\']).rfind(|s| !s.is_empty()).unwrap_or("");
    let key: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).take(3).collect::<String>().to_uppercase();
    if key.is_empty() { "TSK".into() } else { key }
}

pub fn task_key(project: &str, number: i64) -> String {
    format!("{}-{number}", project_key(project))
}

/// `oni-12-fix-the-login-form`: the key plus a slug of the title, used for both the branch and the worktree folder.
pub fn task_branch(project: &str, number: i64, title: &str) -> String {
    let mut slug = String::new();
    let mut gap = false;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
            gap = false;
        } else if !gap {
            slug.push('-');
            gap = true;
        }
    }
    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_matches('-');
    let key = task_key(project, number).to_lowercase();
    if slug.is_empty() { key } else { format!("{key}-{slug}") }
}

/// A position that sorts after `before` and ahead of `after`; either end may be open.
pub fn position_between(before: Option<f64>, after: Option<f64>) -> f64 {
    match (before, after) {
        (None, None) => 1.0,
        (None, Some(after)) => after - 1.0,
        (Some(before), None) => before + 1.0,
        (Some(before), Some(after)) => (before + after) / 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_branches() {
        assert_eq!(project_key("/Users/me/oniko"), "ONI");
        assert_eq!(project_key("/x/.a-b"), "AB");
        assert_eq!(project_key("/"), "TSK");
        assert_eq!(task_key("/p/oniko", 12), "ONI-12");
        assert_eq!(task_branch("/p/oniko", 12, "Fix the login form!"), "oni-12-fix-the-login-form");
        assert_eq!(task_branch("/p/oniko", 3, "???"), "oni-3");
    }

    #[test]
    fn positions() {
        assert_eq!(position_between(None, None), 1.0);
        assert_eq!(position_between(None, Some(1.0)), 0.0);
        assert_eq!(position_between(Some(1.0), None), 2.0);
        assert_eq!(position_between(Some(1.0), Some(2.0)), 1.5);
    }

    #[test]
    fn run_json_matches_the_electron_shape() {
        let run = TaskRun::Working(RunInfo { thread_id: "t".into(), worktree: "/w".into(), branch: "b".into() });
        assert_eq!(serde_json::to_string(&run).unwrap(), r#"{"stage":"working","threadId":"t","worktree":"/w","branch":"b"}"#);
        let parsed: TaskRun = serde_json::from_str(r#"{"stage":"pull_request","threadId":"t","worktree":"/w","branch":"b"}"#).unwrap();
        assert!(matches!(parsed, TaskRun::PullRequest(_)));
        assert_eq!(serde_json::to_string(&TaskRun::Starting).unwrap(), r#"{"stage":"starting"}"#);
    }
}
