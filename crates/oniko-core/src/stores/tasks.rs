use std::rc::Rc;

use anyhow::{Result, anyhow, bail};

use crate::db::Db;
use crate::model::{
    DEFAULT_STATUSES, StatusKind, SubscriptionId, Task, TaskId, TaskRun, TaskStatus, TaskStatusId, TasksState, next_status_color,
    position_between,
};
use crate::util::{now_ms, uuid};

#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    /// `Some(None)` clears it back to "whichever is active".
    pub subscription_id: Option<Option<SubscriptionId>>,
    pub pull_request: Option<bool>,
    /// `Some(None)` clears it back to "the last thread's model".
    pub model: Option<Option<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct StatusPatch {
    pub name: Option<String>,
    pub kind: Option<StatusKind>,
}

/// A task that just landed in a status it was not in before, including the one it is created in.
pub type Entered = (Task, TaskStatus);

/// Every project's tasks and statuses.
pub struct TaskStore {
    db: Rc<Db>,
    current: TasksState,
}

fn require_title(title: &str) -> Result<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        bail!("A task needs a name");
    }
    Ok(trimmed.to_owned())
}

fn require_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("A status needs a name");
    }
    Ok(trimmed.to_owned())
}

impl TaskStore {
    pub fn new(db: Rc<Db>) -> Result<TaskStore> {
        let current = TasksState { statuses: db.load_task_statuses()?, tasks: db.load_tasks()? };
        Ok(TaskStore { db, current })
    }

    pub fn state(&self) -> &TasksState {
        &self.current
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.current.task(id)
    }

    /// A project gets the default statuses the first time its board is opened. True when it seeded them.
    pub fn open(&mut self, project: &str) -> Result<bool> {
        if !self.current.project_statuses(project).is_empty() {
            return Ok(false);
        }
        for (i, (name, color, kind)) in DEFAULT_STATUSES.iter().enumerate() {
            self.db.save_task_status(&TaskStatus {
                id: uuid(),
                project: project.to_owned(),
                name: (*name).to_owned(),
                color: *color,
                kind: *kind,
                position: (i + 1) as f64,
            })?;
        }
        self.reload()?;
        Ok(true)
    }

    pub fn create(&mut self, project: &str, title: &str, description: &str, status_id: &str) -> Result<(TaskId, Entered)> {
        let status = self.status(status_id)?.clone();
        if status.project != project {
            bail!("That status belongs to another project");
        }
        let now = now_ms();
        let id = uuid();
        let number = self.current.tasks.iter().filter(|t| t.project == project).map(|t| t.number).max().unwrap_or(0) + 1;
        let last = self.current.status_tasks(&status.id).last().map(|t| t.position);
        self.db.save_task(&Task {
            id: id.clone(),
            project: project.to_owned(),
            number,
            title: require_title(title)?,
            description: description.trim().to_owned(),
            status_id: status.id.clone(),
            position: position_between(last, None),
            created_at: now,
            updated_at: now,
            subscription_id: None,
            pull_request: false,
            model: None,
            blocked_by: vec![],
            run: None,
        })?;
        self.reload()?;
        Ok((id.clone(), (self.task(&id)?.clone(), status)))
    }

    pub fn update(&mut self, id: &str, patch: TaskPatch) -> Result<()> {
        let mut task = self.task(id)?.clone();
        if let Some(subscription_id) = &patch.subscription_id
            && *subscription_id != task.subscription_id
            && task.thread_id().is_some()
        {
            bail!("The agent already started on its subscription");
        }
        if let Some(model) = &patch.model
            && *model != task.model
            && task.thread_id().is_some()
        {
            bail!("The agent already started on its model");
        }
        if patch.pull_request == Some(false)
            && let Some(dependent) = self.current.dependents(id).first()
        {
            bail!("{} waits for this pull request; remove that link first", dependent.key());
        }
        if let Some(title) = &patch.title {
            task.title = require_title(title)?;
        }
        if let Some(description) = &patch.description {
            task.description = description.trim().to_owned();
        }
        if let Some(subscription_id) = patch.subscription_id {
            task.subscription_id = subscription_id;
        }
        if let Some(pull_request) = patch.pull_request {
            task.pull_request = pull_request;
        }
        if let Some(model) = patch.model {
            task.model = model;
        }
        task.updated_at = now_ms();
        self.db.save_task(&task)?;
        self.reload()
    }

    /// Written by the task runner only; not a user edit, so `updated_at` stays.
    pub fn set_run(&mut self, id: &str, run: Option<TaskRun>) -> Result<()> {
        let task = Task { run, ..self.task(id)?.clone() };
        self.db.save_task(&task)?;
        self.reload()
    }

    /// Puts the task into `status_id` just ahead of `before_id`, or last when there is none.
    /// Returns what it entered when the status changed, and whether anything changed at all.
    pub fn move_task(&mut self, id: &str, status_id: &str, before_id: Option<&str>) -> Result<(bool, Option<Entered>)> {
        let task = self.task(id)?.clone();
        let status = self.status(status_id)?.clone();
        if status.project != task.project {
            bail!("That status belongs to another project");
        }
        let siblings: Vec<&Task> = self.current.status_tasks(status_id).into_iter().filter(|t| t.id != id).collect();
        let at = before_id.and_then(|before| siblings.iter().position(|t| t.id == before)).unwrap_or(siblings.len());
        let position = position_between(at.checked_sub(1).map(|i| siblings[i].position), siblings.get(at).map(|t| t.position));
        if task.status_id == status_id && task.position == position {
            return Ok((false, None));
        }
        let moved_status = task.status_id != status_id;
        self.db.save_task(&Task { status_id: status_id.to_owned(), position, updated_at: now_ms(), ..task })?;
        self.reload()?;
        let entered = moved_status.then(|| (self.current.task(id).cloned(), status)).and_then(|(t, s)| t.map(|t| (t, s)));
        Ok((true, entered))
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.db.delete_task(id)?;
        self.reload()
    }

    /// `task` waits for `blocker`'s pull request to merge, so the blocker is made to open one.
    pub fn add_dependency(&mut self, task_id: &str, blocker_id: &str) -> Result<()> {
        let task = self.task(task_id)?;
        let blocker = self.task(blocker_id)?;
        if task.project != blocker.project {
            bail!("A task can only wait for a task of the same project");
        }
        if task_id == blocker_id {
            bail!("A task cannot wait for itself");
        }
        if self.current.depends_on(blocker_id, task_id) {
            bail!("{} already waits for {}", blocker.key(), task.key());
        }
        if !blocker.pull_request {
            self.db.save_task(&Task { pull_request: true, updated_at: now_ms(), ..blocker.clone() })?;
        }
        self.db.add_dependency(task_id, blocker_id)?;
        self.reload()
    }

    pub fn remove_dependency(&mut self, task_id: &str, blocker_id: &str) -> Result<()> {
        self.db.remove_dependency(task_id, blocker_id)?;
        self.reload()
    }

    pub fn add_status(&mut self, project: &str, name: &str) -> Result<TaskStatusId> {
        let existing = self.current.project_statuses(project);
        let id = uuid();
        let status = TaskStatus {
            id: id.clone(),
            project: project.to_owned(),
            name: require_name(name)?,
            color: next_status_color(&existing),
            kind: StatusKind::Started,
            position: position_between(existing.last().map(|s| s.position), None),
        };
        self.db.save_task_status(&status)?;
        self.reload()?;
        Ok(id)
    }

    pub fn update_status(&mut self, id: &str, patch: StatusPatch) -> Result<()> {
        let mut status = self.status(id)?.clone();
        if let Some(name) = &patch.name {
            status.name = require_name(name)?;
        }
        if let Some(kind) = patch.kind {
            status.kind = kind;
        }
        self.db.save_task_status(&status)?;
        self.reload()
    }

    /// A project keeps at least one status; tasks in a removed one move to the first that remains.
    pub fn remove_status(&mut self, id: &str) -> Result<()> {
        let status = self.status(id)?.clone();
        let fallback = self
            .current
            .project_statuses(&status.project)
            .into_iter()
            .find(|s| s.id != id)
            .map(|s| s.id.clone())
            .ok_or_else(|| anyhow!("A project needs at least one status"))?;
        self.db.delete_task_status(id, &fallback)?;
        self.reload()
    }

    fn task(&self, id: &str) -> Result<&Task> {
        self.current.task(id).ok_or_else(|| anyhow!("Unknown task {id}"))
    }

    fn status(&self, id: &str) -> Result<&TaskStatus> {
        self.current.status(id).ok_or_else(|| anyhow!("Unknown status {id}"))
    }

    /// Re-reading after each write keeps memory identical to the db, including what a status removal moved.
    fn reload(&mut self) -> Result<()> {
        self.current = TasksState { statuses: self.db.load_task_statuses()?, tasks: self.db.load_tasks()? };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RunInfo;

    const PROJECT: &str = "/tmp/oniko";

    struct Opened {
        db: Rc<Db>,
        store: TaskStore,
        backlog: String,
        todo: String,
        done: String,
    }

    fn opened() -> Opened {
        let db = Rc::new(Db::in_memory().unwrap());
        let mut store = TaskStore::new(db.clone()).unwrap();
        assert!(store.open(PROJECT).unwrap());
        assert!(!store.open(PROJECT).unwrap());
        let ids: Vec<String> = store.state().project_statuses(PROJECT).iter().map(|s| s.id.clone()).collect();
        Opened { db, store, backlog: ids[0].clone(), todo: ids[1].clone(), done: ids[4].clone() }
    }

    fn titles(store: &TaskStore, status: &str) -> Vec<String> {
        store.state().status_tasks(status).iter().map(|t| t.title.clone()).collect()
    }

    #[test]
    fn seeds_the_default_statuses_once_per_project() {
        let o = opened();
        let names: Vec<&str> = o.store.state().project_statuses(PROJECT).iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Backlog", "To do", "In progress", "In review", "Done"]);
        let kinds: Vec<StatusKind> = o.store.state().project_statuses(PROJECT).iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [StatusKind::Backlog, StatusKind::Unstarted, StatusKind::Started, StatusKind::Review, StatusKind::Completed]);
        assert!(o.store.state().project_statuses("/other").is_empty());
    }

    #[test]
    fn creates_edits_and_persists_a_task() {
        let mut o = opened();
        let (id, (task, status)) = o.store.create(PROJECT, "  Ship tasks ", "List and board", &o.todo).unwrap();
        assert_eq!((task.title.as_str(), status.name.as_str()), ("Ship tasks", "To do"));
        o.store.update(&id, TaskPatch { description: Some("List, board, custom statuses".into()), ..Default::default() }).unwrap();
        let reopened = TaskStore::new(o.db.clone()).unwrap();
        let saved = reopened.get(&id).unwrap();
        assert_eq!((saved.title.as_str(), saved.description.as_str()), ("Ship tasks", "List, board, custom statuses"));
        let error = o.store.update(&id, TaskPatch { title: Some("   ".into()), ..Default::default() }).unwrap_err();
        assert!(error.to_string().contains("needs a name"));
    }

    #[test]
    fn moves_a_task_between_statuses_and_orders_it_ahead_of_another() {
        let mut o = opened();
        let (a, _) = o.store.create(PROJECT, "a", "", &o.backlog).unwrap();
        let (b, _) = o.store.create(PROJECT, "b", "", &o.backlog).unwrap();
        let (c, _) = o.store.create(PROJECT, "c", "", &o.todo).unwrap();
        o.store.move_task(&b, &o.todo, Some(&c)).unwrap();
        assert_eq!(titles(&o.store, &o.todo), ["b", "c"]);
        o.store.move_task(&a, &o.todo, None).unwrap();
        assert_eq!(titles(&o.store, &o.todo), ["b", "c", "a"]);
        o.store.move_task(&a, &o.todo, Some(&b)).unwrap();
        assert_eq!(titles(&o.store, &o.todo), ["a", "b", "c"]);
        assert!(titles(&o.store, &o.backlog).is_empty());
    }

    #[test]
    fn adds_custom_statuses_with_an_unused_color() {
        let mut o = opened();
        let id = o.store.add_status(PROJECT, "Review").unwrap();
        o.store.update_status(&id, StatusPatch { name: Some("Code review".into()), ..Default::default() }).unwrap();
        let statuses = TaskStore::new(o.db.clone()).unwrap().state().project_statuses(PROJECT).into_iter().cloned().collect::<Vec<_>>();
        assert_eq!(statuses.last().unwrap().name, "Code review");
        assert!(!statuses[..statuses.len() - 1].iter().any(|s| s.color == statuses.last().unwrap().color));
    }

    #[test]
    fn removing_a_status_moves_its_tasks_and_keeps_the_last_one() {
        let mut o = opened();
        o.store.create(PROJECT, "old", "", &o.backlog).unwrap();
        o.store.create(PROJECT, "x", "", &o.todo).unwrap();
        o.store.create(PROJECT, "y", "", &o.todo).unwrap();
        o.store.remove_status(&o.todo).unwrap();
        assert_eq!(titles(&o.store, &o.backlog), ["old", "x", "y"]);
        o.store.remove_status(&o.backlog).unwrap();
        let first = o.store.state().project_statuses(PROJECT)[0].id.clone();
        assert_eq!(titles(&o.store, &first), ["old", "x", "y"]);
        let rest: Vec<String> = o.store.state().project_statuses(PROJECT).iter().map(|s| s.id.clone()).collect();
        for id in &rest[..rest.len() - 1] {
            o.store.remove_status(id).unwrap();
        }
        assert_eq!(o.store.state().project_statuses(PROJECT).iter().map(|s| s.id.clone()).collect::<Vec<_>>(), [o.done.clone()]);
        assert!(o.store.remove_status(&o.done).unwrap_err().to_string().contains("at least one status"));
    }

    #[test]
    fn numbers_tasks_per_project_and_reports_entered_statuses() {
        let mut o = opened();
        let (a, _) = o.store.create(PROJECT, "a", "", &o.todo).unwrap();
        let (b, _) = o.store.create(PROJECT, "b", "", &o.todo).unwrap();
        o.store.open("/other").unwrap();
        let other_status = o.store.state().project_statuses("/other")[0].id.clone();
        let (other, _) = o.store.create("/other", "c", "", &other_status).unwrap();
        let number = |id: &str| o.store.get(id).unwrap().number;
        assert_eq!([number(&a), number(&b), number(&other)], [1, 2, 1]);
        let (_, same) = o.store.move_task(&a, &o.todo, None).unwrap();
        assert!(same.is_none());
        let (_, entered) = o.store.move_task(&a, &o.backlog, None).unwrap();
        assert_eq!(entered.unwrap().1.name, "Backlog");
    }

    #[test]
    fn links_tasks_of_one_project_without_cycles() {
        let mut o = opened();
        let (a, _) = o.store.create(PROJECT, "a", "", &o.backlog).unwrap();
        let (b, _) = o.store.create(PROJECT, "b", "", &o.backlog).unwrap();
        let (c, _) = o.store.create(PROJECT, "c", "", &o.backlog).unwrap();
        o.store.add_dependency(&b, &a).unwrap();
        o.store.add_dependency(&c, &b).unwrap();
        assert!(o.store.get(&a).unwrap().pull_request, "a blocker always opens a pull request");
        for (task, blocker) in [(&a, &c), (&a, &b)] {
            assert!(o.store.add_dependency(task, blocker).unwrap_err().to_string().contains("already waits"), "{task} <- {blocker}");
        }
        assert!(o.store.add_dependency(&a, &a).unwrap_err().to_string().contains("itself"));
        o.store.open("/other").unwrap();
        let other_status = o.store.state().project_statuses("/other")[0].id.clone();
        let (other, _) = o.store.create("/other", "x", "", &other_status).unwrap();
        assert!(o.store.add_dependency(&other, &a).unwrap_err().to_string().contains("same project"));
        let error = o.store.update(&a, TaskPatch { pull_request: Some(false), ..Default::default() }).unwrap_err();
        assert!(error.to_string().contains("ONI-2 waits"), "{error}");
        let reopened = TaskStore::new(o.db.clone()).unwrap();
        assert_eq!(reopened.get(&c).unwrap().blocked_by, std::slice::from_ref(&b));
        o.store.remove(&b).unwrap();
        let reopened = TaskStore::new(o.db.clone()).unwrap();
        assert!(reopened.get(&c).unwrap().blocked_by.is_empty());
        assert!(reopened.state().dependents(&a).is_empty());
        o.store.update(&a, TaskPatch { pull_request: Some(false), ..Default::default() }).unwrap();
    }

    #[test]
    fn persists_the_agent_fields_and_guards_the_subscription() {
        let mut o = opened();
        let (id, _) = o.store.create(PROJECT, "a", "", &o.backlog).unwrap();
        o.store
            .update(&id, TaskPatch { subscription_id: Some(Some("sub-1".into())), pull_request: Some(true), ..Default::default() })
            .unwrap();
        let run = TaskRun::Working(RunInfo { thread_id: "t-1".into(), worktree: "/w".into(), branch: "b".into() });
        o.store.set_run(&id, Some(run.clone())).unwrap();
        let saved = TaskStore::new(o.db.clone()).unwrap().get(&id).cloned().unwrap();
        assert_eq!((saved.subscription_id.as_deref(), saved.pull_request, saved.run), (Some("sub-1"), true, Some(run)));
        let error = o.store.update(&id, TaskPatch { subscription_id: Some(None), ..Default::default() }).unwrap_err();
        assert!(error.to_string().contains("already started"));
    }

    #[test]
    fn the_model_is_fixed_once_the_agent_started() {
        let mut o = opened();
        let (id, _) = o.store.create(PROJECT, "a", "", &o.backlog).unwrap();
        o.store.update(&id, TaskPatch { model: Some(Some("opus".into())), ..Default::default() }).unwrap();
        assert_eq!(TaskStore::new(o.db.clone()).unwrap().get(&id).unwrap().model.as_deref(), Some("opus"));
        o.store.set_run(&id, Some(TaskRun::Working(RunInfo { thread_id: "t-1".into(), worktree: "/w".into(), branch: "b".into() }))).unwrap();
        let error = o.store.update(&id, TaskPatch { model: Some(Some("haiku".into())), ..Default::default() }).unwrap_err();
        assert!(error.to_string().contains("already started"));
        o.store.update(&id, TaskPatch { model: Some(Some("opus".into())), ..Default::default() }).unwrap();
    }
}
