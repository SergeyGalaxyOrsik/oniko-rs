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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

/// GitHub's view of a task's pull request, as of the last check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: i64,
    pub url: String,
    pub state: PrState,
    pub mergeable: Mergeable,
}

/// How far Oniko went about a conflict with the base branch: one attempt by the agent, then the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictFix {
    #[default]
    Clear,
    /// The agent is resolving it in the worktree right now.
    Fixing,
    /// The agent's turn on it ended; the next check tells whether it worked.
    Attempted,
    /// It still conflicted after the attempt and the user was told.
    Escalated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub thread_id: ThreadId,
    pub branch: String,
    /// None while parked: the folder is removed, the branch stays.
    pub worktree: Option<String>,
    pub pr: PullRequest,
    pub conflict_fix: ConflictFix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Merged {
    pub thread_id: ThreadId,
    pub branch: String,
    pub pr: PullRequest,
}

/// What a check of the pull request asks Oniko to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    Merged,
    Closed,
    FixConflict,
    ConflictPersists,
}

impl Review {
    /// The review once GitHub reports `pr`. Only transitions yield an action, so repeated checks act once.
    pub fn observe(&self, pr: PullRequest) -> (Review, Option<ReviewAction>) {
        let mut next = Review { pr: pr.clone(), ..self.clone() };
        let action = match (pr.state, pr.mergeable, self.conflict_fix) {
            (PrState::Merged, ..) => Some(ReviewAction::Merged),
            (PrState::Closed, ..) => (self.pr.state != PrState::Closed).then_some(ReviewAction::Closed),
            (PrState::Open, _, ConflictFix::Fixing) => None,
            (PrState::Open, Mergeable::Conflicting, ConflictFix::Clear) => {
                next.conflict_fix = ConflictFix::Fixing;
                Some(ReviewAction::FixConflict)
            }
            (PrState::Open, Mergeable::Conflicting, ConflictFix::Attempted) => {
                next.conflict_fix = ConflictFix::Escalated;
                Some(ReviewAction::ConflictPersists)
            }
            (PrState::Open, Mergeable::Mergeable, _) => {
                next.conflict_fix = ConflictFix::Clear;
                None
            }
            (PrState::Open, ..) => None,
        };
        (next, action)
    }
}

/// The agent working a task, from the moment the scheduler starts it. A failed run is retried by
/// moving the task into a "Not started" status again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum TaskRun {
    Starting,
    Failed { error: String },
    Working(RunInfo),
    /// The agent was asked to rebase on the base branch, push and open the pull request.
    PullRequest(RunInfo),
    /// Done without a pull request.
    Finished(RunInfo),
    Review(Review),
    /// Has no worktree: a merged task's folder is removed.
    Merged(Merged),
}

impl TaskRun {
    pub fn thread_id(&self) -> Option<&ThreadId> {
        match self {
            TaskRun::Working(info) | TaskRun::PullRequest(info) | TaskRun::Finished(info) => Some(&info.thread_id),
            TaskRun::Review(review) => Some(&review.thread_id),
            TaskRun::Merged(merged) => Some(&merged.thread_id),
            TaskRun::Starting | TaskRun::Failed { .. } => None,
        }
    }

    pub fn branch(&self) -> Option<&str> {
        match self {
            TaskRun::Working(info) | TaskRun::PullRequest(info) | TaskRun::Finished(info) => Some(&info.branch),
            TaskRun::Review(review) => Some(&review.branch),
            TaskRun::Merged(merged) => Some(&merged.branch),
            TaskRun::Starting | TaskRun::Failed { .. } => None,
        }
    }

    /// The folder the agent works in, when it is on disk.
    pub fn worktree(&self) -> Option<&str> {
        match self {
            TaskRun::Working(info) | TaskRun::PullRequest(info) | TaskRun::Finished(info) => Some(&info.worktree),
            TaskRun::Review(review) => review.worktree.as_deref(),
            TaskRun::Starting | TaskRun::Failed { .. } | TaskRun::Merged(_) => None,
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
    /// The model the agent runs on; None keeps the last thread's. Fixed once a thread exists.
    pub model: Option<String>,
    /// Tasks that must be completed before this one may start: same project, no cycles.
    pub blocked_by: Vec<TaskId>,
    pub run: Option<TaskRun>,
}

impl Task {
    pub fn thread_id(&self) -> Option<&ThreadId> {
        self.run.as_ref()?.thread_id()
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

    fn kind(&self, task: &Task) -> Option<StatusKind> {
        self.status(&task.status_id).map(|s| s.kind)
    }

    /// Done for the plan: merged, or moved to a completed status by hand.
    fn completed(&self, task: &Task) -> bool {
        self.kind(task) == Some(StatusKind::Completed)
    }

    /// The scheduling rule: queued in a "Not started" status, never run, and every blocker completed.
    pub fn ready_to_start(&self) -> Vec<TaskId> {
        self.tasks
            .iter()
            .filter(|t| t.run.is_none() && self.kind(t) == Some(StatusKind::Unstarted))
            .filter(|t| t.blocked_by.iter().all(|id| self.task(id).is_some_and(|b| self.completed(b))))
            .map(|t| t.id.clone())
            .collect()
    }

    /// Why a task that has not started is held back; a blocker that needs the user is named first.
    pub fn waiting_reason(&self, task: &Task) -> Option<String> {
        if task.run.is_some() {
            return None;
        }
        let pending: Vec<&Task> = task.blocked_by.iter().filter_map(|id| self.task(id)).filter(|b| !self.completed(b)).collect();
        if pending.is_empty() {
            return None;
        }
        let stuck: Vec<String> = pending
            .iter()
            .filter_map(|b| match &b.run {
                _ if self.kind(b) == Some(StatusKind::Canceled) => Some(format!("{} was canceled", b.key())),
                Some(TaskRun::Review(review)) if review.pr.state == PrState::Closed => {
                    Some(format!("{}: pull request closed without merging", b.key()))
                }
                Some(TaskRun::Failed { .. }) => Some(format!("{} failed", b.key())),
                _ => None,
            })
            .collect();
        if !stuck.is_empty() {
            return Some(stuck.join("; "));
        }
        Some(format!("Waiting for {}", pending.iter().map(|b| b.key()).collect::<Vec<_>>().join(", ")))
    }

    /// Whether `task` waits on `other`, directly or through other tasks.
    pub fn depends_on(&self, task: &str, other: &str) -> bool {
        let mut seen: Vec<&str> = Vec::new();
        let mut stack: Vec<&str> = vec![task];
        while let Some(id) = stack.pop() {
            for blocker in self.task(id).map(|t| t.blocked_by.as_slice()).unwrap_or_default() {
                if blocker == other {
                    return true;
                }
                if !seen.contains(&blocker.as_str()) {
                    seen.push(blocker);
                    stack.push(blocker);
                }
            }
        }
        false
    }

    /// Tasks that wait directly on `id`.
    pub fn dependents(&self, id: &str) -> Vec<&Task> {
        self.tasks.iter().filter(|t| t.blocked_by.iter().any(|b| b == id)).collect()
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

    fn pr(state: PrState, mergeable: Mergeable) -> PullRequest {
        PullRequest { number: 7, url: "https://github.com/o/r/pull/7".into(), state, mergeable }
    }

    fn review(conflict_fix: ConflictFix) -> Review {
        Review { thread_id: "t".into(), branch: "b".into(), worktree: None, pr: pr(PrState::Open, Mergeable::Mergeable), conflict_fix }
    }

    #[test]
    fn review_json_round_trips_under_its_stage() {
        let run = TaskRun::Review(review(ConflictFix::Clear));
        let text = serde_json::to_string(&run).unwrap();
        assert!(text.starts_with(r#"{"stage":"review","threadId":"t","branch":"b","worktree":null,"pr":{"number":7"#), "{text}");
        assert_eq!(serde_json::from_str::<TaskRun>(&text).unwrap(), run);
    }

    #[test]
    fn a_conflict_is_handed_to_the_agent_once_then_to_the_user_once() {
        let conflicting = || pr(PrState::Open, Mergeable::Conflicting);
        let (fixing, action) = review(ConflictFix::Clear).observe(conflicting());
        assert_eq!((fixing.conflict_fix, action), (ConflictFix::Fixing, Some(ReviewAction::FixConflict)));
        assert_eq!(fixing.observe(conflicting()), (fixing.clone(), None));
        let attempted = Review { conflict_fix: ConflictFix::Attempted, ..fixing };
        let (escalated, action) = attempted.observe(conflicting());
        assert_eq!((escalated.conflict_fix, action), (ConflictFix::Escalated, Some(ReviewAction::ConflictPersists)));
        assert_eq!(escalated.observe(conflicting()).1, None);
        let (clear, action) = escalated.observe(pr(PrState::Open, Mergeable::Mergeable));
        assert_eq!((clear.conflict_fix, action), (ConflictFix::Clear, None));
    }

    #[test]
    fn closing_and_merging_are_reported_on_the_transition() {
        let (closed, action) = review(ConflictFix::Clear).observe(pr(PrState::Closed, Mergeable::Unknown));
        assert_eq!(action, Some(ReviewAction::Closed));
        assert_eq!(closed.observe(pr(PrState::Closed, Mergeable::Unknown)).1, None);
        assert_eq!(closed.observe(pr(PrState::Merged, Mergeable::Unknown)).1, Some(ReviewAction::Merged));
    }

    struct Plan {
        state: TasksState,
    }

    impl Plan {
        fn new() -> Plan {
            let statuses = [("b", StatusKind::Backlog), ("u", StatusKind::Unstarted), ("s", StatusKind::Started), ("c", StatusKind::Completed), ("x", StatusKind::Canceled)]
                .into_iter()
                .enumerate()
                .map(|(i, (id, kind))| TaskStatus { id: id.into(), project: "/p/oniko".into(), name: id.into(), color: StatusColor::Slate, kind, position: i as f64 })
                .collect();
            Plan { state: TasksState { statuses, tasks: vec![] } }
        }

        fn task(mut self, number: i64, status: &str, blocked_by: &[i64]) -> Plan {
            self.state.tasks.push(Task {
                id: number.to_string(),
                project: "/p/oniko".into(),
                number,
                title: format!("task {number}"),
                description: String::new(),
                status_id: status.into(),
                position: number as f64,
                created_at: 0,
                updated_at: 0,
                subscription_id: None,
                pull_request: true,
                model: None,
                blocked_by: blocked_by.iter().map(|n| n.to_string()).collect(),
                run: None,
            });
            self
        }

        fn run(mut self, number: i64, run: TaskRun) -> Plan {
            self.state.tasks.iter_mut().find(|t| t.number == number).unwrap().run = Some(run);
            self
        }

        fn reason(&self, number: i64) -> Option<String> {
            self.state.waiting_reason(self.state.task(&number.to_string()).unwrap())
        }
    }

    #[test]
    fn starts_queued_tasks_whose_blockers_are_all_completed() {
        let working = TaskRun::Working(RunInfo { thread_id: "t".into(), worktree: "/w".into(), branch: "b".into() });
        let plan = Plan::new()
            .task(21, "s", &[])
            .task(22, "u", &[])
            .task(23, "u", &[21])
            .task(24, "u", &[21, 25])
            .task(25, "c", &[])
            .task(26, "b", &[])
            .task(27, "u", &[25])
            .task(28, "u", &[])
            .run(21, working)
            .run(28, TaskRun::Failed { error: "x".into() });
        assert_eq!(plan.state.ready_to_start(), ["22", "27"]);
        assert_eq!(plan.reason(23).as_deref(), Some("Waiting for ONI-21"));
        assert_eq!(plan.reason(24).as_deref(), Some("Waiting for ONI-21"));
        assert_eq!(plan.reason(22), None);
        assert_eq!(plan.reason(21), None);
    }

    #[test]
    fn names_the_blocker_that_needs_the_user() {
        let closed = TaskRun::Review(Review { pr: pr(PrState::Closed, Mergeable::Unknown), ..review(ConflictFix::Clear) });
        let plan = Plan::new()
            .task(21, "s", &[])
            .task(22, "x", &[])
            .task(23, "s", &[])
            .task(24, "u", &[21, 23])
            .task(25, "u", &[22])
            .run(21, closed)
            .run(23, TaskRun::Failed { error: "x".into() });
        assert_eq!(plan.reason(24).as_deref(), Some("ONI-21: pull request closed without merging; ONI-23 failed"));
        assert_eq!(plan.reason(25).as_deref(), Some("ONI-22 was canceled"));
        assert!(plan.state.ready_to_start().is_empty());
    }

    #[test]
    fn follows_dependencies_transitively() {
        let plan = Plan::new().task(1, "u", &[]).task(2, "u", &[1]).task(3, "u", &[2]);
        assert!(plan.state.depends_on("3", "1"));
        assert!(!plan.state.depends_on("1", "3"));
        assert_eq!(plan.state.dependents("1").iter().map(|t| t.number).collect::<Vec<_>>(), [2]);
    }
}
