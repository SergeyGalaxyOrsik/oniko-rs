//! The core actor: one thread owns all state, as Electron's main process did. Commands from the UI
//! and results of background work (agent events, git, login probes) arrive as jobs and run one at a
//! time, so no state is ever shared or locked.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};

use crate::db::Db;
use crate::model::reducer::{Outcome, ThreadMap, reduce_threads};
use crate::model::{
    AgentId, AppEvent, AppSnapshot, SequencedEvent, Subscription, SubscriptionId, TaskId, TaskStatusId, Thread, ThreadEvent,
    ThreadEventBody, ThreadId, ThreadStatus,
};
use crate::registry::{AccountSpecs, account_specs, base_env};
use crate::session::{Emit, SessionHandle, SessionOptions, log_path_for};
use crate::stores::subscriptions::SubscriptionStore;
use crate::stores::tasks::{StatusPatch, TaskPatch, TaskStore};
use crate::telemetry::contract::DisplayInfo;
use crate::telemetry::{Telemetry, TelemetryConfig, TelemetryState};
use crate::util::{home_dir, now_ms, uuid};

pub type Job = Box<dyn FnOnce(&mut Core) + Send>;

/// What the core needs from an agent session; the real one is [`SessionHandle`].
pub trait AgentSession {
    fn ensure(&self);
    fn prompt(&self, text: String);
    fn cancel(&self);
    fn set_config(&self, config_id: String, value: String);
    fn respond_permission(&self, request_id: &str, option_id: Option<String>);
    fn authenticate(&self, method_id: String);
    fn dispose(&self) -> Option<tokio::task::JoinHandle<()>>;
}

impl AgentSession for SessionHandle {
    fn ensure(&self) {
        SessionHandle::ensure(self)
    }
    fn prompt(&self, text: String) {
        SessionHandle::prompt(self, text)
    }
    fn cancel(&self) {
        SessionHandle::cancel(self)
    }
    fn set_config(&self, config_id: String, value: String) {
        SessionHandle::set_config(self, config_id, value)
    }
    fn respond_permission(&self, request_id: &str, option_id: Option<String>) {
        SessionHandle::respond_permission(self, request_id, option_id)
    }
    fn authenticate(&self, method_id: String) {
        SessionHandle::authenticate(self, method_id)
    }
    fn dispose(&self) -> Option<tokio::task::JoinHandle<()>> {
        SessionHandle::dispose(self)
    }
}

pub type SessionFactory = Arc<dyn Fn(SessionOptions) -> Box<dyn AgentSession> + Send + Sync>;

/// Runs git in a folder: stdout on success, stderr (or the error) otherwise.
pub type Git = Arc<dyn Fn(String, Vec<String>) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

pub fn system_git() -> Git {
    Arc::new(|cwd, args| {
        Box::pin(async move {
            let out = tokio::process::Command::new("git")
                .args(&args)
                .current_dir(&cwd)
                .env_clear()
                .envs(base_env())
                .output()
                .await
                .map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                Err(if stderr.is_empty() { format!("git {} failed with {}", args.join(" "), out.status) } else { stderr })
            }
        })
    })
}

/// A notification about a task's agent; clicking it should open the thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskNotice {
    pub title: String,
    pub body: String,
    pub thread_id: ThreadId,
}

pub type Notify = Arc<dyn Fn(TaskNotice) + Send + Sync>;

/// The UI's line to the core. Cheap to clone; every call is answered on the core thread.
#[derive(Clone)]
pub struct CoreHandle {
    tx: mpsc::UnboundedSender<Job>,
    pub(crate) runtime: tokio::runtime::Handle,
}

const STOPPED: &str = "Oniko's core has stopped";

impl CoreHandle {
    pub fn send(&self, job: impl FnOnce(&mut Core) + Send + 'static) {
        let _ = self.tx.send(Box::new(job));
    }

    /// Runs `f` on the core thread and resolves with its result, on any executor.
    pub fn call<R: Send + 'static, F: FnOnce(&mut Core) -> R + Send + 'static>(&self, f: F) -> impl Future<Output = Result<R, String>> + use<R, F> {
        let (tx, rx) = oneshot::channel();
        self.send(move |core| {
            let _ = tx.send(f(core));
        });
        async move { rx.await.map_err(|_| STOPPED.to_owned()) }
    }

    /// A named command, counted by telemetry, with its failure reported.
    pub fn command<R: Send + 'static, F: FnOnce(&mut Core) -> Result<R, String> + Send + 'static>(
        &self,
        name: &'static str,
        f: F,
    ) -> impl Future<Output = Result<R, String>> + use<R, F> {
        let call = self.call(move |core| {
            core.telemetry.invoked(name);
            let result = f(core);
            if let Err(message) = &result {
                core.telemetry.failed(name, message);
            }
            result
        });
        async move { call.await.and_then(|r| r) }
    }

    /// Runs a future on the core's Tokio runtime and resolves with its output, on any executor.
    pub fn on_runtime<T: Send + 'static, Fut: Future<Output = T> + Send + 'static>(&self, future: Fut) -> impl Future<Output = Option<T>> + use<T, Fut> {
        let task = self.runtime.spawn(future);
        async move { task.await.ok() }
    }

    pub fn snapshot(&self) -> impl Future<Output = Result<AppSnapshot, String>> + use<> {
        self.command("app:snapshot", |core| Ok(core.snapshot()))
    }

    /// Starts on the active subscription unless one is given. `config` holds values picked before the session exists.
    pub fn create_thread(
        &self,
        cwd: String,
        subscription_id: Option<SubscriptionId>,
        config: Vec<(String, String)>,
    ) -> impl Future<Output = Result<ThreadId, String>> + use<> {
        self.command("threads:create", move |core| core.create_thread_command(cwd, subscription_id, config))
    }

    pub fn prompt(&self, id: ThreadId, text: String) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("threads:prompt", move |core| core.session(&id).map(|s| s.prompt(text)))
    }

    pub fn cancel(&self, id: ThreadId) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("threads:cancel", move |core| core.session(&id).map(|s| s.cancel()))
    }

    pub fn set_config(&self, id: ThreadId, config_id: String, value: String) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("threads:setConfig", move |core| core.session(&id).map(|s| s.set_config(config_id, value)))
    }

    /// `None` declines.
    pub fn respond_permission(&self, id: ThreadId, request_id: String, option_id: Option<String>) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("threads:respondPermission", move |core| core.session(&id).map(|s| s.respond_permission(&request_id, option_id)))
    }

    pub fn authenticate(&self, id: ThreadId, method_id: String) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("threads:authenticate", move |core| core.session(&id).map(|s| s.authenticate(method_id)))
    }

    pub fn remove_thread(&self, id: ThreadId) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("threads:remove", move |core| {
            core.remove_thread(&id);
            Ok(())
        })
    }

    pub fn add_subscription(&self, agent_id: AgentId, config_dir: String, label: Option<String>) -> impl Future<Output = Result<SubscriptionId, String>> + use<> {
        self.command("subscriptions:add", move |core| core.add_subscription(agent_id, &config_dir, label.as_deref()))
    }

    pub fn rename_subscription(&self, id: SubscriptionId, label: String) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("subscriptions:update", move |core| core.rename_subscription(&id, &label))
    }

    pub fn remove_subscription(&self, id: SubscriptionId) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("subscriptions:remove", move |core| core.remove_subscription(&id))
    }

    pub fn refresh_subscriptions(&self, id: Option<SubscriptionId>) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("subscriptions:refresh", move |core| {
            core.refresh_subscriptions(id.as_deref());
            Ok(())
        })
    }

    pub fn set_active_subscription(&self, id: SubscriptionId) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("subscriptions:setActive", move |core| {
            if core.subscriptions.set_active(&id) {
                core.publish_subscriptions();
            }
            Ok(())
        })
    }

    /// Config dirs that look like Claude subscriptions and are not added yet.
    pub fn detect_subscriptions(&self) -> impl Future<Output = Result<Vec<String>, String>> + use<> {
        self.command("subscriptions:detect", |core| Ok(core.subscriptions.detect()))
    }

    /// Seeds the default statuses for a project that has none yet.
    pub fn open_project(&self, project: String) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("tasks:open", move |core| {
            if core.tasks.open(&project).map_err(err)? {
                core.publish_tasks();
            }
            Ok(())
        })
    }

    pub fn create_task(&self, project: String, title: String, description: String, status_id: TaskStatusId) -> impl Future<Output = Result<TaskId, String>> + use<> {
        self.command("tasks:create", move |core| {
            let (id, entered) = core.tasks.create(&project, &title, &description, &status_id).map_err(err)?;
            core.publish_tasks();
            core.task_entered(entered);
            Ok(id)
        })
    }

    /// The subscription can only change until the agent has started.
    pub fn update_task(&self, id: TaskId, patch: TaskPatch) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("tasks:update", move |core| {
            core.tasks.update(&id, patch).map_err(err)?;
            core.publish_tasks();
            Ok(())
        })
    }

    /// Places the task in `status_id` ahead of `before_id`, or last when it is None.
    pub fn move_task(&self, id: TaskId, status_id: TaskStatusId, before_id: Option<TaskId>) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("tasks:move", move |core| core.move_task(&id, &status_id, before_id.as_deref()))
    }

    pub fn remove_task(&self, id: TaskId) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("tasks:remove", move |core| {
            core.tasks.remove(&id).map_err(err)?;
            core.publish_tasks();
            Ok(())
        })
    }

    pub fn add_status(&self, project: String, name: String) -> impl Future<Output = Result<TaskStatusId, String>> + use<> {
        self.command("tasks:addStatus", move |core| {
            let id = core.tasks.add_status(&project, &name).map_err(err)?;
            core.publish_tasks();
            Ok(id)
        })
    }

    pub fn update_status(&self, id: TaskStatusId, patch: StatusPatch) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("tasks:updateStatus", move |core| {
            core.tasks.update_status(&id, patch).map_err(err)?;
            core.publish_tasks();
            Ok(())
        })
    }

    /// Its tasks move to the project's first remaining status.
    pub fn remove_status(&self, id: TaskStatusId) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("tasks:removeStatus", move |core| {
            core.tasks.remove_status(&id).map_err(err)?;
            core.publish_tasks();
            Ok(())
        })
    }

    pub fn recent_folders(&self) -> impl Future<Output = Result<Vec<String>, String>> + use<> {
        self.command("folders:recent", |core| Ok(core.recent_folders()))
    }

    /// A folder the user picked as a project.
    pub fn remember_folder(&self, path: String) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("dialog:pickFolder", move |core| {
            core.remember_folder(&path);
            Ok(())
        })
    }

    pub fn telemetry(&self) -> impl Future<Output = Result<TelemetryState, String>> + use<> {
        self.command("telemetry:get", |core| Ok(core.telemetry.state()))
    }

    /// Turning it off also drops whatever is still queued.
    pub fn set_telemetry_enabled(&self, enabled: bool) -> impl Future<Output = Result<(), String>> + use<> {
        self.command("telemetry:setEnabled", move |core| {
            core.telemetry.set_enabled(enabled);
            Ok(())
        })
    }

    /// Stops every agent and gives telemetry a moment to send. Call before the app exits.
    pub fn shutdown(&self) -> impl Future<Output = ()> + use<> {
        let stop = self.call(|core| core.shutdown());
        let runtime = self.runtime.clone();
        async move {
            let Ok((tasks, telemetry)) = stop.await else { return };
            let done = runtime.spawn(async move {
                let agents = async {
                    for task in tasks {
                        let _ = task.await;
                    }
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(3), agents).await;
                telemetry.quit().await;
            });
            let _ = done.await;
        }
    }
}

fn err(error: anyhow::Error) -> String {
    format!("{error:#}")
}

const RECENT_KEY: &str = "recentFolders";
const RECENT_LIMIT: usize = 8;

pub struct CoreDeps {
    pub db: Rc<Db>,
    pub specs: AccountSpecs,
    /// The agent's default config dir, seeded once as "Default" when it exists.
    pub seed: Option<(AgentId, String)>,
    pub log_dir: PathBuf,
    /// Worktrees go in `<root>/<project folder>/<branch>`.
    pub worktree_root: PathBuf,
    pub git: Git,
    pub notify: Notify,
    pub sessions: SessionFactory,
    pub telemetry: Telemetry,
}

pub struct Core {
    pub(crate) db: Rc<Db>,
    seq: u64,
    events: mpsc::UnboundedSender<SequencedEvent>,
    pub(crate) me: CoreHandle,
    pub(crate) threads: ThreadMap,
    sessions: HashMap<ThreadId, Box<dyn AgentSession>>,
    pub(crate) subscriptions: SubscriptionStore,
    pub(crate) tasks: TaskStore,
    log_dir: PathBuf,
    pub(crate) worktree_root: PathBuf,
    pub(crate) git: Git,
    pub(crate) notify: Notify,
    session_factory: SessionFactory,
    pub(crate) telemetry: Telemetry,
}

impl Core {
    pub fn new(deps: CoreDeps, me: CoreHandle, events: mpsc::UnboundedSender<SequencedEvent>) -> Result<Core> {
        let subscriptions = SubscriptionStore::new(deps.db.clone(), deps.specs, deps.seed)?;
        let mut threads = ThreadMap::new();
        for stored in deps.db.load_threads()? {
            let restored = stored.restored();
            deps.db.save_thread(&restored, 0..restored.timeline.len())?;
            threads.insert(restored.id.clone(), restored);
        }
        let tasks = TaskStore::new(deps.db.clone())?;
        let mut core = Core {
            db: deps.db,
            seq: 0,
            events,
            me,
            threads,
            sessions: HashMap::new(),
            subscriptions,
            tasks,
            log_dir: deps.log_dir,
            worktree_root: deps.worktree_root,
            git: deps.git,
            notify: deps.notify,
            session_factory: deps.sessions,
            telemetry: deps.telemetry,
        };
        core.fail_interrupted_starts();
        Ok(core)
    }

    pub fn snapshot(&self) -> AppSnapshot {
        AppSnapshot {
            seq: self.seq,
            home: home_dir().to_string_lossy().into_owned(),
            threads: self.thread_list().into_iter().cloned().collect(),
            subscriptions: self.subscriptions.state(),
            tasks: self.tasks.state().clone(),
        }
    }

    pub(crate) fn publish(&mut self, event: AppEvent) {
        self.seq += 1;
        let _ = self.events.send(SequencedEvent { seq: self.seq, event });
    }

    pub(crate) fn publish_subscriptions(&mut self) {
        let state = self.subscriptions.state();
        self.publish(AppEvent::Subscriptions(state));
    }

    pub(crate) fn publish_tasks(&mut self) {
        let state = self.tasks.state().clone();
        self.publish(AppEvent::Tasks(state));
    }

    /// Asks the UI to show a thread, e.g. after a notification about it is clicked.
    pub fn open_thread(&mut self, id: ThreadId) {
        self.publish(AppEvent::OpenThread(id));
    }

    fn thread_list(&self) -> Vec<&Thread> {
        let mut list: Vec<&Thread> = self.threads.values().collect();
        list.sort_by_key(|t| std::cmp::Reverse(t.created_at));
        list
    }

    /// Every thread change goes through here: reduced, written through to the db, then broadcast.
    pub(crate) fn dispatch(&mut self, event: ThreadEvent) {
        let outcome = reduce_threads(&mut self.threads, &event);
        let saved = match &outcome {
            Outcome::Unchanged => Ok(()),
            Outcome::Created => self.threads.get(&event.thread_id).map_or(Ok(()), |t| self.db.save_thread(t, 0..t.timeline.len())),
            Outcome::Updated { items } => self.threads.get(&event.thread_id).map_or(Ok(()), |t| self.db.save_thread(t, items.iter().copied())),
            Outcome::Removed => self.db.delete_thread(&event.thread_id),
        };
        if let Err(error) = saved {
            eprintln!("oniko: could not save thread {}: {error:#}", event.thread_id);
        }
        if outcome == Outcome::Unchanged {
            return;
        }
        self.publish(AppEvent::Thread(event.clone()));
        self.on_task_thread_event(&event);
    }

    pub(crate) fn count_using(&self, subscription_id: &str) -> usize {
        self.threads.values().filter(|t| t.subscription_id == subscription_id).count()
    }

    fn create_thread_command(&mut self, cwd: String, subscription_id: Option<SubscriptionId>, config: Vec<(String, String)>) -> Result<ThreadId, String> {
        let subscription = match &subscription_id {
            Some(id) => self.subscriptions.get(id),
            None => self.subscriptions.active(),
        }
        .cloned()
        .ok_or("Add a subscription in Settings")?;
        self.remember_folder(&cwd);
        self.create_thread(&subscription, cwd, config, String::new())
    }

    /// Without a title, the first prompt names the thread.
    pub(crate) fn create_thread(&mut self, subscription: &Subscription, cwd: String, config: Vec<(String, String)>, title: String) -> Result<ThreadId, String> {
        let id = uuid();
        let thread = Thread {
            id: id.clone(),
            agent_id: subscription.agent_id,
            subscription_id: subscription.id.clone(),
            cwd,
            title,
            created_at: now_ms(),
            acp_session_id: None,
            status: ThreadStatus::Connecting,
            timeline: vec![],
            config: vec![],
            commands: vec![],
        };
        self.dispatch(ThreadEvent { thread_id: id.clone(), body: ThreadEventBody::ThreadCreated(Box::new(thread)) });
        self.session_with(&id, config)?.ensure();
        Ok(id)
    }

    /// Sessions are created lazily, so a thread without a live process gets one on first use.
    pub(crate) fn session(&mut self, id: &str) -> Result<&dyn AgentSession, String> {
        self.session_with(id, vec![])
    }

    fn session_with(&mut self, id: &str, initial_config: Vec<(String, String)>) -> Result<&dyn AgentSession, String> {
        if !self.sessions.contains_key(id) {
            let thread = self.threads.get(id).ok_or_else(|| format!("Unknown thread {id}"))?;
            let subscription = self.subscriptions.get(&thread.subscription_id).ok_or("This thread’s subscription was removed")?;
            let account_env = self.subscriptions.specs().get(&subscription.agent_id).map(|spec| spec.env_for(&subscription.config_dir)).unwrap_or_default();
            let me = self.me.clone();
            let thread_id = id.to_owned();
            let emit: Emit = Arc::new(move |body| {
                let thread_id = thread_id.clone();
                me.send(move |core| core.dispatch(ThreadEvent { thread_id, body }));
            });
            let session = (self.session_factory)(SessionOptions {
                agent_id: thread.agent_id,
                cwd: thread.cwd.clone(),
                account_env,
                acp_session_id: thread.acp_session_id.clone(),
                initial_config,
                log_path: log_path_for(&self.log_dir, id),
                emit,
            });
            self.sessions.insert(id.to_owned(), session);
        }
        Ok(self.sessions[id].as_ref())
    }

    pub(crate) fn remove_thread(&mut self, id: &str) {
        if let Some(session) = self.sessions.remove(id) {
            session.dispose();
        }
        self.dispatch(ThreadEvent { thread_id: id.to_owned(), body: ThreadEventBody::ThreadRemoved });
    }

    /// Threads that predate subscriptions are stored without one and only load once they are adopted.
    fn load_adopted(&mut self) {
        let stored = match self.db.load_threads() {
            Ok(stored) => stored,
            Err(error) => return eprintln!("oniko: could not load adopted threads: {error:#}"),
        };
        for thread in stored {
            if self.threads.contains_key(&thread.id) {
                continue;
            }
            let restored = thread.restored();
            self.dispatch(ThreadEvent { thread_id: restored.id.clone(), body: ThreadEventBody::ThreadCreated(Box::new(restored)) });
        }
    }

    fn add_subscription(&mut self, agent_id: AgentId, config_dir: &str, label: Option<&str>) -> Result<SubscriptionId, String> {
        let (id, adopted) = self.subscriptions.add(agent_id, config_dir, label)?;
        if adopted {
            self.load_adopted();
        }
        self.publish_subscriptions();
        self.refresh_subscriptions(Some(&id));
        Ok(id)
    }

    fn rename_subscription(&mut self, id: &str, label: &str) -> Result<(), String> {
        if self.subscriptions.update(id, label).map_err(err)? {
            self.publish_subscriptions();
        }
        Ok(())
    }

    fn remove_subscription(&mut self, id: &str) -> Result<(), String> {
        let using = self.count_using(id);
        self.subscriptions.remove(id, using).map_err(err)?;
        self.publish_subscriptions();
        Ok(())
    }

    /// Marks the subscriptions as being checked, then checks each login in the background.
    pub fn refresh_subscriptions(&mut self, id: Option<&str>) {
        let targets = self.subscriptions.begin_probe(id);
        if targets.is_empty() {
            return;
        }
        self.publish_subscriptions();
        for target in targets {
            let Some(spec) = self.subscriptions.specs().get(&target.agent_id).cloned() else { continue };
            let me = self.me.clone();
            let probe = spec.probe_status(&target.config_dir);
            self.me.runtime.spawn(async move {
                let account = probe.await;
                me.send(move |core| match core.subscriptions.finish_probe(&target.id, account) {
                    Ok(true) => core.publish_subscriptions(),
                    Ok(false) => {}
                    Err(error) => eprintln!("oniko: could not save a login check: {error:#}"),
                });
            });
        }
    }

    fn move_task(&mut self, id: &str, status_id: &str, before_id: Option<&str>) -> Result<(), String> {
        let (changed, entered) = self.tasks.move_task(id, status_id, before_id).map_err(err)?;
        if changed {
            self.publish_tasks();
        }
        if let Some(entered) = entered {
            self.task_entered(entered);
        }
        Ok(())
    }

    pub fn recent_folders(&self) -> Vec<String> {
        self.db.get_setting(RECENT_KEY).unwrap_or_default()
    }

    pub fn remember_folder(&mut self, path: &str) {
        let mut folders: Vec<String> = vec![path.to_owned()];
        folders.extend(self.recent_folders().into_iter().filter(|p| p != path));
        folders.truncate(RECENT_LIMIT);
        if let Err(error) = self.db.set_setting(RECENT_KEY, &folders) {
            eprintln!("oniko: could not save recent folders: {error:#}");
        }
    }

    fn shutdown(&mut self) -> (Vec<tokio::task::JoinHandle<()>>, Telemetry) {
        let tasks = self.sessions.drain().filter_map(|(_, session)| session.dispose()).collect();
        (tasks, self.telemetry.clone())
    }
}

pub struct StartOptions {
    /// The folder the Electron app used too (`~/Library/Application Support/oniko` on macOS), so both open the same database.
    pub user_data: PathBuf,
    pub worktree_root: PathBuf,
    pub app_version: String,
    pub app_path: String,
    pub displays: Vec<DisplayInfo>,
    pub notify: Notify,
}

impl StartOptions {
    /// The defaults, honoring the environment overrides the Electron app had.
    pub fn from_env(app_version: &str, notify: Notify) -> StartOptions {
        let user_data = std::env::var_os("ONIKO_USER_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::config_dir().unwrap_or_else(home_dir).join("oniko"));
        let worktree_root =
            std::env::var_os("ONIKO_WORKTREES").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".oniko").join("worktrees"));
        let app_path = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_string_lossy().into_owned())).unwrap_or_default();
        StartOptions { user_data, worktree_root, app_version: app_version.to_owned(), app_path, displays: vec![], notify }
    }
}

/// Starts the core on its own thread. Returns once the database is open, or with why it could not be.
pub fn start(options: StartOptions) -> Result<(CoreHandle, mpsc::UnboundedReceiver<SequencedEvent>)> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("oniko-async").build()?;
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<Job>();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let handle = CoreHandle { tx: job_tx, runtime: runtime.handle().clone() };
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let me = handle.clone();
    std::thread::Builder::new().name("oniko-core".into()).spawn(move || {
        runtime.block_on(async move {
            let built = (|| -> Result<Core> {
                let log_dir = options.user_data.join("logs");
                std::fs::create_dir_all(&log_dir).with_context(|| format!("creating {}", log_dir.display()))?;
                let db_path = options.user_data.join("oniko.db");
                let db = Rc::new(Db::open(&db_path)?);
                let telemetry = Telemetry::start(TelemetryConfig {
                    db_path,
                    app_version: options.app_version.clone(),
                    app_path: options.app_path.clone(),
                    displays: options.displays.clone(),
                })?;
                let deps = CoreDeps {
                    db,
                    specs: account_specs(),
                    seed: Some((AgentId::Claude, home_dir().join(".claude").to_string_lossy().into_owned())),
                    log_dir,
                    worktree_root: options.worktree_root.clone(),
                    git: system_git(),
                    notify: options.notify.clone(),
                    sessions: Arc::new(|options| Box::new(SessionHandle::spawn(options)) as Box<dyn AgentSession>),
                    telemetry,
                };
                let mut core = Core::new(deps, me, event_tx)?;
                if let Ok(smoke) = std::env::var("ONIKO_SMOKE_CWD") {
                    core.remember_folder(&smoke);
                }
                core.refresh_subscriptions(None);
                Ok(core)
            })();
            let mut core = match built {
                Ok(core) => {
                    let _ = ready_tx.send(Ok(()));
                    core
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            while let Some(job) = job_rx.recv().await {
                job(&mut core);
            }
        });
    })?;
    ready_rx.recv().context("the core thread stopped while starting")??;
    Ok((handle, event_rx))
}

#[cfg(test)]
pub(crate) mod testing {
    //! A core wired to fakes: an in-memory db, recorded sessions and a scripted git.

    use std::sync::Mutex;

    use super::*;
    use crate::stores::subscriptions::tests::fake_specs;
    use crate::telemetry::client::SettingsStore;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Call {
        Ensure(String),
        Prompt(String, String),
        Dispose(String),
    }

    pub struct FakeSession {
        cwd: String,
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl AgentSession for FakeSession {
        fn ensure(&self) {
            self.calls.lock().unwrap().push(Call::Ensure(self.cwd.clone()));
        }
        fn prompt(&self, text: String) {
            self.calls.lock().unwrap().push(Call::Prompt(self.cwd.clone(), text));
        }
        fn cancel(&self) {}
        fn set_config(&self, _: String, _: String) {}
        fn respond_permission(&self, _: &str, _: Option<String>) {}
        fn authenticate(&self, _: String) {}
        fn dispose(&self) -> Option<tokio::task::JoinHandle<()>> {
            self.calls.lock().unwrap().push(Call::Dispose(self.cwd.clone()));
            None
        }
    }

    struct NoSettings;
    impl SettingsStore for NoSettings {
        fn get(&self, _: &str) -> Option<serde_json::Value> {
            None
        }
        fn set(&self, _: &str, _: serde_json::Value) {}
    }

    pub struct Harness {
        pub core: Core,
        pub jobs: mpsc::UnboundedReceiver<Job>,
        /// Kept open so publishing has a listener, as the UI would be.
        pub _events: mpsc::UnboundedReceiver<SequencedEvent>,
        pub calls: Arc<Mutex<Vec<Call>>>,
        pub notices: Arc<Mutex<Vec<TaskNotice>>>,
        pub dir: tempfile::TempDir,
    }

    impl Harness {
        /// `git` answers each call; the subscription dir is a real folder so it can be added.
        pub fn new(git: impl Fn(&str, &[String]) -> Result<String, String> + Send + Sync + 'static) -> Harness {
            let dir = tempfile::tempdir().unwrap();
            let config_dir = dir.path().join(".claude");
            std::fs::create_dir(&config_dir).unwrap();
            let calls: Arc<Mutex<Vec<Call>>> = Arc::default();
            let notices: Arc<Mutex<Vec<TaskNotice>>> = Arc::default();
            let (job_tx, jobs) = mpsc::unbounded_channel();
            let (event_tx, events) = mpsc::unbounded_channel();
            let me = CoreHandle { tx: job_tx, runtime: tokio::runtime::Handle::current() };
            let session_calls = calls.clone();
            let git = Arc::new(git);
            let recorded_notices = notices.clone();
            let deps = CoreDeps {
                db: Rc::new(Db::in_memory().unwrap()),
                specs: fake_specs(vec![]),
                seed: Some((AgentId::Claude, config_dir.to_string_lossy().into_owned())),
                log_dir: dir.path().join("logs"),
                worktree_root: dir.path().join("worktrees"),
                git: Arc::new(move |cwd, args| {
                    let result = git(&cwd, &args);
                    Box::pin(async move { result })
                }),
                notify: Arc::new(move |notice| recorded_notices.lock().unwrap().push(notice)),
                sessions: Arc::new(move |options| Box::new(FakeSession { cwd: options.cwd, calls: session_calls.clone() }) as Box<dyn AgentSession>),
                telemetry: Telemetry::disabled(Arc::new(NoSettings)),
            };
            let core = Core::new(deps, me, event_tx).unwrap();
            Harness { core, jobs, _events: events, calls, notices, dir }
        }

        /// Runs queued jobs until nothing more arrives for a moment, as the core thread would.
        pub async fn settle(&mut self) {
            while let Ok(Some(job)) = tokio::time::timeout(std::time::Duration::from_millis(50), self.jobs.recv()).await {
                job(&mut self.core);
            }
        }

        pub fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        pub fn notices(&self) -> Vec<TaskNotice> {
            self.notices.lock().unwrap().clone()
        }
    }
}
