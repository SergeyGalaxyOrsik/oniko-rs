use serde::Deserialize;

use super::agents::AgentId;
use super::thread::{ConfigChoice, ConfigControl, ConfigSource, Thread};
use crate::acp;

/// The pre-configOptions `models` field some adapters (Codex) still return from session/new.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyModelState {
    pub current_model_id: String,
    pub available_models: Vec<LegacyModel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyModel {
    pub model_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

fn category_name(category: &acp::SessionConfigOptionCategory) -> Option<String> {
    serde_json::to_value(category).ok()?.as_str().map(str::to_owned)
}

pub fn controls_from_config_options(options: &[acp::SessionConfigOption]) -> Vec<ConfigControl> {
    options
        .iter()
        .filter_map(|option| {
            let acp::SessionConfigKind::Select(select) = &option.kind else { return None };
            let flat: Vec<&acp::SessionConfigSelectOption> = match &select.options {
                acp::SessionConfigSelectOptions::Ungrouped(options) => options.iter().collect(),
                acp::SessionConfigSelectOptions::Grouped(groups) => groups.iter().flat_map(|g| g.options.iter()).collect(),
                _ => Vec::new(),
            };
            Some(ConfigControl {
                id: option.id.0.to_string(),
                label: option.name.clone(),
                category: option.category.as_ref().and_then(category_name),
                source: ConfigSource::ConfigOption,
                value: select.current_value.0.to_string(),
                options: flat
                    .into_iter()
                    .map(|o| ConfigChoice { value: o.value.0.to_string(), label: o.name.clone(), description: o.description.clone() })
                    .collect(),
            })
        })
        .collect()
}

pub fn control_from_modes(modes: &acp::SessionModeState) -> ConfigControl {
    ConfigControl {
        id: "mode".into(),
        label: "Mode".into(),
        category: Some("mode".into()),
        source: ConfigSource::Mode,
        value: modes.current_mode_id.0.to_string(),
        options: modes
            .available_modes
            .iter()
            .map(|m| ConfigChoice { value: m.id.0.to_string(), label: m.name.clone(), description: m.description.clone() })
            .collect(),
    }
}

pub fn control_from_models(models: &LegacyModelState) -> ConfigControl {
    ConfigControl {
        id: "model".into(),
        label: "Model".into(),
        category: Some("model".into()),
        source: ConfigSource::Model,
        value: models.current_model_id.clone(),
        options: models
            .available_models
            .iter()
            .map(|m| ConfigChoice { value: m.model_id.clone(), label: m.name.clone(), description: m.description.clone() })
            .collect(),
    }
}

/// configOptions supersede the legacy fields; legacy controls fill only what configOptions lack.
pub fn normalize_config(
    config_options: Option<&[acp::SessionConfigOption]>,
    modes: Option<&acp::SessionModeState>,
    models: Option<&LegacyModelState>,
) -> Vec<ConfigControl> {
    let mut controls = config_options.map(controls_from_config_options).unwrap_or_default();
    let has = |controls: &[ConfigControl], id: &str| controls.iter().any(|c| c.id == id);
    if let Some(modes) = modes
        && !has(&controls, "mode")
    {
        controls.push(control_from_modes(modes));
    }
    if let Some(models) = models
        && !has(&controls, "model")
    {
        controls.push(control_from_models(models));
    }
    controls
}

/// session/new, session/load and session/resume all answer with these fields, read leniently from the raw JSON.
pub fn config_from_setup(response: &serde_json::Value) -> Vec<ConfigControl> {
    let field = |name: &str| response.get(name).filter(|v| !v.is_null()).cloned();
    let options: Option<Vec<acp::SessionConfigOption>> = field("configOptions").and_then(|v| serde_json::from_value(v).ok());
    let modes: Option<acp::SessionModeState> = field("modes").and_then(|v| serde_json::from_value(v).ok());
    let models: Option<LegacyModelState> = field("models").and_then(|v| serde_json::from_value(v).ok());
    normalize_config(options.as_deref(), modes.as_ref(), models.as_ref())
}

pub fn with_value(config: &[ConfigControl], id: &str, value: &str) -> Vec<ConfigControl> {
    config.iter().cloned().map(|mut c| {
        if c.id == id {
            c.value = value.to_owned();
        }
        c
    }).collect()
}

/// Controls for a thread that has no session yet: the newest thread on the same subscription, else
/// on the same agent, carrying its last values. Empty until that agent has run once.
pub fn new_thread_config<'a>(threads: impl IntoIterator<Item = &'a Thread>, agent_id: AgentId, subscription_id: &str) -> Vec<ConfigControl> {
    let mut known: Vec<&Thread> = threads.into_iter().filter(|t| t.agent_id == agent_id && !t.config.is_empty()).collect();
    known.sort_by_key(|t| std::cmp::Reverse(t.created_at));
    known
        .iter()
        .find(|t| t.subscription_id == subscription_id)
        .or(known.first())
        .map(|t| t.config.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_options_win_over_legacy_fields() {
        let response = json!({
            "sessionId": "s",
            "configOptions": [{
                "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "plan",
                "options": [{ "value": "plan", "name": "Plan" }, { "value": "auto", "name": "Auto", "description": "Go" }]
            }, {
                "id": "effort", "name": "Effort", "category": "thought_level", "type": "select", "currentValue": "high",
                "options": [{ "group": "g", "name": "G", "options": [{ "value": "high", "name": "High" }] }]
            }],
            "modes": { "currentModeId": "default", "availableModes": [{ "id": "default", "name": "Default" }] },
            "models": { "currentModelId": "m1", "availableModels": [{ "modelId": "m1", "name": "M1" }] }
        });
        let controls = config_from_setup(&response);
        let ids: Vec<&str> = controls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["mode", "effort", "model"]);
        assert_eq!(controls[0].source, ConfigSource::ConfigOption);
        assert_eq!(controls[0].options[1].description.as_deref(), Some("Go"));
        assert_eq!(controls[1].category.as_deref(), Some("thought_level"));
        assert_eq!(controls[1].options[0].value, "high");
        assert_eq!(controls[2].source, ConfigSource::Model);
    }

    #[test]
    fn legacy_modes_fill_in() {
        let response = json!({ "modes": { "currentModeId": "a", "availableModes": [{ "id": "a", "name": "A" }] } });
        let controls = config_from_setup(&response);
        assert_eq!(controls.len(), 1);
        assert_eq!(controls[0].source, ConfigSource::Mode);
        assert_eq!(with_value(&controls, "mode", "b")[0].value, "b");
    }
}
