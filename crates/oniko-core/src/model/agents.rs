use std::collections::{BTreeMap, HashMap};

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

/// Per agent and model, whether a task's agent ran in auto mode (true) or the adapter fell back to
/// accepting edits (false). A model not seen yet has no entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutoModes(HashMap<AgentId, BTreeMap<String, bool>>);

impl AutoModes {
    pub fn get(&self, agent: AgentId, model: &str) -> Option<bool> {
        self.0.get(&agent)?.get(model).copied()
    }

    /// True when it changed what was known.
    pub fn record(&mut self, agent: AgentId, model: &str, supported: bool) -> bool {
        self.0.entry(agent).or_default().insert(model.to_owned(), supported) != Some(supported)
    }

    pub fn of(&self, agent: AgentId) -> BTreeMap<String, bool> {
        self.0.get(&agent).cloned().unwrap_or_default()
    }

    pub fn set(&mut self, agent: AgentId, models: BTreeMap<String, bool>) {
        self.0.insert(agent, models);
    }
}
