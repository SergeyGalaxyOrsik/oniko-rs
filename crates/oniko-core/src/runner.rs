//! Puts an agent on a task: the scheduler starts each queued task whose blockers are done, in its own
//! worktree branched off the freshly fetched base branch, moves it to the first "In progress" status,
//! and the user hears from it by notification.

use std::path::{Path, PathBuf};

use crate::core::{Cli, Core, TaskNotice};
use crate::model::config::new_thread_config;
use crate::model::{ConflictFix, RunInfo, StatusKind, Subscription, Task, TaskRun, ThreadEvent, ThreadEventBody, ThreadStatus};
use crate::stores::tasks::Entered;
use crate::util::{basename, resolve};

/// The Claude adapter's permission mode where a classifier approves commands instead of the user.
/// On a model without auto mode the adapter falls back to accepting edits and asks for the rest.
const TASK_MODE: (&str, &str) = ("mode", "auto");

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
        "Commit anything left uncommitted. Then run `git fetch origin`, rebase `{branch}` onto origin's default branch, \
         resolve any conflicts and re-run the project's checks. Only then push `{branch}` to origin and open a pull request \
         for it with `gh pr create`, with a title and description that explain the change."
    )
}

pub fn conflict_prompt(branch: &str) -> String {
    format!(
        "The pull request for `{branch}` now conflicts with origin's default branch. Run `git fetch origin`, rebase `{branch}` \
         onto it, resolve the conflicts, re-run the project's checks, then push with `git push --force-with-lease`."
    )
}

pub(crate) fn argv<const N: usize>(parts: [&str; N]) -> Vec<String> {
    parts.into_iter().map(str::to_owned).collect()
}

/// `<root>/<project folder>/<branch>`: the same every time, so a parked worktree comes back where it was.
pub fn worktree_path(root: &Path, project: &str, branch: &str) -> PathBuf {
    root.join(basename(project)).join(branch)
}

/// Fetches origin's default branch and returns it as the start point for new branches; a repo
/// without `origin` starts from its HEAD. Only the remote-tracking ref moves, never the user's checkout.
pub async fn fetch_base(git: &Cli, project: &str) -> Result<String, String> {
    let remotes = git(project.to_owned(), argv(["remote"])).await?;
    if !remotes.lines().any(|remote| remote.trim() == "origin") {
        return Ok("HEAD".into());
    }
    let base = git(project.to_owned(), argv(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]))
        .await
        .map(|out| out.trim().to_owned())
        .unwrap_or_else(|_| "origin/main".into());
    let name = base.strip_prefix("origin/").unwrap_or(&base);
    git(project.to_owned(), argv(["fetch", "origin", name])).await.map_err(|e| format!("Could not fetch {base}: {e}"))?;
    Ok(base)
}

/// Reuses the task's worktree from an earlier run, else branches off the base; refuses a folder that
/// is not one of this repo's worktrees.
pub async fn worktree_for(git: Cli, project: String, worktree: PathBuf, branch: String) -> Result<String, String> {
    let worktree_text = worktree.to_string_lossy().into_owned();
    git(project.clone(), argv(["rev-parse", "--is-inside-work-tree"])).await.map_err(|_| format!("{project} is not a git repository"))?;
    if worktree.exists() {
        let listed = git(project.clone(), argv(["worktree", "list", "--porcelain"])).await?;
        // Git may print the canonical path, which differs where a parent is a symlink (macOS's /var).
        let here: Vec<PathBuf> = [Some(resolve(&worktree)), std::fs::canonicalize(&worktree).ok()].into_iter().flatten().collect();
        let known = listed.lines().filter_map(|line| line.strip_prefix("worktree ")).any(|path| here.contains(&resolve(Path::new(path))));
        if known {
            return Ok(worktree_text);
        }
        return Err(format!("{worktree_text} already exists and is not a worktree of {project}"));
    }
    if let Some(parent) = worktree.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    // A folder removed behind git's back stays registered and would block adding it again.
    git(project.clone(), argv(["worktree", "prune"])).await?;
    let has_branch = !git(project.clone(), argv(["branch", "--list", &branch])).await?.trim().is_empty();
    let args = if has_branch {
        argv(["worktree", "add", &worktree_text, &branch])
    } else {
        let base = fetch_base(&git, &project).await?;
        argv(["worktree", "add", "-b", &branch, &worktree_text, &base])
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

    pub(crate) fn set_run(&mut self, id: &str, run: Option<TaskRun>) {
        match self.tasks.set_run(id, run) {
            Ok(()) => self.publish_tasks(),
            Err(error) => eprintln!("oniko: could not save the run of task {id}: {error:#}"),
        }
    }

    /// Moves the task to the project's first status of `kind`; a project without one leaves it where it is.
    pub(crate) fn move_to_first(&mut self, id: &str, kind: StatusKind) {
        let Some(task) = self.tasks.get(id) else { return };
        let target = self.tasks.state().project_statuses(&task.project).into_iter().find(|s| s.kind == kind).map(|s| s.id.clone());
        if let Some(target) = target
            && let Ok((true, _)) = self.tasks.move_task(id, &target, None)
        {
            self.publish_tasks();
        }
    }

    pub(crate) fn notify_task(&self, thread_id: &str, title: String, body: String) {
        (self.notify)(TaskNotice { title, body, thread_id: thread_id.to_owned() });
    }

    /// Entering "Not started" again clears a failed run, so the scheduler retries it.
    pub(crate) fn task_entered(&mut self, (task, status): Entered) {
        if status.kind == StatusKind::Unstarted && matches!(task.run, Some(TaskRun::Failed { .. })) {
            self.set_run(&task.id, None);
        }
    }

    /// Starts every task the plan says is ready. Safe to call any time: a started task is no longer ready.
    pub(crate) fn schedule(&mut self) {
        for id in self.tasks.state().ready_to_start() {
            self.start_task(&id);
        }
    }

    fn start_task(&mut self, id: &str) {
        self.set_run(id, Some(TaskRun::Starting));
        let Some(task) = self.tasks.get(id).cloned() else { return };
        let assigned = task.subscription_id.as_deref().and_then(|s| self.subscriptions.get(s));
        let Some(subscription) = assigned.or(self.subscriptions.active()).cloned() else {
            return self.set_run(id, Some(TaskRun::Failed { error: "Add a subscription in Settings".into() }));
        };
        let config = match self.initial_config(&task, &subscription) {
            Ok(config) => config,
            Err(error) => return self.set_run(id, Some(TaskRun::Failed { error })),
        };
        let branch = task.branch();
        let work = worktree_for(self.git.clone(), task.project.clone(), worktree_path(&self.worktree_root, &task.project, &branch), branch.clone());
        let id = id.to_owned();
        self.with_git_lock(&task.project, work, move |core, result| core.worktree_ready(&id, &subscription.id, branch, config, result));
    }

    /// The agent's last known values with the task's model in place, in auto mode so nobody has to approve
    /// each command. A model the agent is known not to offer fails the run: the session would silently
    /// keep another one.
    fn initial_config(&self, task: &Task, subscription: &Subscription) -> Result<Vec<(String, String)>, String> {
        let known = new_thread_config(self.threads.values(), subscription.agent_id, &subscription.id);
        let mut config: Vec<(String, String)> = known.iter().filter(|c| c.id != TASK_MODE.0).map(|c| (c.id.clone(), c.value.clone())).collect();
        if let Some(model) = &task.model {
            if let Some(control) = known.iter().find(|c| c.id == "model")
                && !control.options.iter().any(|o| o.value == *model)
            {
                return Err(format!("The agent does not offer the model {model}"));
            }
            match config.iter_mut().find(|(id, _)| id == "model") {
                Some(entry) => entry.1 = model.clone(),
                None => config.push(("model".into(), model.clone())),
            }
        }
        // Last, because the session applies these in order and the adapter drops auto mode for a model
        // that does not support it; set after the model, the adapter judges it against the final one.
        config.push((TASK_MODE.0.into(), TASK_MODE.1.into()));
        Ok(config)
    }

    fn worktree_ready(&mut self, id: &str, subscription_id: &str, branch: String, config: Vec<(String, String)>, result: Result<String, String>) {
        // The task may have been deleted while git ran.
        let Some(task) = self.tasks.get(id).cloned() else { return };
        let worktree = match result {
            Ok(worktree) => worktree,
            Err(error) => return self.set_run(id, Some(TaskRun::Failed { error })),
        };
        let Some(subscription) = self.subscriptions.get(subscription_id).cloned() else {
            return self.set_run(id, Some(TaskRun::Failed { error: "The subscription was removed".into() }));
        };
        let thread_id = self.create_thread(&subscription, worktree.clone(), format!("{}: {}", task.key(), task.title));
        // Recorded before the session exists, which looks the task up for its env.
        self.set_run(id, Some(TaskRun::Working(RunInfo { thread_id: thread_id.clone(), worktree, branch: branch.clone() })));
        self.move_to_first(id, StatusKind::Started);
        match self.session_with(&thread_id, config) {
            Ok(session) => session.prompt(task_prompt(&task, &branch)),
            Err(error) => self.set_run(id, Some(TaskRun::Failed { error })),
        }
    }

    /// Follows a task's thread: asks for the pull request after the first turn, and notifies on what needs the user.
    pub(crate) fn on_task_thread_event(&mut self, event: &ThreadEvent) {
        let Some(task) = self.tasks.state().task_for_thread(&event.thread_id).cloned() else { return };
        let Some(run) = task.run.clone() else { return };
        let key = task.key();
        let thread_id = event.thread_id.as_str();
        match &event.body {
            // GitHub still tracks a run under review or merged, so that outlives its thread. Otherwise the
            // worktree stays on disk and entering "Not started" again starts a fresh thread in it.
            ThreadEventBody::ThreadRemoved => {
                if !matches!(run, TaskRun::Review(_) | TaskRun::Merged(_)) {
                    self.set_run(&task.id, None);
                }
            }
            ThreadEventBody::PermissionRequest { tool_call, .. } => {
                self.notify_task(thread_id, format!("{key} needs your approval"), tool_call.fields.title.clone().unwrap_or_else(|| task.title.clone()));
            }
            ThreadEventBody::Status(ThreadStatus::Error { message }) => self.notify_task(thread_id, format!("{key}: the agent failed"), message.clone()),
            ThreadEventBody::TurnEnd { stop_reason, .. } if stop_reason != "cancelled" => match run {
                TaskRun::Working(info) if task.pull_request => {
                    self.set_run(&task.id, Some(TaskRun::PullRequest(info.clone())));
                    self.notify_task(thread_id, format!("{key}: work done, opening a pull request"), task.title.clone());
                    if let Ok(session) = self.session(&info.thread_id) {
                        session.prompt(pull_request_prompt(&info.branch));
                    }
                }
                TaskRun::Working(info) => {
                    self.set_run(&task.id, Some(TaskRun::Finished(info)));
                    self.notify_task(thread_id, format!("{key}: the agent finished"), task.title.clone());
                }
                TaskRun::Finished(_) => self.notify_task(thread_id, format!("{key}: the agent finished"), task.title.clone()),
                TaskRun::PullRequest(info) => self.find_pull_request(&task, info),
                TaskRun::Review(review) if review.conflict_fix == ConflictFix::Fixing => self.conflict_turn_ended(&task, review),
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::core::testing::{Call, Harness};
    use crate::model::{ConfigChoice, ConfigControl, ConfigSource, Mergeable, Merged, PrState, PullRequest, Thread};
    use crate::stores::tasks::TaskPatch;

    fn git(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git").args(["-c", "user.email=t@example.com", "-c", "user.name=t"]).args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["commit", "-q", "--allow-empty", "-m", "init"]);
    }

    /// A bare `origin` whose default branch is `branch`, and a second clone to push to it as GitHub would.
    fn with_origin(h: &Harness, repo: &Path, branch: &str) -> PathBuf {
        let origin = h.dir.path().join("origin.git");
        git(h.dir.path(), &["init", "-q", "--bare", "-b", branch, origin.to_str().unwrap()]);
        git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
        git(repo, &["push", "-q", "origin", &format!("HEAD:{branch}")]);
        git(repo, &["remote", "set-head", "origin", branch]);
        let upstream = h.dir.path().join("upstream");
        git(h.dir.path(), &["clone", "-q", origin.to_str().unwrap(), upstream.to_str().unwrap()]);
        upstream
    }

    fn push_commit(upstream: &Path, branch: &str, message: &str) -> String {
        git(upstream, &["commit", "-q", "--allow-empty", "-m", message]);
        git(upstream, &["push", "-q", "origin", &format!("HEAD:{branch}")]);
        git(upstream, &["rev-parse", "HEAD"])
    }

    fn contains(worktree: &str, commit: &str) -> bool {
        Command::new("git").args(["merge-base", "--is-ancestor", commit, "HEAD"]).current_dir(worktree).status().unwrap().success()
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
        in_review: String,
        done: String,
    }

    fn board(h: &mut Harness, project: &Path) -> Board {
        let project = project.to_string_lossy().into_owned();
        h.core.tasks.open(&project).unwrap();
        let ids: Vec<String> = h.core.tasks.state().project_statuses(&project).iter().map(|s| s.id.clone()).collect();
        Board { project, backlog: ids[0].clone(), todo: ids[1].clone(), in_progress: ids[2].clone(), in_review: ids[3].clone(), done: ids[4].clone() }
    }

    fn create(h: &mut Harness, b: &Board, title: &str, description: &str, status: &str) -> String {
        let (id, entered) = h.core.tasks.create(&b.project, title, description, status).unwrap();
        h.core.task_entered(entered);
        h.core.schedule();
        id
    }

    fn move_to(h: &mut Harness, id: &str, status: &str) {
        h.core.move_task(id, status, None).unwrap();
    }

    fn task(h: &Harness, id: &str) -> Task {
        h.core.tasks.get(id).unwrap().clone()
    }

    fn run(h: &Harness, id: &str) -> Option<TaskRun> {
        task(h, id).run
    }

    fn thread_of(h: &Harness, id: &str) -> String {
        task(h, id).thread_id().unwrap().clone()
    }

    fn turn_end(h: &mut Harness, thread: &str, stop_reason: &str) {
        let event = ThreadEvent { thread_id: thread.into(), body: ThreadEventBody::TurnEnd { stop_reason: stop_reason.into(), duration_ms: 1 } };
        h.core.dispatch(event);
    }

    fn prompts(h: &Harness) -> Vec<String> {
        h.calls().into_iter().filter_map(|c| if let Call::Prompt(_, text) = c { Some(text) } else { None }).collect()
    }

    fn notice_titles(h: &Harness) -> Vec<String> {
        h.notices().into_iter().map(|n| n.title).collect()
    }

    fn github_says(h: &Harness, prs: &[(&str, &str, &str)]) {
        let listed: Vec<serde_json::Value> = prs
            .iter()
            .enumerate()
            .map(|(i, (branch, state, mergeable))| {
                serde_json::json!({ "number": i + 1, "url": format!("https://github.com/o/app/pull/{}", i + 1), "state": state, "headRefName": branch, "mergeable": mergeable })
            })
            .collect();
        *h.gh.lock().unwrap() = Ok(serde_json::to_string(&listed).unwrap());
    }

    fn review(h: &Harness, id: &str) -> crate::model::Review {
        match run(h, id) {
            Some(TaskRun::Review(review)) => review,
            other => panic!("not in review: {other:?}"),
        }
    }

    /// Starts a pull request task and walks it into review with a clean worktree.
    async fn into_review(h: &mut Harness, b: &Board, title: &str) -> (String, String, PathBuf) {
        let id = create(h, b, title, "", &b.backlog);
        h.core.tasks.update(&id, TaskPatch { pull_request: Some(true), ..Default::default() }).unwrap();
        move_to(h, &id, &b.todo);
        h.settle().await;
        let thread = thread_of(h, &id);
        let branch = run(h, &id).unwrap().branch().unwrap().to_owned();
        let worktree = PathBuf::from(run(h, &id).unwrap().worktree().unwrap());
        turn_end(h, &thread, "end_turn");
        github_says(h, &[(&branch, "OPEN", "MERGEABLE")]);
        turn_end(h, &thread, "end_turn");
        h.settle().await;
        (id, thread, worktree)
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
        let task = task(&h, &id);
        let Some(TaskRun::Working(info)) = &task.run else { panic!("not working: {:?}", task.run) };
        assert_eq!((info.worktree.as_str(), info.branch.as_str()), (worktree.to_str().unwrap(), "app-1-fix-the-login-form"));
        assert_eq!(task.status_id, b.in_progress);
        let thread = &h.core.threads[&info.thread_id];
        assert_eq!(thread.title, "APP-1: Fix the login form");
        assert_eq!(thread.cwd, worktree.to_str().unwrap());
        assert_eq!(git(&worktree, &["branch", "--show-current"]), "app-1-fix-the-login-form");
        assert!(prompts(&h)[0].starts_with("APP-1: Fix the login form\n\nIt rejects valid emails.\n\n"));
        let error = h.core.tasks.update(&id, TaskPatch { subscription_id: Some(Some("other".into())), ..Default::default() }).unwrap_err();
        assert!(error.to_string().contains("already started"));
    }

    #[tokio::test]
    async fn starts_independent_tasks_together() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let ids: Vec<String> = ["One", "Two", "Three"].iter().map(|title| create(&mut h, &b, title, "", &b.todo)).collect();
        h.settle().await;
        for id in &ids {
            assert!(matches!(run(&h, id), Some(TaskRun::Working(_))), "{:?}", run(&h, id));
        }
        assert_eq!(prompts(&h).len(), 3);
    }

    #[tokio::test]
    async fn branches_off_the_fetched_remote_default_and_leaves_the_project_checkout_alone() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        git(&repo, &["branch", "-m", "main", "trunk"]);
        let upstream = with_origin(&h, &repo, "trunk");
        let landed = push_commit(&upstream, "trunk", "landed elsewhere");
        git(&repo, &["checkout", "-q", "-b", "my-own-work"]);
        std::fs::write(repo.join("draft.txt"), "unsaved").unwrap();
        let head_before = git(&repo, &["rev-parse", "HEAD"]);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Build on trunk", "", &b.todo);
        h.settle().await;
        let worktree = run(&h, &id).unwrap().worktree().unwrap().to_owned();
        assert!(contains(&worktree, &landed), "the worktree lacks origin/trunk's latest commit");
        assert_eq!(git(&repo, &["branch", "--show-current"]), "my-own-work");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head_before);
        assert!(!contains(repo.to_str().unwrap(), &landed));
        assert_eq!(git(&repo, &["status", "--porcelain"]), "?? draft.txt");
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
    async fn fails_when_the_agent_does_not_offer_the_tasks_model() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let subscription = h.core.subscriptions.active().unwrap().clone();
        let choice = |value: &str| ConfigChoice { value: value.into(), label: value.into(), description: None };
        let known = Thread {
            id: "earlier".into(),
            agent_id: subscription.agent_id,
            subscription_id: subscription.id.clone(),
            cwd: "/elsewhere".into(),
            title: "earlier".into(),
            created_at: 1,
            acp_session_id: None,
            status: ThreadStatus::Idle,
            timeline: vec![],
            config: vec![ConfigControl {
                id: "model".into(),
                label: "Model".into(),
                category: Some("model".into()),
                source: ConfigSource::ConfigOption,
                value: "sonnet".into(),
                options: vec![choice("sonnet"), choice("opus")],
            }],
            commands: vec![],
        };
        h.core.dispatch(ThreadEvent { thread_id: known.id.clone(), body: ThreadEventBody::ThreadCreated(Box::new(known)) });
        let missing = create(&mut h, &b, "Too fancy", "", &b.backlog);
        h.core.tasks.update(&missing, TaskPatch { model: Some(Some("gpt-9".into())), ..Default::default() }).unwrap();
        let offered = create(&mut h, &b, "Just right", "", &b.backlog);
        h.core.tasks.update(&offered, TaskPatch { model: Some(Some("opus".into())), ..Default::default() }).unwrap();
        move_to(&mut h, &missing, &b.todo);
        move_to(&mut h, &offered, &b.todo);
        h.settle().await;
        let Some(TaskRun::Failed { error }) = run(&h, &missing) else { panic!("{:?}", run(&h, &missing)) };
        assert!(error.contains("gpt-9"), "{error}");
        assert!(!h.dir.path().join("worktrees/app/app-1-too-fancy").exists());
        let worktree = run(&h, &offered).unwrap().worktree().unwrap().to_owned();
        let spawned = h.spawned.lock().unwrap().iter().find(|s| s.cwd == worktree).cloned().unwrap();
        assert_eq!(spawned.initial_config, [("model".to_owned(), "opus".to_owned()), ("mode".to_owned(), "auto".to_owned())]);
    }

    #[tokio::test]
    async fn a_task_thread_shares_the_projects_cargo_target_and_a_composer_thread_does_not() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Build it", "", &b.todo);
        h.settle().await;
        let worktree = run(&h, &id).unwrap().worktree().unwrap().to_owned();
        h.core.create_thread_command(repo.to_string_lossy().into_owned(), None, vec![]).unwrap();
        let spawned = h.spawned.lock().unwrap().clone();
        let env_of = |cwd: &str| spawned.iter().find(|s| s.cwd == cwd).unwrap().env.get("CARGO_TARGET_DIR").cloned();
        assert_eq!(env_of(&worktree), Some(Some(repo.join("target").to_string_lossy().into_owned())));
        assert_eq!(env_of(repo.to_str().unwrap()), None);
    }

    #[tokio::test]
    async fn fails_when_the_agent_did_not_open_a_pull_request() {
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
        let asked = prompts(&h).last().unwrap().clone();
        assert!(asked.contains("rebase") && asked.contains("gh pr create"), "{asked}");
        turn_end(&mut h, &thread, "end_turn");
        h.settle().await;
        assert_eq!(run(&h, &id), Some(TaskRun::Failed { error: "The agent did not open a pull request".into() }));
        assert_eq!(notice_titles(&h), ["APP-1: work done, opening a pull request", "APP-1: no pull request"]);
    }

    #[tokio::test]
    async fn parks_a_clean_worktree_in_review_and_hands_a_conflict_to_the_agent_once() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let (id, thread, worktree) = into_review(&mut h, &b, "Ship it").await;
        let parked = review(&h, &id);
        assert_eq!((parked.worktree.as_deref(), parked.pr.state), (None, PrState::Open));
        assert_eq!(task(&h, &id).status_id, b.in_review, "an open pull request moves the task to In review");
        assert!(!worktree.exists(), "a clean worktree is removed while the pull request waits");
        assert!(h.calls().contains(&Call::Dispose(worktree.to_string_lossy().into_owned())));
        assert_eq!(git(&repo, &["branch", "--list", &parked.branch]), format!("{}", parked.branch));

        github_says(&h, &[(&parked.branch, "OPEN", "CONFLICTING")]);
        h.core.check_pull_requests();
        h.settle().await;
        assert!(worktree.exists(), "the worktree comes back at the same path");
        assert_eq!(review(&h, &id).worktree.as_deref(), Some(worktree.to_str().unwrap()));
        let conflict_prompts = |h: &Harness| prompts(h).iter().filter(|p| p.contains("now conflicts")).count();
        assert_eq!(conflict_prompts(&h), 1);
        h.core.check_pull_requests();
        h.settle().await;
        assert_eq!(conflict_prompts(&h), 1, "no second prompt while the agent is on it");

        turn_end(&mut h, &thread, "end_turn");
        h.settle().await;
        assert!(!worktree.exists(), "parked again once the agent's turn ends");
        for _ in 0..3 {
            h.core.check_pull_requests();
            h.settle().await;
        }
        assert_eq!(conflict_prompts(&h), 1);
        let still = notice_titles(&h).into_iter().filter(|t| t.contains("still conflicts")).count();
        assert_eq!(still, 1, "{:?}", notice_titles(&h));

        github_says(&h, &[(&parked.branch, "OPEN", "MERGEABLE")]);
        h.core.check_pull_requests();
        h.settle().await;
        assert_eq!(review(&h, &id).conflict_fix, ConflictFix::Clear);
    }

    #[tokio::test]
    async fn stays_in_progress_on_a_board_without_a_review_status() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        h.core.tasks.remove_status(&b.in_review).unwrap();
        let (id, _, _) = into_review(&mut h, &b, "Ship it").await;
        assert_eq!(task(&h, &id).status_id, b.in_progress);
        assert!(matches!(run(&h, &id), Some(TaskRun::Review(_))));
    }

    #[tokio::test]
    async fn keeps_a_dirty_worktree_and_reports_a_closed_pull_request_once() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Ship it", "", &b.backlog);
        h.core.tasks.update(&id, TaskPatch { pull_request: Some(true), ..Default::default() }).unwrap();
        move_to(&mut h, &id, &b.todo);
        h.settle().await;
        let thread = thread_of(&h, &id);
        let worktree = PathBuf::from(run(&h, &id).unwrap().worktree().unwrap());
        let branch = run(&h, &id).unwrap().branch().unwrap().to_owned();
        std::fs::write(worktree.join("notes.txt"), "left behind").unwrap();
        turn_end(&mut h, &thread, "end_turn");
        github_says(&h, &[(&branch, "OPEN", "UNKNOWN")]);
        turn_end(&mut h, &thread, "end_turn");
        h.settle().await;
        assert!(worktree.exists());
        assert_eq!(review(&h, &id).worktree.as_deref(), Some(worktree.to_str().unwrap()));
        github_says(&h, &[(&branch, "CLOSED", "UNKNOWN")]);
        for _ in 0..2 {
            h.core.check_pull_requests();
            h.settle().await;
        }
        assert_eq!(review(&h, &id).pr.state, PrState::Closed);
        assert_eq!(notice_titles(&h).iter().filter(|t| t.contains("closed without merging")).count(), 1);
    }

    #[tokio::test]
    async fn a_merged_blocker_cleans_up_and_starts_its_dependent_on_the_new_base() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let upstream = with_origin(&h, &repo, "main");
        let b = board(&mut h, &repo);
        let a = create(&mut h, &b, "Schema", "", &b.backlog);
        let next = create(&mut h, &b, "API", "", &b.backlog);
        h.core.tasks.add_dependency(&next, &a).unwrap();
        h.core.run_plan(&b.project).unwrap();
        h.settle().await;
        assert!(matches!(run(&h, &a), Some(TaskRun::Working(_))));
        assert_eq!(run(&h, &next), None);
        assert_eq!(h.core.tasks.state().waiting_reason(&task(&h, &next)).as_deref(), Some("Waiting for APP-1"));
        assert!(task(&h, &next).pull_request, "the last task of a plan opens a pull request too");

        let thread = thread_of(&h, &a);
        let branch = run(&h, &a).unwrap().branch().unwrap().to_owned();
        let worktree = PathBuf::from(run(&h, &a).unwrap().worktree().unwrap());
        turn_end(&mut h, &thread, "end_turn");
        github_says(&h, &[(&branch, "OPEN", "MERGEABLE")]);
        turn_end(&mut h, &thread, "end_turn");
        h.settle().await;
        assert_eq!(review(&h, &a).pr.state, PrState::Open);
        assert_eq!(run(&h, &next), None, "the dependent waits for the merge, not the pull request");

        let merge = push_commit(&upstream, "main", "Merge APP-1");
        github_says(&h, &[(&branch, "MERGED", "UNKNOWN")]);
        h.core.check_pull_requests();
        h.settle().await;
        let Some(TaskRun::Merged(merged)) = run(&h, &a) else { panic!("{:?}", run(&h, &a)) };
        assert_eq!(merged.pr.state, PrState::Merged);
        assert_eq!(task(&h, &a).status_id, b.done);
        assert!(!worktree.exists());
        assert_eq!(git(&repo, &["branch", "--list", &branch]), "");
        assert!(notice_titles(&h).contains(&"APP-1: pull request merged".to_owned()));
        let started = run(&h, &next).and_then(|r| r.worktree().map(str::to_owned)).expect("the dependent started");
        assert!(contains(&started, &merge), "the dependent's worktree lacks the merge");
    }

    #[tokio::test]
    async fn on_startup_cleans_a_merged_task_whose_folder_survived() {
        let mut h = real_git();
        let repo = h.dir.path().join("app");
        init_repo(&repo);
        let b = board(&mut h, &repo);
        let id = create(&mut h, &b, "Leftover", "", &b.todo);
        h.settle().await;
        let run_before = run(&h, &id).unwrap();
        let (worktree, branch) = (PathBuf::from(run_before.worktree().unwrap()), run_before.branch().unwrap().to_owned());
        let pr = PullRequest { number: 1, url: "u".into(), state: PrState::Merged, mergeable: Mergeable::Unknown };
        let merged = Merged { thread_id: thread_of(&h, &id), branch: branch.clone(), pr };
        h.core.tasks.set_run(&id, Some(TaskRun::Merged(merged))).unwrap();
        for _ in 0..2 {
            h.core.resume_reviews();
            h.settle().await;
        }
        assert!(!worktree.exists());
        assert_eq!(git(&repo, &["branch", "--list", &branch]), "");
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
