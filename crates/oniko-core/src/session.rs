//! One agent process and one ACP session for one thread, on the Rust ACP SDK.
//!
//! The session runs as its own task and takes commands through [`SessionHandle`]. The process is
//! started on first use, and again after it exits; a thread that already has an ACP session id
//! reattaches to it with `session/resume`, or `session/load` with the replay muted.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{Agent, ByteStreams, Client, ErrorCode, UntypedMessage};
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::acp;
use crate::model::config::{config_from_setup, controls_from_config_options, with_value};
use crate::model::{AgentId, ConfigControl, ConfigSource, NoticeLevel, PermissionOutcome, ThreadEventBody, ThreadStatus};
use crate::registry::{EnvLayer, base_env, child_env, spawn_spec};
use crate::util::{iso_now, now_ms, uuid};

pub const FRESH_SESSION_NOTICE: &str = "Started a fresh agent session; earlier context is not in the agent's memory";

pub type Emit = Arc<dyn Fn(ThreadEventBody) + Send + Sync>;

pub struct SessionOptions {
    pub agent_id: AgentId,
    pub cwd: String,
    /// The subscription's env (CLAUDE_CONFIG_DIR set or unset), applied last so nothing inherited survives it.
    pub account_env: EnvLayer,
    pub acp_session_id: Option<String>,
    /// Values picked in the new-thread composer, applied once right after session/new and before the first prompt.
    pub initial_config: Vec<(String, String)>,
    pub log_path: PathBuf,
    pub emit: Emit,
}

enum Command {
    Ensure,
    Prompt(String),
    Cancel,
    SetConfig(String, String),
    Authenticate(String),
    Dispose,
}

struct Pending {
    responder: Responder<acp::RequestPermissionResponse>,
    started_at: i64,
}

/// What both the session task and the permission handler touch.
struct Shared {
    emit: Emit,
    /// After dispose the thread is gone or the app is quitting; late events from the dying process must not reach the store.
    disposed: AtomicBool,
    /// session/load replays the whole conversation, which is already in the timeline.
    suppress_updates: AtomicBool,
    turn_started_at: Mutex<Option<i64>>,
    pending: Mutex<HashMap<String, Pending>>,
    log: Mutex<Option<std::fs::File>>,
}

impl Shared {
    fn emit(&self, body: ThreadEventBody) {
        if !self.disposed.load(Ordering::SeqCst) {
            (self.emit)(body);
        }
    }

    fn status(&self, status: ThreadStatus) {
        self.emit(ThreadEventBody::Status(status));
    }

    fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.emit(ThreadEventBody::Notice { level, text: text.into() });
    }

    fn settle(&self, request_id: &str, option_id: Option<String>) {
        let Some(pending) = self.pending.lock().expect("pending lock").remove(request_id) else { return };
        let (outcome, wire) = match option_id {
            Some(id) => (
                PermissionOutcome::Selected { option_id: id.clone() },
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(id)),
            ),
            None => (PermissionOutcome::Cancelled, acp::RequestPermissionOutcome::Cancelled),
        };
        self.emit(ThreadEventBody::PermissionResolved { request_id: request_id.to_owned(), outcome });
        if self.turn_started_at.lock().expect("turn lock").is_some() {
            self.status(ThreadStatus::Running { started_at: pending.started_at });
        }
        let _ = pending.responder.respond(acp::RequestPermissionResponse::new(wire));
    }

    fn cancel_pending(&self) {
        let ids: Vec<String> = self.pending.lock().expect("pending lock").keys().cloned().collect();
        for id in ids {
            self.settle(&id, None);
        }
    }

    fn log_line(&self, line: &str) {
        if let Some(file) = self.log.lock().expect("log lock").as_mut() {
            let _ = writeln!(file, "[oniko {}] {line}", iso_now());
        }
    }
}

/// The thread's handle on its session task. Dropping it does not stop the task; call [`dispose`](Self::dispose).
pub struct SessionHandle {
    tx: mpsc::UnboundedSender<Command>,
    shared: Arc<Shared>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SessionHandle {
    /// Must be called inside a Tokio runtime.
    pub fn spawn(options: SessionOptions) -> SessionHandle {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            emit: options.emit.clone(),
            disposed: AtomicBool::new(false),
            suppress_updates: AtomicBool::new(false),
            turn_started_at: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            log: Mutex::new(None),
        });
        let task = tokio::spawn(run(options, rx, shared.clone()));
        SessionHandle { tx, shared, task: Mutex::new(Some(task)) }
    }

    /// Starts the agent now rather than on the first prompt.
    pub fn ensure(&self) {
        let _ = self.tx.send(Command::Ensure);
    }

    /// The prompt shows in the timeline at once, even while the agent is still starting.
    pub fn prompt(&self, text: String) {
        self.shared.emit(ThreadEventBody::UserPrompt { id: uuid(), text: text.clone() });
        let _ = self.tx.send(Command::Prompt(text));
    }

    pub fn cancel(&self) {
        self.shared.cancel_pending();
        let _ = self.tx.send(Command::Cancel);
    }

    pub fn set_config(&self, config_id: String, value: String) {
        let _ = self.tx.send(Command::SetConfig(config_id, value));
    }

    /// `None` declines.
    pub fn respond_permission(&self, request_id: &str, option_id: Option<String>) {
        self.shared.settle(request_id, option_id);
    }

    pub fn authenticate(&self, method_id: String) {
        let _ = self.tx.send(Command::Authenticate(method_id));
    }

    /// Stops the agent; the returned task ends once its process is gone.
    pub fn dispose(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.shared.disposed.store(true, Ordering::SeqCst);
        self.shared.cancel_pending();
        let _ = self.tx.send(Command::Dispose);
        self.task.lock().expect("task lock").take()
    }
}

/// What the session remembers across agent processes.
struct State {
    options: SessionOptions,
    acp_session_id: Option<String>,
    config: Vec<ConfigControl>,
    /// A prompt sent before the agent could take it, sent once authentication succeeds.
    queued_prompt: Option<String>,
}

enum Ended {
    Disposed,
    Exited(Option<i32>),
}

async fn run(options: SessionOptions, mut rx: mpsc::UnboundedReceiver<Command>, shared: Arc<Shared>) {
    match std::fs::OpenOptions::new().create(true).append(true).open(&options.log_path) {
        Ok(file) => *shared.log.lock().expect("log lock") = Some(file),
        Err(error) => eprintln!("oniko: cannot open {}: {error}", options.log_path.display()),
    }
    let mut state = State { acp_session_id: options.acp_session_id.clone(), options, config: vec![], queued_prompt: None };
    loop {
        // Without a process, only commands that need the agent start one.
        let first = match rx.recv().await {
            None | Some(Command::Dispose) => return,
            Some(Command::Cancel | Command::Authenticate(_)) => continue,
            Some(command) => command,
        };
        match connect(&mut state, &mut rx, &shared, first).await {
            Ok(Ended::Disposed) => return,
            Ok(Ended::Exited(code)) => {
                shared.log_line(&format!("{} exited with code {code:?}", state.options.agent_id.label()));
                shared.cancel_pending();
                shared.status(ThreadStatus::Exited { code });
            }
            Err(message) => {
                shared.log_line(&format!("connect failed: {message}"));
                shared.status(ThreadStatus::Error { message });
            }
        }
        if shared.disposed.load(Ordering::SeqCst) {
            return;
        }
    }
}

fn message_of(error: &agent_client_protocol::Error) -> String {
    if error.message.is_empty() { error.to_string() } else { error.message.clone() }
}

fn is_auth_required(error: &agent_client_protocol::Error) -> bool {
    error.code == ErrorCode::AUTH_REQUIRED
}

async fn request(cx: &ConnectionTo<Agent>, method: &str, params: Value) -> Result<Value, agent_client_protocol::Error> {
    cx.send_request(UntypedMessage::new(method, params)?).block_task().await
}

/// Starts the agent process and serves it until it exits or the thread goes away.
async fn connect(
    state: &mut State,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    shared: &Arc<Shared>,
    first: Command,
) -> Result<Ended, String> {
    shared.status(ThreadStatus::Connecting);
    let spec = spawn_spec(state.options.agent_id)?;
    let env = child_env(base_env(), [&spec.env, &state.options.account_env]);
    shared.log_line(&format!("starting {} {}", spec.command, spec.args.join(" ")));
    let mut command = tokio::process::Command::new(&spec.command);
    command
        .args(&spec.args)
        .current_dir(&state.options.cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Adapters log to stderr; keeping it out of stdout keeps the ndjson stream clean.
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own process group, so stopping it also stops what it started: npx runs the adapter as a child.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|e| format!("Could not start {}: {e}", spec.command))?;
    let pid = child.id();
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        return Err("Agent process has no stdio".into());
    };
    if let Some(file) = shared.log.lock().expect("log lock").as_ref().and_then(|f| f.try_clone().ok()) {
        tokio::spawn(async move {
            let mut stderr = stderr;
            let mut file = tokio::fs::File::from_std(file);
            let _ = tokio::io::copy(&mut stderr, &mut file).await;
        });
    }
    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    let (exit_tx, mut exit_rx) = oneshot::channel::<Option<i32>>();
    tokio::spawn(async move {
        let status = tokio::select! {
            status = child.wait() => status,
            _ = kill_rx => {
                kill_group(pid);
                let _ = child.kill().await;
                child.wait().await
            }
        };
        let _ = exit_tx.send(status.ok().and_then(|s| s.code()));
    });

    let notifications = shared.clone();
    let permissions = shared.clone();
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let served = Client
        .v1()
        .name("oniko")
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                if !notifications.suppress_updates.load(Ordering::SeqCst) {
                    notifications.emit(ThreadEventBody::SessionUpdate(Box::new(notification.update)));
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: acp::RequestPermissionRequest, responder: Responder<acp::RequestPermissionResponse>, _cx| {
                let request_id = uuid();
                let started_at = permissions.turn_started_at.lock().expect("turn lock").unwrap_or_else(now_ms);
                permissions.emit(ThreadEventBody::PermissionRequest {
                    request_id: request_id.clone(),
                    tool_call: Box::new(request.tool_call),
                    options: request.options,
                });
                permissions.status(ThreadStatus::AwaitingPermission { request_id: request_id.clone(), started_at });
                // Answered later, when the user picks; the dispatch loop moves on meanwhile.
                permissions.pending.lock().expect("pending lock").insert(request_id, Pending { responder, started_at });
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async |cx: ConnectionTo<Agent>| Ok(serve(cx, state, rx, shared, first, &mut exit_rx).await))
        .await;
    let ended = served.map_err(|e| message_of(&e)).and_then(|r| r);
    if !matches!(ended, Ok(Ended::Exited(_))) {
        // Waits for the process to be gone, so quitting never leaves an agent behind.
        let _ = kill_tx.send(());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), exit_rx).await;
    }
    ended
}

#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // SAFETY: killpg only sends a signal; the group is the one the child was started in.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {}

enum Attach {
    Ready,
    AuthRequired,
}

async fn serve(
    cx: ConnectionTo<Agent>,
    state: &mut State,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    shared: &Arc<Shared>,
    first: Command,
    exit_rx: &mut oneshot::Receiver<Option<i32>>,
) -> Result<Ended, String> {
    let init = tokio::select! {
        init = request(&cx, "initialize", json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "oniko", "version": env!("CARGO_PKG_VERSION") },
        })) => init.map_err(|e| message_of(&e))?,
        code = &mut *exit_rx => return Ok(Ended::Exited(code.ok().flatten())),
    };
    let mut ready = matches!(attach(&cx, state, shared, &init).await?, Attach::Ready);
    let mut turns: FuturesUnordered<BoxFuture<'static, ()>> = FuturesUnordered::new();
    let mut next = Some(first);
    loop {
        let command = match next.take() {
            Some(command) => command,
            None => tokio::select! {
                command = rx.recv() => match command {
                    Some(command) => command,
                    None => return Ok(Ended::Disposed),
                },
                Some(()) = turns.next(), if !turns.is_empty() => continue,
                code = &mut *exit_rx => return Ok(Ended::Exited(code.ok().flatten())),
            },
        };
        match command {
            Command::Dispose => return Ok(Ended::Disposed),
            Command::Ensure => {}
            Command::Prompt(text) => {
                if ready {
                    turns.push(turn(cx.clone(), shared.clone(), state.acp_session_id.clone(), text));
                } else {
                    state.queued_prompt = Some(text);
                }
            }
            Command::Cancel => {
                if let Some(session_id) = &state.acp_session_id {
                    let _ = cx.send_notification(UntypedMessage::new("session/cancel", json!({ "sessionId": session_id })).map_err(|e| message_of(&e))?);
                }
            }
            Command::SetConfig(config_id, value) => {
                if ready {
                    write_config(&cx, state, shared, &config_id, &value).await;
                }
            }
            Command::Authenticate(method_id) => {
                if let Err(error) = request(&cx, "authenticate", json!({ "methodId": method_id })).await {
                    shared.notice(NoticeLevel::Error, format!("Authentication failed: {}", message_of(&error)));
                    continue;
                }
                shared.status(ThreadStatus::Connecting);
                ready = matches!(attach(&cx, state, shared, &init).await?, Attach::Ready);
                if ready && let Some(text) = state.queued_prompt.take() {
                    turns.push(turn(cx.clone(), shared.clone(), state.acp_session_id.clone(), text));
                }
            }
        }
    }
}

fn auth_methods(init: &Value) -> Vec<acp::AuthMethod> {
    init.get("authMethods").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

async fn attach(cx: &ConnectionTo<Agent>, state: &mut State, shared: &Arc<Shared>, init: &Value) -> Result<Attach, String> {
    let result = async {
        let previous = state.acp_session_id.clone();
        if let Some(previous) = &previous
            && let Some(setup) = reattach(cx, state, shared, init, previous).await?
        {
            attached(state, shared, previous.clone(), &setup);
            return Ok(());
        }
        let setup = request(cx, "session/new", json!({ "cwd": state.options.cwd, "mcpServers": [] })).await?;
        let session_id = setup.get("sessionId").and_then(Value::as_str).unwrap_or_default().to_owned();
        attached(state, shared, session_id, &setup);
        if previous.is_none() {
            apply_initial_config(cx, state, shared).await;
        } else {
            shared.notice(NoticeLevel::Info, FRESH_SESSION_NOTICE);
        }
        Ok::<(), agent_client_protocol::Error>(())
    }
    .await;
    match result {
        Ok(()) => Ok(Attach::Ready),
        Err(error) if is_auth_required(&error) => {
            shared.status(ThreadStatus::AuthRequired { methods: auth_methods(init) });
            Ok(Attach::AuthRequired)
        }
        Err(error) => Err(message_of(&error)),
    }
}

/// Prefers session/resume because the timeline is already ours; session/load would replay the
/// whole conversation, so its updates are dropped while it runs.
async fn reattach(
    cx: &ConnectionTo<Agent>,
    state: &State,
    shared: &Arc<Shared>,
    init: &Value,
    session_id: &str,
) -> Result<Option<Value>, agent_client_protocol::Error> {
    let caps = init.get("agentCapabilities");
    let can_resume = caps.and_then(|c| c.get("sessionCapabilities")).and_then(|c| c.get("resume")).is_some_and(|r| !r.is_null());
    let can_load = caps.and_then(|c| c.get("loadSession")).and_then(Value::as_bool).unwrap_or(false);
    let params = json!({ "sessionId": session_id, "cwd": state.options.cwd, "mcpServers": [] });
    let result = if can_resume {
        request(cx, "session/resume", params).await.map(|res| (res, "session/resume"))
    } else if can_load {
        shared.suppress_updates.store(true, Ordering::SeqCst);
        let res = request(cx, "session/load", params).await;
        shared.suppress_updates.store(false, Ordering::SeqCst);
        res.map(|res| (res, "session/load"))
    } else {
        shared.log_line("agent advertises neither session/resume nor session/load");
        return Ok(None);
    };
    match result {
        Ok((setup, via)) => {
            shared.log_line(&format!("reattached {session_id} via {via}"));
            Ok(Some(setup))
        }
        Err(error) if is_auth_required(&error) => Err(error),
        Err(error) => {
            shared.log_line(&format!("reattach {session_id} failed: {}", message_of(&error)));
            Ok(None)
        }
    }
}

fn attached(state: &mut State, shared: &Arc<Shared>, session_id: String, setup: &Value) {
    state.acp_session_id = Some(session_id.clone());
    state.config = config_from_setup(setup);
    shared.emit(ThreadEventBody::SessionAttached { acp_session_id: session_id });
    shared.emit(ThreadEventBody::Config(state.config.clone()));
    shared.status(ThreadStatus::Idle);
}

/// One at a time and in order: a mode change can replace the other config options.
async fn apply_initial_config(cx: &ConnectionTo<Agent>, state: &mut State, shared: &Arc<Shared>) {
    for (config_id, value) in std::mem::take(&mut state.options.initial_config) {
        let wanted = state
            .config
            .iter()
            .find(|c| c.id == config_id)
            .is_some_and(|c| c.value != value && c.options.iter().any(|o| o.value == value));
        if wanted {
            write_config(cx, state, shared, &config_id, &value).await;
        }
    }
}

/// How each config source is written back to the agent.
async fn write_config(cx: &ConnectionTo<Agent>, state: &mut State, shared: &Arc<Shared>, config_id: &str, value: &str) {
    let Some(control) = state.config.iter().find(|c| c.id == config_id).cloned() else { return };
    let Some(session_id) = state.acp_session_id.clone() else { return };
    let result = match control.source {
        ConfigSource::ConfigOption => request(cx, "session/set_config_option", json!({ "sessionId": session_id, "configId": control.id, "value": value }))
            .await
            .map(|res| {
                let options: Vec<acp::SessionConfigOption> =
                    res.get("configOptions").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
                Some(controls_from_config_options(&options))
            }),
        ConfigSource::Mode => request(cx, "session/set_mode", json!({ "sessionId": session_id, "modeId": value })).await.map(|_| None),
        ConfigSource::Model => request(cx, "session/set_model", json!({ "sessionId": session_id, "modelId": value })).await.map(|_| None),
    };
    match result {
        Ok(Some(mut replaced)) => {
            replaced.extend(state.config.iter().filter(|c| c.source != ConfigSource::ConfigOption).cloned());
            state.config = replaced;
        }
        Ok(None) => state.config = with_value(&state.config, config_id, value),
        Err(error) => {
            shared.notice(NoticeLevel::Error, format!("Could not change {}: {}", control.label, message_of(&error)));
            return;
        }
    }
    shared.emit(ThreadEventBody::Config(state.config.clone()));
}

fn turn(cx: ConnectionTo<Agent>, shared: Arc<Shared>, session_id: Option<String>, text: String) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let Some(session_id) = session_id else { return };
        let started_at = now_ms();
        *shared.turn_started_at.lock().expect("turn lock") = Some(started_at);
        shared.status(ThreadStatus::Running { started_at });
        let prompt = json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] });
        match request(&cx, "session/prompt", prompt).await {
            Ok(res) => {
                let stop_reason = res.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn").to_owned();
                shared.emit(ThreadEventBody::TurnEnd { stop_reason, duration_ms: now_ms() - started_at });
            }
            Err(error) => shared.notice(NoticeLevel::Error, message_of(&error)),
        }
        *shared.turn_started_at.lock().expect("turn lock") = None;
        shared.status(ThreadStatus::Idle);
    })
}

pub fn log_path_for(dir: &std::path::Path, thread_id: &str) -> PathBuf {
    dir.join(format!("{thread_id}.log"))
}
