//! Follows a task's pull request on GitHub, the only place that knows it merged: finds it once the
//! agent says it opened it, parks the worktree while it waits, asks the agent once to fix a conflict,
//! and cleans up after the merge so the tasks waiting on it start from the new base.

use std::time::Duration;

use serde::Deserialize;

use crate::core::Core;
use crate::model::{
    ConflictFix, Mergeable, Merged, PrState, PullRequest, Review, ReviewAction, RunInfo, StatusKind, Task, TaskRun,
};
use crate::runner::{argv, conflict_prompt, fetch_base, worktree_for, worktree_path};

const POLL_EVERY: Duration = Duration::from_secs(30);
const PR_FIELDS: &str = "number,url,state,headRefName,mergeable";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPullRequest {
    number: i64,
    url: String,
    state: String,
    head_ref_name: String,
    mergeable: String,
}

/// `gh pr list --json number,url,state,headRefName,mergeable`, newest first, as (branch, pull request).
pub fn parse_pull_requests(json: &str) -> Result<Vec<(String, PullRequest)>, String> {
    let listed: Vec<GhPullRequest> = serde_json::from_str(json).map_err(|e| format!("Unreadable gh output: {e}"))?;
    Ok(listed
        .into_iter()
        .map(|pr| {
            let state = match pr.state.as_str() {
                "MERGED" => PrState::Merged,
                "CLOSED" => PrState::Closed,
                _ => PrState::Open,
            };
            let mergeable = match pr.mergeable.as_str() {
                "MERGEABLE" => Mergeable::Mergeable,
                "CONFLICTING" => Mergeable::Conflicting,
                _ => Mergeable::Unknown,
            };
            (pr.head_ref_name, PullRequest { number: pr.number, url: pr.url, state, mergeable })
        })
        .collect())
}

fn pull_request_of(listed: Result<String, String>, branch: &str) -> Result<Option<PullRequest>, String> {
    Ok(parse_pull_requests(&listed?)?.into_iter().find(|(head, _)| head == branch).map(|(_, pr)| pr))
}

impl Core {
    fn review_of(&self, id: &str) -> Option<(Task, Review)> {
        let task = self.tasks.get(id)?.clone();
        let Some(TaskRun::Review(review)) = task.run.clone() else { return None };
        Some((task, review))
    }

    /// Saves and publishes the change only when there is one.
    fn update_review(&mut self, id: &str, change: impl FnOnce(&mut Review)) {
        let Some((_, review)) = self.review_of(id) else { return };
        let mut next = review.clone();
        change(&mut next);
        if next != review {
            self.set_run(id, Some(TaskRun::Review(next)));
        }
    }

    /// After the pull request turn ends, GitHub rather than the agent's reply says whether there is one.
    pub(crate) fn find_pull_request(&mut self, task: &Task, info: RunInfo) {
        let (gh, me, id, project) = (self.gh.clone(), self.me.clone(), task.id.clone(), task.project.clone());
        let args = argv(["pr", "list", "--state", "all", "--head", &info.branch, "--json", PR_FIELDS]);
        self.me.runtime.spawn(async move {
            let listed = gh(project, args).await;
            me.send(move |core| core.pull_request_found(&id, info, listed));
        });
    }

    fn pull_request_found(&mut self, id: &str, info: RunInfo, listed: Result<String, String>) {
        let Some(task) = self.tasks.get(id).cloned() else { return };
        if task.run.as_ref() != Some(&TaskRun::PullRequest(info.clone())) {
            return;
        }
        let key = task.key();
        let found = match pull_request_of(listed, &info.branch) {
            Ok(Some(pr)) => Ok(pr),
            Ok(None) => Err("The agent did not open a pull request".to_owned()),
            Err(error) => Err(format!("Could not look up the pull request: {error}")),
        };
        let pr = match found {
            Ok(pr) => pr,
            Err(error) => {
                self.notify_task(&info.thread_id, format!("{key}: no pull request"), error.clone());
                return self.set_run(id, Some(TaskRun::Failed { error }));
            }
        };
        let review = Review { thread_id: info.thread_id, branch: info.branch, worktree: Some(info.worktree), pr, conflict_fix: ConflictFix::Clear };
        self.set_run(id, Some(TaskRun::Review(review.clone())));
        self.move_to_first(id, StatusKind::Review);
        self.notify_task(&review.thread_id, format!("{key}: pull request opened"), review.pr.url.clone());
        self.park(&task.project, id, &review);
        self.ensure_polling();
    }

    /// While the pull request waits its folder is not needed: a clean worktree is removed, the branch
    /// stays. The agent is stopped first so no process sits in a deleted folder.
    fn park(&mut self, project: &str, id: &str, review: &Review) {
        let Some(worktree) = review.worktree.clone() else { return };
        let stopped = self.dispose_session(&review.thread_id);
        let (git, repo, id) = (self.git.clone(), project.to_owned(), id.to_owned());
        let work = async move {
            if let Some(stopped) = stopped {
                let _ = stopped.await;
            }
            if !std::path::Path::new(&worktree).exists() {
                return Ok(true);
            }
            if !git(worktree.clone(), argv(["status", "--porcelain"])).await?.trim().is_empty() {
                return Ok(false);
            }
            git(repo, argv(["worktree", "remove", &worktree])).await.map(|_| true)
        };
        self.with_git_lock(project, work, move |core, removed: Result<bool, String>| match removed {
            Ok(true) => core.update_review(&id, |r| r.worktree = None),
            Ok(false) => {}
            Err(error) => eprintln!("oniko: could not park the worktree of task {id}: {error}"),
        });
    }

    fn in_review(&self) -> bool {
        self.tasks.state().tasks.iter().any(|t| matches!(t.run, Some(TaskRun::Review(_))))
    }

    pub(crate) fn ensure_polling(&mut self) {
        if self.pr_poll.is_some() || !self.in_review() {
            return;
        }
        let me = self.me.clone();
        self.pr_poll = Some(self.me.runtime.spawn(async move {
            let mut every = tokio::time::interval_at(tokio::time::Instant::now() + POLL_EVERY, POLL_EVERY);
            loop {
                every.tick().await;
                me.send(|core| core.check_pull_requests());
            }
        }));
    }

    /// One `gh pr list` per project with a task in review; polling stops once none is.
    pub(crate) fn check_pull_requests(&mut self) {
        let mut projects: Vec<String> = Vec::new();
        for task in &self.tasks.state().tasks {
            if matches!(task.run, Some(TaskRun::Review(_))) && !projects.contains(&task.project) {
                projects.push(task.project.clone());
            }
        }
        if projects.is_empty() {
            if let Some(poll) = self.pr_poll.take() {
                poll.abort();
            }
            return;
        }
        for project in projects {
            let (gh, me) = (self.gh.clone(), self.me.clone());
            self.me.runtime.spawn(async move {
                let listed = gh(project.clone(), argv(["pr", "list", "--state", "all", "--limit", "200", "--json", PR_FIELDS])).await;
                me.send(move |core| core.pull_requests_listed(&project, listed));
            });
        }
    }

    fn pull_requests_listed(&mut self, project: &str, listed: Result<String, String>) {
        let listed = match listed.and_then(|json| parse_pull_requests(&json)) {
            Ok(listed) => listed,
            Err(error) => return eprintln!("oniko: could not check the pull requests of {project}: {error}"),
        };
        let ids: Vec<String> = self.tasks.state().tasks.iter().filter(|t| t.project == project).map(|t| t.id.clone()).collect();
        for id in ids {
            let Some((task, review)) = self.review_of(&id) else { continue };
            let Some((_, pr)) = listed.iter().find(|(head, _)| *head == review.branch) else { continue };
            self.observe_review(&task, review, pr.clone());
        }
    }

    fn observe_review(&mut self, task: &Task, review: Review, pr: PullRequest) {
        let (next, action) = review.observe(pr);
        if action == Some(ReviewAction::Merged) {
            return self.merged(task, next);
        }
        if next != review {
            self.set_run(&task.id, Some(TaskRun::Review(next.clone())));
        }
        let key = task.key();
        match action {
            Some(ReviewAction::Closed) => self.notify_task(&next.thread_id, format!("{key}: pull request closed without merging"), next.pr.url),
            Some(ReviewAction::FixConflict) => self.fix_conflict(task, &next),
            Some(ReviewAction::ConflictPersists) => {
                self.notify_task(&next.thread_id, format!("{key}: the pull request still conflicts"), "The agent could not resolve it; it needs you.".into())
            }
            Some(ReviewAction::Merged) | None => {}
        }
    }

    /// Brings the parked worktree back at the same path and hands the conflict to the agent.
    fn fix_conflict(&mut self, task: &Task, review: &Review) {
        if !self.threads.contains_key(&review.thread_id) {
            self.update_review(&task.id, |r| r.conflict_fix = ConflictFix::Escalated);
            return self.notify_task(&review.thread_id, format!("{}: the pull request conflicts", task.key()), "Its thread was removed; it needs you.".into());
        }
        let path = worktree_path(&self.worktree_root, &task.project, &review.branch);
        let work = worktree_for(self.git.clone(), task.project.clone(), path, review.branch.clone());
        let id = task.id.clone();
        self.with_git_lock(&task.project, work, move |core, result| core.unparked(&id, result));
    }

    fn unparked(&mut self, id: &str, result: Result<String, String>) {
        let Some((task, review)) = self.review_of(id) else { return };
        if review.conflict_fix != ConflictFix::Fixing {
            return;
        }
        let title = format!("{}: the pull request conflicts", task.key());
        let worktree = match result {
            Ok(worktree) => worktree,
            Err(error) => {
                self.update_review(id, |r| r.conflict_fix = ConflictFix::Escalated);
                return self.notify_task(&review.thread_id, title, error);
            }
        };
        self.update_review(id, |r| r.worktree = Some(worktree));
        self.notify_task(&review.thread_id, title, "The agent is resolving it.".into());
        if let Ok(session) = self.session(&review.thread_id) {
            session.prompt(conflict_prompt(&review.branch));
        }
    }

    /// The next check tells whether the fix worked; until then the worktree is parked again.
    pub(crate) fn conflict_turn_ended(&mut self, task: &Task, review: Review) {
        self.update_review(&task.id, |r| r.conflict_fix = ConflictFix::Attempted);
        self.park(&task.project, &task.id, &review);
    }

    fn merged(&mut self, task: &Task, review: Review) {
        let merged = Merged { thread_id: review.thread_id, branch: review.branch, pr: review.pr };
        self.set_run(&task.id, Some(TaskRun::Merged(merged.clone())));
        self.move_to_first(&task.id, StatusKind::Completed);
        self.notify_task(&merged.thread_id, format!("{}: pull request merged", task.key()), task.title.clone());
        self.clean_merged(&task.project, &merged);
    }

    /// Removes the worktree (only if it is clean) and the local branch, fetches the base the waiting
    /// tasks start from, then lets them start. Each step checks what is there, so it can run again.
    fn clean_merged(&mut self, project: &str, merged: &Merged) {
        let stopped = self.dispose_session(&merged.thread_id);
        let worktree = worktree_path(&self.worktree_root, project, &merged.branch);
        let (git, repo, branch) = (self.git.clone(), project.to_owned(), merged.branch.clone());
        let work = async move {
            if let Some(stopped) = stopped {
                let _ = stopped.await;
            }
            let path = worktree.to_string_lossy().into_owned();
            if worktree.exists()
                && let Err(error) = git(repo.clone(), argv(["worktree", "remove", &path])).await
            {
                eprintln!("oniko: left the worktree {path} in place: {error}");
            }
            let has_branch = git(repo.clone(), argv(["branch", "--list", &branch])).await.is_ok_and(|out| !out.trim().is_empty());
            if has_branch && let Err(error) = git(repo.clone(), argv(["branch", "-D", &branch])).await {
                eprintln!("oniko: could not delete the branch {branch}: {error}");
            }
            if let Err(error) = fetch_base(&git, &repo).await {
                eprintln!("oniko: {error}");
            }
        };
        self.with_git_lock(project, work, |core, ()| core.schedule());
    }

    /// On startup: merged tasks whose folder survived get cleaned, an interrupted conflict fix counts
    /// as attempted, review worktrees are parked if they can be, and reviews are checked now and then
    /// every minute.
    pub(crate) fn resume_reviews(&mut self) {
        let tasks = self.tasks.state().tasks.clone();
        for task in tasks {
            match task.run.clone() {
                Some(TaskRun::Merged(merged)) if worktree_path(&self.worktree_root, &task.project, &merged.branch).exists() => {
                    self.clean_merged(&task.project, &merged)
                }
                Some(TaskRun::Review(review)) if review.conflict_fix == ConflictFix::Fixing => self.conflict_turn_ended(&task, review),
                Some(TaskRun::Review(review)) => self.park(&task.project, &task.id, &review),
                _ => {}
            }
        }
        if self.in_review() {
            self.check_pull_requests();
            self.ensure_polling();
        }
    }
}
