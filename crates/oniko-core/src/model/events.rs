use super::agents::AutoModes;
use super::subscriptions::SubscriptionsState;
use super::tasks::TasksState;
use super::thread::{ConfigControl, NoticeLevel, PermissionOutcome, Thread, ThreadId, ThreadStatus};
use crate::acp;

#[derive(Debug, Clone)]
pub enum ThreadEventBody {
    ThreadCreated(Box<Thread>),
    Status(ThreadStatus),
    SessionAttached { acp_session_id: String },
    SessionUpdate(Box<acp::SessionUpdate>),
    UserPrompt { id: String, text: String },
    PermissionRequest { request_id: String, tool_call: Box<acp::ToolCallUpdate>, options: Vec<acp::PermissionOption> },
    PermissionResolved { request_id: String, outcome: PermissionOutcome },
    TurnEnd { stop_reason: String, duration_ms: i64 },
    Config(Vec<ConfigControl>),
    Notice { level: NoticeLevel, text: String },
    ThreadRemoved,
}

#[derive(Debug, Clone)]
pub struct ThreadEvent {
    pub thread_id: ThreadId,
    pub body: ThreadEventBody,
}

/// Subscriptions and tasks are small and app-wide, so their events carry the whole replacement state.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Thread(ThreadEvent),
    Subscriptions(SubscriptionsState),
    Tasks(TasksState),
    AutoModes(AutoModes),
    /// A clicked notification asks the UI to show this thread.
    OpenThread(ThreadId),
}

/// Every broadcast event carries the bus's sequence number, and a snapshot records the last one
/// it includes, so the UI can tell which events a snapshot already reflects.
#[derive(Debug, Clone)]
pub struct SequencedEvent {
    pub seq: u64,
    pub event: AppEvent,
}

#[derive(Debug, Clone)]
pub struct AppSnapshot {
    pub seq: u64,
    pub home: String,
    pub threads: Vec<Thread>,
    pub subscriptions: SubscriptionsState,
    pub tasks: TasksState,
    pub auto_modes: AutoModes,
}
