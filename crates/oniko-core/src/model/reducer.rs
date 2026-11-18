//! Applies events to threads. Main and the UI each run this on their own copy, so both compute the
//! same timeline ids independently. Every change reports which timeline rows it touched, which is
//! all the database needs to write: a streaming chunk rewrites one row.

use std::collections::HashMap;

use super::agents::AutoModes;
use super::config::{controls_from_config_options, with_value};
use super::events::{AppEvent, ThreadEvent, ThreadEventBody};
use super::subscriptions::SubscriptionsState;
use super::tasks::TasksState;
use super::thread::{ConfigSource, Location, NoticeLevel, Thread, ThreadId, TimelineItem, ToolItem};
use crate::acp;

const TITLE_LIMIT: usize = 80;

pub type ThreadMap = HashMap<ThreadId, Thread>;

/// What one event did to the thread map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Unchanged,
    Created,
    /// The thread row changed and so did these timeline positions.
    Updated { items: Vec<usize> },
    Removed,
}

fn text_of(block: &acp::ContentBlock) -> &str {
    match block {
        acp::ContentBlock::Text(text) => &text.text,
        _ => "",
    }
}

/// Ids derive from timeline position so main and the UI compute the same id independently.
fn next_id(thread: &Thread, prefix: &str) -> String {
    format!("{prefix}-{}", thread.timeline.len())
}

fn push(thread: &mut Thread, item: TimelineItem) -> Outcome {
    thread.timeline.push(item);
    Outcome::Updated { items: vec![thread.timeline.len() - 1] }
}

fn append_text(thread: &mut Thread, assistant: bool, text: &str) -> Outcome {
    if text.is_empty() {
        return Outcome::Unchanged;
    }
    let last = thread.timeline.len().checked_sub(1);
    match thread.timeline.last_mut() {
        Some(TimelineItem::Assistant { text: existing, .. }) if assistant => existing.push_str(text),
        Some(TimelineItem::Thought { text: existing, .. }) if !assistant => existing.push_str(text),
        _ => {
            let id = next_id(thread, if assistant { "assistant" } else { "thought" });
            let text = text.to_owned();
            return push(thread, if assistant { TimelineItem::Assistant { id, text } } else { TimelineItem::Thought { id, text } });
        }
    }
    Outcome::Updated { items: last.into_iter().collect() }
}

fn kind_name(kind: &acp::ToolKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| "other".into())
}

fn locations(locations: &[acp::ToolCallLocation]) -> Vec<Location> {
    locations.iter().map(|l| Location { path: l.path.to_string_lossy().into_owned(), line: l.line }).collect()
}

/// Returns the tool's timeline position.
fn upsert_tool(thread: &mut Thread, call: &acp::ToolCallUpdate) -> usize {
    let id = call.tool_call_id.0.to_string();
    let index = thread.timeline.iter().position(|item| matches!(item, TimelineItem::Tool(t) if t.id == id));
    let existing = index.and_then(|i| match &thread.timeline[i] {
        TimelineItem::Tool(tool) => Some(tool.clone()),
        _ => None,
    });
    let f = &call.fields;
    let merged = ToolItem {
        title: f.title.clone().or_else(|| existing.as_ref().map(|e| e.title.clone())).unwrap_or_else(|| "Tool call".into()),
        tool_kind: f.kind.as_ref().map(kind_name).or_else(|| existing.as_ref().map(|e| e.tool_kind.clone())).unwrap_or_else(|| "other".into()),
        status: f.status.or(existing.as_ref().map(|e| e.status)).unwrap_or_default(),
        content: f.content.clone().or_else(|| existing.as_ref().map(|e| e.content.clone())).unwrap_or_default(),
        locations: f.locations.as_deref().map(locations).or_else(|| existing.as_ref().map(|e| e.locations.clone())).unwrap_or_default(),
        id,
    };
    match index {
        Some(i) => {
            thread.timeline[i] = TimelineItem::Tool(merged);
            i
        }
        None => {
            thread.timeline.push(TimelineItem::Tool(merged));
            thread.timeline.len() - 1
        }
    }
}

fn last_user_index(timeline: &[TimelineItem]) -> Option<usize> {
    timeline.iter().rposition(|item| matches!(item, TimelineItem::User { .. }))
}

fn apply_session_update(thread: &mut Thread, update: &acp::SessionUpdate) -> Outcome {
    use acp::SessionUpdate as U;
    match update {
        U::AgentMessageChunk(chunk) => append_text(thread, true, text_of(&chunk.content)),
        U::AgentThoughtChunk(chunk) => append_text(thread, false, text_of(&chunk.content)),
        U::ToolCall(call) => {
            let index = upsert_tool(thread, &acp::ToolCallUpdate::from(call.clone()));
            Outcome::Updated { items: vec![index] }
        }
        U::ToolCallUpdate(call) => {
            let index = upsert_tool(thread, call);
            Outcome::Updated { items: vec![index] }
        }
        U::Plan(plan) => {
            let turn_start = last_user_index(&thread.timeline);
            let index = thread
                .timeline
                .iter()
                .enumerate()
                .position(|(i, item)| turn_start.is_none_or(|start| i > start) && matches!(item, TimelineItem::Plan { .. }));
            match index {
                None => {
                    let id = next_id(thread, "plan");
                    push(thread, TimelineItem::Plan { id, entries: plan.entries.clone() })
                }
                Some(i) => {
                    let id = thread.timeline[i].id().to_owned();
                    thread.timeline[i] = TimelineItem::Plan { id, entries: plan.entries.clone() };
                    Outcome::Updated { items: vec![i] }
                }
            }
        }
        U::AvailableCommandsUpdate(update) => {
            thread.commands = update.available_commands.clone();
            Outcome::Updated { items: vec![] }
        }
        U::CurrentModeUpdate(update) => {
            let Some(legacy) = thread.config.iter().find(|c| c.source == ConfigSource::Mode) else { return Outcome::Unchanged };
            thread.config = with_value(&thread.config, &legacy.id.clone(), &update.current_mode_id.0);
            Outcome::Updated { items: vec![] }
        }
        U::ConfigOptionUpdate(update) => {
            let mut config = controls_from_config_options(&update.config_options);
            config.extend(thread.config.iter().filter(|c| c.source != ConfigSource::ConfigOption).cloned());
            thread.config = config;
            Outcome::Updated { items: vec![] }
        }
        U::Notice(notice) => {
            let text = match &notice.description {
                Some(description) => format!("{}: {description}", notice.title),
                None => notice.title.clone(),
            };
            let level = if notice.severity == acp::NoticeSeverity::Error { NoticeLevel::Error } else { NoticeLevel::Info };
            let id = next_id(thread, "notice");
            push(thread, TimelineItem::Notice { id, level, text })
        }
        // Our own user_prompt event records the user turn, and the rest carry nothing the timeline shows.
        _ => Outcome::Unchanged,
    }
}

fn reduce_thread(thread: &mut Thread, body: &ThreadEventBody) -> Outcome {
    match body {
        ThreadEventBody::Status(status) => {
            if thread.status == *status {
                return Outcome::Unchanged;
            }
            thread.status = status.clone();
            Outcome::Updated { items: vec![] }
        }
        ThreadEventBody::SessionAttached { acp_session_id } => {
            thread.acp_session_id = Some(acp_session_id.clone());
            Outcome::Updated { items: vec![] }
        }
        ThreadEventBody::SessionUpdate(update) => apply_session_update(thread, update),
        ThreadEventBody::UserPrompt { id, text } => {
            if thread.title.is_empty() {
                let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
                thread.title = collapsed.chars().take(TITLE_LIMIT).collect();
            }
            push(thread, TimelineItem::User { id: id.clone(), text: text.clone() })
        }
        ThreadEventBody::PermissionRequest { request_id, tool_call, options } => {
            let tool_index = upsert_tool(thread, tool_call);
            let tool_title = match &thread.timeline[tool_index] {
                TimelineItem::Tool(tool) => Some(tool.title.clone()),
                _ => None,
            };
            thread.timeline.push(TimelineItem::Permission {
                id: request_id.clone(),
                tool_call_id: tool_call.tool_call_id.0.to_string(),
                title: tool_call.fields.title.clone().or(tool_title).unwrap_or_else(|| "Permission requested".into()),
                options: options.clone(),
                outcome: None,
            });
            Outcome::Updated { items: vec![tool_index, thread.timeline.len() - 1] }
        }
        ThreadEventBody::PermissionResolved { request_id, outcome } => {
            let Some(index) = thread.timeline.iter().position(|item| matches!(item, TimelineItem::Permission { id, .. } if id == request_id))
            else {
                return Outcome::Unchanged;
            };
            if let TimelineItem::Permission { outcome: slot, .. } = &mut thread.timeline[index] {
                *slot = Some(outcome.clone());
            }
            Outcome::Updated { items: vec![index] }
        }
        ThreadEventBody::TurnEnd { stop_reason, duration_ms } => {
            let id = next_id(thread, "turn");
            push(thread, TimelineItem::TurnEnd { id, stop_reason: stop_reason.clone(), duration_ms: *duration_ms })
        }
        ThreadEventBody::Config(config) => {
            thread.config = config.clone();
            Outcome::Updated { items: vec![] }
        }
        ThreadEventBody::Notice { level, text } => {
            let id = next_id(thread, "notice");
            push(thread, TimelineItem::Notice { id, level: *level, text: text.clone() })
        }
        ThreadEventBody::ThreadCreated(_) | ThreadEventBody::ThreadRemoved => Outcome::Unchanged,
    }
}

pub fn reduce_threads(threads: &mut ThreadMap, event: &ThreadEvent) -> Outcome {
    match &event.body {
        ThreadEventBody::ThreadCreated(thread) => {
            threads.insert(event.thread_id.clone(), (**thread).clone());
            Outcome::Created
        }
        ThreadEventBody::ThreadRemoved => {
            if threads.remove(&event.thread_id).is_some() { Outcome::Removed } else { Outcome::Unchanged }
        }
        body => match threads.get_mut(&event.thread_id) {
            Some(thread) => reduce_thread(thread, body),
            None => Outcome::Unchanged,
        },
    }
}

/// The UI's whole picture of the app, kept current by applying every broadcast event.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub threads: ThreadMap,
    pub subscriptions: SubscriptionsState,
    pub tasks: TasksState,
    pub auto_modes: AutoModes,
}

impl AppState {
    pub fn apply(&mut self, event: &AppEvent) {
        match event {
            AppEvent::Subscriptions(state) => self.subscriptions = state.clone(),
            AppEvent::Tasks(state) => self.tasks = state.clone(),
            AppEvent::AutoModes(state) => self.auto_modes = state.clone(),
            AppEvent::Thread(event) => {
                reduce_threads(&mut self.threads, event);
            }
            AppEvent::OpenThread(_) => {}
        }
    }

    /// Newest first, the order the sidebar shows.
    pub fn thread_list(&self) -> Vec<&Thread> {
        let mut list: Vec<&Thread> = self.threads.values().collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        list
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::agents::AgentId;
    use crate::model::thread::{PermissionOutcome, ThreadStatus};

    pub const THREAD_ID: &str = "t-1";
    pub const SUBSCRIPTION_ID: &str = "sub-1";

    pub fn new_thread() -> Thread {
        Thread {
            id: THREAD_ID.into(),
            agent_id: AgentId::Claude,
            subscription_id: SUBSCRIPTION_ID.into(),
            cwd: "/tmp/project".into(),
            title: String::new(),
            created_at: 1_000,
            acp_session_id: None,
            status: ThreadStatus::Connecting,
            timeline: vec![],
            config: vec![],
            commands: vec![],
        }
    }

    pub fn event(body: ThreadEventBody) -> ThreadEvent {
        ThreadEvent { thread_id: THREAD_ID.into(), body }
    }

    pub fn update(json: serde_json::Value) -> ThreadEvent {
        event(ThreadEventBody::SessionUpdate(Box::new(serde_json::from_value(json).expect("valid session update"))))
    }

    pub fn chunk(text: &str) -> ThreadEvent {
        update(serde_json::json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }))
    }

    fn reduce(events: impl IntoIterator<Item = ThreadEvent>) -> Thread {
        let mut map = ThreadMap::new();
        map.insert(THREAD_ID.into(), new_thread());
        for e in events {
            reduce_threads(&mut map, &e);
        }
        map.remove(THREAD_ID).expect("thread still exists")
    }

    fn kinds(thread: &Thread) -> Vec<&'static str> {
        thread.timeline.iter().map(TimelineItem::kind).collect()
    }

    #[test]
    fn merges_consecutive_agent_message_chunks_into_one_assistant_item() {
        let thread = reduce([chunk("Hel"), chunk("lo")]);
        assert_eq!(thread.timeline, vec![TimelineItem::Assistant { id: "assistant-0".into(), text: "Hello".into() }]);
    }

    #[test]
    fn starts_a_new_assistant_item_after_a_tool_call_interrupts_the_text() {
        let thread = reduce([
            chunk("a"),
            update(serde_json::json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Read" })),
            chunk("b"),
        ]);
        assert_eq!(kinds(&thread), ["assistant", "tool", "assistant"]);
        assert_eq!(thread.timeline[2].id(), "assistant-2");
    }

    #[test]
    fn keeps_thoughts_separate_from_assistant_text() {
        let thread = reduce([
            update(serde_json::json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "hmm" } })),
            chunk("answer"),
        ]);
        assert_eq!(kinds(&thread), ["thought", "assistant"]);
    }

    #[test]
    fn merges_tool_call_update_into_the_existing_tool_item_by_id() {
        let thread = reduce([
            update(serde_json::json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Edit a.ts", "kind": "edit", "status": "pending",
                "locations": [{ "path": "/a.ts", "line": 3 }] })),
            update(serde_json::json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "done" } }] })),
        ]);
        let TimelineItem::Tool(tool) = &thread.timeline[0] else { panic!("expected a tool") };
        assert_eq!(thread.timeline.len(), 1);
        assert_eq!(tool.title, "Edit a.ts");
        assert_eq!(tool.tool_kind, "edit");
        assert_eq!(tool.status, acp::ToolCallStatus::Completed);
        assert_eq!(tool.content.len(), 1);
        assert_eq!(tool.locations, vec![Location { path: "/a.ts".into(), line: Some(3) }]);
    }

    #[test]
    fn records_a_permission_request_and_then_its_resolution() {
        let tool_call: acp::ToolCallUpdate = serde_json::from_value(serde_json::json!({ "toolCallId": "c1", "title": "Run ls" })).unwrap();
        let options: Vec<acp::PermissionOption> =
            serde_json::from_value(serde_json::json!([{ "optionId": "allow", "name": "Allow", "kind": "allow_once" }])).unwrap();
        let thread = reduce([
            event(ThreadEventBody::PermissionRequest { request_id: "r1".into(), tool_call: Box::new(tool_call), options }),
            event(ThreadEventBody::PermissionResolved {
                request_id: "r1".into(),
                outcome: PermissionOutcome::Selected { option_id: "allow".into() },
            }),
        ]);
        assert_eq!(kinds(&thread), ["tool", "permission"]);
        let TimelineItem::Permission { title, outcome, tool_call_id, .. } = &thread.timeline[1] else { panic!() };
        assert_eq!(title, "Run ls");
        assert_eq!(tool_call_id, "c1");
        assert_eq!(outcome, &Some(PermissionOutcome::Selected { option_id: "allow".into() }));
    }

    #[test]
    fn titles_the_thread_from_the_first_prompt_only() {
        let thread = reduce([
            event(ThreadEventBody::UserPrompt { id: "u1".into(), text: "  fix\n the   bug ".into() }),
            event(ThreadEventBody::UserPrompt { id: "u2".into(), text: "other".into() }),
        ]);
        assert_eq!(thread.title, "fix the bug");
    }

    #[test]
    fn replaces_the_plan_within_a_turn_and_starts_a_new_one_next_turn() {
        let plan = |content: &str| {
            update(serde_json::json!({ "sessionUpdate": "plan", "entries": [{ "content": content, "priority": "high", "status": "pending" }] }))
        };
        let thread = reduce([
            event(ThreadEventBody::UserPrompt { id: "u1".into(), text: "a".into() }),
            plan("one"),
            plan("two"),
            event(ThreadEventBody::UserPrompt { id: "u2".into(), text: "b".into() }),
            plan("three"),
        ]);
        assert_eq!(kinds(&thread), ["user", "plan", "user", "plan"]);
        let TimelineItem::Plan { entries, .. } = &thread.timeline[1] else { panic!() };
        assert_eq!(entries[0].content, "two");
    }

    #[test]
    fn replaces_config_controls_from_config_option_update_and_keeps_legacy_ones() {
        let mut start = new_thread();
        start.config = crate::model::config::config_from_setup(
            &serde_json::json!({ "modes": { "currentModeId": "a", "availableModes": [{ "id": "a", "name": "A" }, { "id": "b", "name": "B" }] } }),
        );
        let mut map = ThreadMap::new();
        map.insert(THREAD_ID.into(), start);
        reduce_threads(&mut map, &update(serde_json::json!({ "sessionUpdate": "config_option_update", "configOptions": [
            { "id": "model", "name": "Model", "type": "select", "currentValue": "x", "options": [{ "value": "x", "name": "X" }] }
        ] })));
        reduce_threads(&mut map, &update(serde_json::json!({ "sessionUpdate": "current_mode_update", "currentModeId": "b" })));
        let thread = &map[THREAD_ID];
        let ids: Vec<(&str, &str)> = thread.config.iter().map(|c| (c.id.as_str(), c.value.as_str())).collect();
        assert_eq!(ids, [("model", "x"), ("mode", "b")]);
    }

    #[test]
    fn ignores_events_for_unknown_threads_and_removes_threads() {
        let mut map = ThreadMap::new();
        assert_eq!(reduce_threads(&mut map, &chunk("x")), Outcome::Unchanged);
        map.insert(THREAD_ID.into(), new_thread());
        assert_eq!(reduce_threads(&mut map, &event(ThreadEventBody::ThreadRemoved)), Outcome::Removed);
        assert!(map.is_empty());
    }

    #[test]
    fn restore_marks_an_interrupted_thread_and_cancels_its_open_permission() {
        let mut thread = new_thread();
        thread.status = ThreadStatus::AwaitingPermission { request_id: "r".into(), started_at: 1 };
        thread.timeline.push(TimelineItem::Permission {
            id: "r".into(),
            tool_call_id: "c".into(),
            title: "x".into(),
            options: vec![],
            outcome: None,
        });
        let restored = thread.restored();
        assert_eq!(restored.status, ThreadStatus::Dormant);
        assert!(matches!(&restored.timeline[0], TimelineItem::Permission { outcome: Some(PermissionOutcome::Cancelled), .. }));
        assert!(matches!(&restored.timeline[1], TimelineItem::Notice { id, .. } if id == "notice-1"));
        let mut idle = new_thread();
        idle.status = ThreadStatus::Idle;
        assert!(idle.restored().timeline.is_empty());
    }

    #[test]
    fn timeline_json_matches_the_electron_shape() {
        let item = TimelineItem::TurnEnd { id: "turn-3".into(), stop_reason: "end_turn".into(), duration_ms: 12 };
        assert_eq!(serde_json::to_string(&item).unwrap(), r#"{"kind":"turn_end","id":"turn-3","stopReason":"end_turn","durationMs":12}"#);
        let permission: TimelineItem = serde_json::from_str(
            r#"{"kind":"permission","id":"r","toolCallId":"c","title":"t","options":[],"outcome":{"optionId":"o"}}"#,
        )
        .unwrap();
        assert!(matches!(permission, TimelineItem::Permission { outcome: Some(PermissionOutcome::Selected { .. }), .. }));
        let status: ThreadStatus = serde_json::from_str(r#"{"kind":"awaiting_permission","requestId":"r","startedAt":5}"#).unwrap();
        assert_eq!(status, ThreadStatus::AwaitingPermission { request_id: "r".into(), started_at: 5 });
    }
}
