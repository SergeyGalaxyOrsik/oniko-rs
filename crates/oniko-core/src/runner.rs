//! Puts an agent on a task: a task entering a "Not started" status gets its own worktree and a thread
//! there, moves to the first "In progress" status, and the user hears from it by notification.

use std::path::{Path, PathBuf};

use crate::core::{Core, Git, TaskNotice};
use crate::model::config::new_thread_config;
use crate::model::{RunInfo, StatusKind, Task, TaskRun, ThreadEvent, ThreadEventBody, ThreadStatus};
use crate::stores::tasks::Entered;
use crate::util::{basename, resolve};

pub fn task_prompt(task: &Task, branch: &str) -> String {
    [
        format!("{}: {}", task.key(), task.title),
        task.description.clone(),
        format!(
            "You are working in a git worktree of {} on the branch `{branch}`, made for this task alone. \
             When the work is done, commit it on this branch with a clear message.",
            task.project
        ),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

pub fn pull_request_prompt(branch: &str) -> String {
    format!(
        "Commit anything left uncommitted, push `{branch}` to origin and open a pull request for it with `gh pr create`, \
         with a title and description that explain the change. Reply with the pull request URL."
    )
}

/// Reuses the task's worktree from an earlier run; refuses a folder that is not one of this repo's worktrees.
pub async fn worktree_for(git: Git, project: String, root: PathBuf, branch: String) -> Result<String, String> {
    let parent = root.join(basename(&project));
    let worktree = parent.join(&branch);
    let worktree_text = worktree.to_string_lossy().into_owned();
    git(project.clone(), vec!["rev-parse".into(), "--is-inside-work-tree".into()])
        .await
        .map_err(|_| format!("{project} is not a git repository"))?;
    if worktree.exists() {
        let listed = git(project.clone(), vec!["worktree".into(), "list".into(), "--porcelain".into()]).await?;
        // Git may print the canonical path, which differs where a parent is a symlink (macOS's /var).
        let here: Vec<PathBuf> = [Some(resolve(&worktree)), std::fs::canonicalize(&worktree).ok()].into_iter().flatten().collect();
        let known = listed.lines().filter_map(|line| line.strip_prefix("worktree ")).any(|path| here.contains(&resolve(Path::new(path))));
        if known {
            return Ok(worktree_text);
        }
        return Err(format!("{worktree_text} already exists and is not a worktree of {project}"));
    }
    std::fs::create_dir_all(&parent).map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    let has_branch = !git(project.clone(), vec!["branch".into(), "--list".into(), branch.clone()]).await?.trim().is_empty();
    let args = if has_branch {
        vec!["worktree".into(), "add".into(), worktree_text.clone(), branch]
    } else {
        vec!["worktree".into(), "add".into(), "-b".into(), branch, worktree_text.clone()]
    };
    git(project, args).await?;
    Ok(worktree_text)
}

impl Core {
    /// A start the app quit in the middle of never finished; it is retried like a failure.
    pub(crate) fn fail_interrupted_starts(&mut self) {
        let interrupted: Vec<String> =
            self.tasks.state().tasks.iter().filter(|t| t.run == Some(TaskRun::Starting)).map(|t| t.id.clone()).collect();
        for id in interrupted {
            self.set_run(&id, Some(TaskRun::Failed { error: "Oniko quit before the agent started".into() }));
        }
    }

    fn set_run(&mut self, id: &str, run: Option<TaskRun>) {
        match self.tasks.set_run(id, run) {
            Ok(()) => self.publish_tasks(),
            Err(error) => eprintln!("oniko: could not save the run of task {id}: {error:#}"),
        }
    }

    pub(crate) fn task_entered(&mut self, (task, status): Entered) {
        let idle = task.run.as_ref().is_none_or(|run| matches!(run, TaskRun::Failed { .. }));
        if status.kind == StatusKind::Unstarted && idle {
            self.start_task(&task.id);
        }
    }

    fn start_task(&mut self, id: &str) {
        self.set_run(id, Some(TaskRun::Starting));
        let Some(task) = self.tasks.get(id).cloned() else { return };
        let assigned = task.subscription_id.as_deref().and_then(|s| self.subscriptions.get(s));
        let Some(subscription) = assigned.or(self.subscriptions.active()).cloned() else {
            return self.set_run(id, Some(TaskRun::Failed { error: "Add a subscription in Settings".into() }));
        };
        let me = self.me.clone();
        let id = id.to_owned();
        let work = worktree_for(self.git.clone(), task.project.clone(), self.worktree_root.clone(), task.branch());
        self.me.runtime.spawn(async move {
            let result = work.await;
            me.send(move |core| core.worktree_ready(&id, &subscription.id, result));
        });
    }

    fn worktree_ready(&mut self, id: &str, subscription_id: &str, result: Result<String, String>) {
        // The task may have been deleted while git ran.
        let Some(task) = self.tasks.get(id).cloned() else { return };
        let worktree = match result {
            Ok(worktree) => worktree,
            Err(error) => return self.set_run(id, Some(TaskRun::Failed { error })),
        };
        let Some(subscription) = self.subscriptions.get(subscription_id).cloned() else {
            return self.set_run(id, Some(TaskRun::Failed { error: "The subscription was removed".into() }));
        };
        let config = new_thread_config(self.threads.values(), subscription.agent_id, &subscription.id).into_iter().map(|c| (c.id, c.value)).collect();
        let branch = task.branch();
        let thread_id = match self.create_thread(&subscription, worktree.clone(), config, format!("{}: {}", task.key(), task.title)) {
            Ok(thread_id) => thread_id,
            Err(error) => return self.set_run(id, Some(TaskRun::Failed { error })),
        };
        self.set_run(id, Some(TaskRun::Working(RunInfo { thread_id: thread_id.clone(), worktree, branch: branch.clone() })));
        let started = self.tasks.state().project_statuses(&task.project).into_iter().find(|s| s.kind == StatusKind::Started).map(|s| s.id.clone());
        if let Some(started) = started
            && let Ok((changed, _)) = self.tasks.move_task(id, &started, None)
            && changed
        {
            self.publish_tasks();
        }
        if let Ok(session) = self.session(&thread_id) {
            session.prompt(task_prompt(&task, &branch));
        }
    }

    /// Follows a task's thread: asks for the pull request after the first turn, and notifies on what needs the user.
    pub(crate) fn on_task_thread_event(&mut self, event: &ThreadEvent) {
        let Some(task) = self.tasks.state().task_for_thread(&event.thread_id).cloned() else { return };
        let Some(run) = task.run.clone() else { return };
        let Some(info) = run.info().cloned() else { return };
        let key = task.key();
        let notify = |core: &Core, title: String, body: String| (core.notify)(TaskNotice { title, body, thread_id: info.thread_id.clone() });
        match &event.body {
            // The worktree stays on disk; entering "Not started" again starts a fresh thread in it.
            ThreadEventBody::ThreadRemoved => self.set_run(&task.id, None),
            ThreadEventBody::PermissionRequest { tool_call, .. } => {
                notify(self, format!("{key} needs your approval"), tool_call.fields.title.clone().unwrap_or_else(|| task.title.clone()));
            }
            ThreadEventBody::Status(ThreadStatus::Error { message }) => notify(self, format!("{key}: the agent failed"), message.clone()),
            ThreadEventBody::TurnEnd { stop_reason, .. } if stop_reason != "cancelled" => {
                if matches!(run, TaskRun::Working(_)) && task.pull_request {
                    self.set_run(&task.id, Some(TaskRun::PullRequest(info.clone())));
                    notify(self, format!("{key}: work done, opening a pull request"), task.title.clone());
                    if let Ok(session) = self.session(&info.thread_id) {
                        session.prompt(pull_request_prompt(&info.branch));
                    }
                    return;
                }
                let was_pull_request = matches!(run, TaskRun::PullRequest(_));
                if !matches!(run, TaskRun::Finished(_)) {
                    self.set_run(&task.id, Some(TaskRun::Finished(info.clone())));
                }
                let title = if was_pull_request { format!("{key}: pull request step done") } else { format!("{key}: the agent finished") };
                notify(self, title, task.title.clone());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::core::testing::{Call, Harness};
    use crate::stores::tasks::TaskPatch;

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
    }

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
    }

    fn real_git() -> Harness {
        Harness::new(|cwd, args| {
            let out = Command::new("git").args(args).current_dir(cwd).output().map_err(|e| e.to_string())?;
            if out.status.success() { Ok(String::from_utf8_lossy(&out.stdout).into_owned()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()) }
        })
    }

    struct Board {
        project: String,
        backlog: String,
        todo: String,
        in_progress: String,
    }

    fn board(h: &mut Harness, project: &Path) -> Board {
        let project = project.to_string_lossy().into_owned();
        h.core.tasks.open(&project).unwrap();
        let ids: Vec<String> = h.core.tasks.state().project_statuses(&project).iter().map(|s| s.id.clone()).collect();
        Board { project, backlog: ids[0].clone(), todo: ids[1].clone(), in_progress: ids[2].clone() }
    }

    fn create(h: &mut Harness, b: &Board, title: &str, description: &str, status: &str) -> String {
        let (id, entered) = h.core.tasks.create(&b.project, title, description, status).unwrap();
        h.core.task_entered(entered);
        id
    }

    fn move_to(h: &mut Harness, id: &str, status: &str) {
        let (_, entered) = h.core.tasks.move_task(id, status, None).unwrap();
        if let Some(entered) = entered {
            h.core.task_entered(entered);
        }
    }

    fn run(h: &Harness, id: &str) -> Option<TaskRun> {
        h.core.tasks.get(id).unwrap().run.clone()
    }

    fn thread_of(h: &Harness, id: &str) -> String {
        h.core.tasks.get(id).unwrap().thread_id().unwrap().clone()
    }

    fn turn_end(h: &mut Harness, thread: &str, stop_reason: &str) {
        let event = ThreadEvent { thread_id: thread.into(), body: ThreadEventBody::TurnEnd { stop_reason: stop_reason.into(), duration_ms: 1 } };
        h.core.dispatch(event);
    }

    fn prompts(h: &Harness) -> Vec<String> {
        h.calls().into_iter().filter_map(|c| if let Call::Prompt(_, text) = c { Some(text) } else { None }).collect()
    }

    #[tokio::test]
    async fn leaves_tasks_alone_outside_not_started() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Fix login", "", &b.backlog);
        move_to(&mut h, &id, &b.in_progress);
        h.settle().await;
        assert_eq!(run(&h, &id), None);
        assert!(h.calls().is_empty());
    }

    #[tokio::test]
    async fn makes_a_worktree_starts_a_thread_and_moves_to_in_progress() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Fix the login form", "It rejects valid emails.", &b.backlog);
        move_to(&mut h, &id, &b.todo);
        h.settle().await;
        let worktree = h.dir.path().join("worktrees").join("app").join("app-1-fix-the-login-form");
        let task = h.core.tasks.get(&id).unwrap().clone();
        let Some(TaskRun::Working(info)) = &task.run else { panic!("not working: {:?}", task.run) };
        assert_eq!((info.worktree.as_str(), info.branch.as_str()), (worktree.to_str().unwrap(), "app-1-fix-the-login-form"));
        assert_eq!(task.status_id, b.in_progress);
        let thread = &h.core.threads[&info.thread_id];
        assert_eq!(thread.title, "APP-1: Fix the login form");
        assert_eq!(thread.cwd, worktree.to_str().unwrap());
        let branch = Command::new("git").args(["branch", "--show-current"]).current_dir(&worktree).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "app-1-fix-the-login-form");
        assert!(prompts(&h)[0].starts_with("APP-1: Fix the login form\n\nIt rejects valid emails.\n\n"));
        let error = h.core.tasks.update(&id, TaskPatch { subscription_id: Some(Some("other".into())), ..Default::default() }).unwrap_err();
        assert!(error.to_string().contains("already started"));
    }

    #[tokio::test]
    async fn records_a_failure_outside_a_git_repo_and_retries_when_moved_back() {
        let mut h = real_git();
        let plain = h.dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let b = board(&mut h, &plain);
        let id = create(&mut h, &b, "Anything", "", &b.todo);
        h.settle().await;
        assert_eq!(run(&h, &id), Some(TaskRun::Failed { error: format!("{} is not a git repository", plain.display()) }));
        init_repo(&plain);
        move_to(&mut h, &id, &b.backlog);
        move_to(&mut h, &id, &b.todo);
        h.settle().await;
        assert!(matches!(run(&h, &id), Some(TaskRun::Working(_))));
    }

    #[tokio::test]
    async fn asks_for_a_pull_request_after_the_first_turn_then_notifies() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Ship it", "", &b.backlog);
        h.core.tasks.update(&id, TaskPatch { pull_request: Some(true), ..Default::default() }).unwrap();
        move_to(&mut h, &id, &b.todo);
        h.settle().await;
        let thread = thread_of(&h, &id);
        turn_end(&mut h, &thread, "end_turn");
        assert!(matches!(run(&h, &id), Some(TaskRun::PullRequest(_))));
        assert!(prompts(&h).last().unwrap().contains("gh pr create"));
        turn_end(&mut h, &thread, "end_turn");
        assert!(matches!(run(&h, &id), Some(TaskRun::Finished(_))));
        let titles: Vec<String> = h.notices().into_iter().map(|n| n.title).collect();
        assert_eq!(titles, ["APP-1: work done, opening a pull request", "APP-1: pull request step done"]);
    }

    #[tokio::test]
    async fn finishes_without_a_pull_request_notifies_on_approvals_and_ignores_a_cancelled_turn() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Small fix", "", &b.todo);
        h.settle().await;
        let thread = thread_of(&h, &id);
        let tool_call = serde_json::from_value(serde_json::json!({ "toolCallId": "c", "title": "Run pnpm test" })).unwrap();
        h.core.dispatch(ThreadEvent {
            thread_id: thread.clone(),
            body: ThreadEventBody::PermissionRequest { request_id: "r".into(), tool_call: Box::new(tool_call), options: vec![] },
        });
        turn_end(&mut h, &thread, "cancelled");
        assert!(matches!(run(&h, &id), Some(TaskRun::Working(_))));
        turn_end(&mut h, &thread, "end_turn");
        assert!(matches!(run(&h, &id), Some(TaskRun::Finished(_))));
        assert_eq!(prompts(&h).len(), 1);
        assert_eq!(
            h.notices(),
            [
                TaskNotice { title: "APP-1 needs your approval".into(), body: "Run pnpm test".into(), thread_id: thread.clone() },
                TaskNotice { title: "APP-1: the agent finished".into(), body: "Small fix".into(), thread_id: thread },
            ]
        );
    }

    #[tokio::test]
    async fn reuses_the_worktree_for_a_fresh_thread_after_the_old_one_is_removed() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Again", "", &b.todo);
        h.settle().await;
        let first = thread_of(&h, &id);
        let first_cwd = h.core.threads[&first].cwd.clone();
        h.core.remove_thread(&first);
        assert_eq!(run(&h, &id), None);
        move_to(&mut h, &id, &b.backlog);
        move_to(&mut h, &id, &b.todo);
        h.settle().await;
        let second = thread_of(&h, &id);
        assert_ne!(first, second);
        assert_eq!(h.core.threads[&second].cwd, first_cwd);
        assert!(Path::new(&first_cwd).exists());
    }

    #[tokio::test]
    async fn turns_a_start_interrupted_by_quitting_into_a_failure() {
        let mut h = real_git();
        let b = board(&mut h, Path::new("/tmp/anything"));
        let (id, _) = h.core.tasks.create(&b.project, "Interrupted", "", &b.backlog).unwrap();
        h.core.tasks.set_run(&id, Some(TaskRun::Starting)).unwrap();
        h.core.fail_interrupted_starts();
        assert_eq!(run(&h, &id), Some(TaskRun::Failed { error: "Oniko quit before the agent started".into() }));
    }
}
