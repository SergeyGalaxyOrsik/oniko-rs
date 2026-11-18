use serde::{Deserialize, Serialize};

use super::agents::AgentId;

pub type SubscriptionId = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum AccountStatus {
    Unknown,
    Checking,
    LoggedIn { email: Option<String>, org_name: Option<String>, plan: Option<String>, checked_at: i64 },
    LoggedOut { checked_at: i64 },
    Error { message: String, checked_at: i64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Subscription {
    pub id: SubscriptionId,
    pub agent_id: AgentId,
    pub label: String,
    /// Absolute, `~` expanded, normalized; unique per agent.
    pub config_dir: String,
    pub created_at: i64,
    pub account: AccountStatus,
    /// Derived from the agent's account spec, never stored: the env rule lives only there.
    pub login_command: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SubscriptionsState {
    pub subscriptions: Vec<Subscription>,
    /// The subscription a new thread starts on.
    pub active_subscription_id: Option<SubscriptionId>,
}

impl SubscriptionsState {
    pub fn active(&self) -> Option<&Subscription> {
        let id = self.active_subscription_id.as_ref()?;
        self.get(id)
    }

    pub fn get(&self, id: &str) -> Option<&Subscription> {
        self.subscriptions.iter().find(|s| s.id == id)
    }
}

pub fn plan_label(plan: Option<&str>) -> Option<String> {
    let plan = plan.filter(|p| !p.is_empty())?;
    let mut chars = plan.chars();
    let first = chars.next()?;
    Some(format!("{}{} plan", first.to_uppercase(), chars.as_str()))
}

/// "email · Max plan · Org", skipping whatever the CLI did not report. A personal account's org is
/// named "<email>'s Organization", which repeats the email, so it is left out.
pub fn account_line(account: &AccountStatus) -> Option<String> {
    let AccountStatus::LoggedIn { email, org_name, plan, .. } = account else { return None };
    let personal = email.as_ref().map(|e| format!("{e}'s Organization"));
    let org = org_name.clone().filter(|o| !o.is_empty() && Some(o) != personal.as_ref());
    let parts: Vec<String> = [email.clone().filter(|e| !e.is_empty()), plan_label(plan.as_deref()), org].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_line_skips_the_personal_org() {
        let account = AccountStatus::LoggedIn {
            email: Some("a@b.c".into()),
            org_name: Some("a@b.c's Organization".into()),
            plan: Some("max".into()),
            checked_at: 1,
        };
        assert_eq!(account_line(&account).as_deref(), Some("a@b.c · Max plan"));
        let team = AccountStatus::LoggedIn { email: None, org_name: Some("Acme".into()), plan: None, checked_at: 1 };
        assert_eq!(account_line(&team).as_deref(), Some("Acme"));
        assert_eq!(account_line(&AccountStatus::Unknown), None);
    }
}
