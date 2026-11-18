use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::agents::AgentId;
use super::subscriptions::SubscriptionId;
use crate::acp;

pub type ThreadId = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ThreadStatus {
    Dormant,
    Connecting,
    AuthRequired { methods: Vec<acp::AuthMethod> },
    Idle,
    Running { started_at: i64 },
    AwaitingPermission { request_id: String, started_at: i64 },
    Error { message: String },
    Exited { code: Option<i32> },
}

impl ThreadStatus {
    /// Busy threads take no new prompt; the composer offers Stop instead.
    pub fn is_busy(&self) -> bool {
        matches!(self, ThreadStatus::Running { .. } | ThreadStatus::AwaitingPermission { .. } | ThreadStatus::Connecting)
    }

    pub fn started_at(&self) -> Option<i64> {
        match self {
            ThreadStatus::Running { started_at } | ThreadStatus::AwaitingPermission { started_at, .. } => Some(*started_at),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolItem {
    pub id: String,
    pub title: String,
    pub tool_kind: String,
    pub status: acp::ToolCallStatus,
    pub content: Vec<acp::ToolCallContent>,
    pub locations: Vec<Location>,
}

/// `null` while the user has not answered, `{ optionId }` once they picked, `"cancelled"` otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionOutcome {
    Selected { option_id: String },
    Cancelled,
}

impl Serialize for PermissionOutcome {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            PermissionOutcome::Cancelled => s.serialize_str("cancelled"),
            PermissionOutcome::Selected { option_id } => {
                use serde::ser::SerializeMap;
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("optionId", option_id)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for PermissionOutcome {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Selected {
                #[serde(rename = "optionId")]
                option_id: String,
            },
        }
        match Raw::deserialize(d)? {
            Raw::Selected { option_id } => Ok(PermissionOutcome::Selected { option_id }),
            Raw::Text(text) if text == "cancelled" => Ok(PermissionOutcome::Cancelled),
            Raw::Text(other) => Err(serde::de::Error::custom(format!("unknown permission outcome {other}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum TimelineItem {
    User { id: String, text: String },
    Assistant { id: String, text: String },
    Thought { id: String, text: String },
    Tool(ToolItem),
    Plan { id: String, entries: Vec<acp::PlanEntry> },
    Permission {
        id: String,
        tool_call_id: String,
        title: String,
        options: Vec<acp::PermissionOption>,
        outcome: Option<PermissionOutcome>,
    },
    TurnEnd { id: String, stop_reason: String, duration_ms: i64 },
    Notice { id: String, level: NoticeLevel, text: String },
}

impl TimelineItem {
    pub fn id(&self) -> &str {
        match self {
            TimelineItem::User { id, .. }
            | TimelineItem::Assistant { id, .. }
            | TimelineItem::Thought { id, .. }
            | TimelineItem::Plan { id, .. }
            | TimelineItem::Permission { id, .. }
            | TimelineItem::TurnEnd { id, .. }
            | TimelineItem::Notice { id, .. } => id,
            TimelineItem::Tool(tool) => &tool.id,
        }
    }

    /// The `kind` column of `timeline_items`, the same tag the JSON carries.
    pub fn kind(&self) -> &'static str {
        match self {
            TimelineItem::User { .. } => "user",
            TimelineItem::Assistant { .. } => "assistant",
            TimelineItem::Thought { .. } => "thought",
            TimelineItem::Tool(_) => "tool",
            TimelineItem::Plan { .. } => "plan",
            TimelineItem::Permission { .. } => "permission",
            TimelineItem::TurnEnd { .. } => "turn_end",
            TimelineItem::Notice { .. } => "notice",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    ConfigOption,
    Mode,
    Model,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigChoice {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigControl {
    pub id: String,
    pub label: String,
    /// ACP's semantic category (model, mode, thought_level, model_config, ...), when the agent gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub source: ConfigSource,
    pub value: String,
    pub options: Vec<ConfigChoice>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub id: ThreadId,
    pub agent_id: AgentId,
    /// Fixed at creation: the agent's transcript lives in this subscription's config dir, so resume only works there.
    pub subscription_id: SubscriptionId,
    pub cwd: String,
    pub title: String,
    pub created_at: i64,
    pub acp_session_id: Option<String>,
    pub status: ThreadStatus,
    pub timeline: Vec<TimelineItem>,
    pub config: Vec<ConfigControl>,
    pub commands: Vec<acp::AvailableCommand>,
}

pub const INTERRUPTED_NOTICE: &str = "Interrupted when Oniko closed";

impl Thread {
    /// A thread read back from disk has no live agent process, so it becomes dormant until the next prompt.
    pub fn restored(mut self) -> Thread {
        let interrupted = matches!(
            self.status,
            ThreadStatus::Running { .. } | ThreadStatus::Connecting | ThreadStatus::AwaitingPermission { .. }
        );
        for item in &mut self.timeline {
            if let TimelineItem::Permission { outcome: outcome @ None, .. } = item {
                *outcome = Some(PermissionOutcome::Cancelled);
            }
        }
        if interrupted {
            let id = format!("notice-{}", self.timeline.len());
            self.timeline.push(TimelineItem::Notice { id, level: NoticeLevel::Info, text: INTERRUPTED_NOTICE.into() });
        }
        self.status = ThreadStatus::Dormant;
        self
    }
}
