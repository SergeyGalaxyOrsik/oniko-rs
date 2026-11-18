use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentId {
    Claude,
}

impl AgentId {
    pub const ALL: [AgentId; 1] = [AgentId::Claude];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentId::Claude => "claude",
        }
    }

    pub fn parse(value: &str) -> Option<AgentId> {
        Self::ALL.into_iter().find(|id| id.as_str() == value)
    }

    pub fn info(self) -> AgentInfo {
        match self {
            AgentId::Claude => AgentInfo {
                id: self,
                label: "Claude",
                description: "Claude Code over ACP, using your local Claude login",
            },
        }
    }

    pub fn label(self) -> &'static str {
        self.info().label
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AgentInfo {
    pub id: AgentId,
    pub label: &'static str,
    pub description: &'static str,
}
