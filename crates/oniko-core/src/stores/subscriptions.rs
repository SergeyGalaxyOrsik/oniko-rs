use std::path::Path;
use std::rc::Rc;

use anyhow::{Result, bail};

use crate::db::{Db, StoredSubscription};
use crate::model::{AccountStatus, AgentId, Subscription, SubscriptionId, SubscriptionsState};
use crate::registry::{AccountSpecs, normalize_config_dir};
use crate::util::{basename, home_dir, is_directory, now_ms, uuid};

const ACTIVE_KEY: &str = "activeSubscriptionId";
const SEEDED_KEY: &str = "subscriptionsSeeded";
pub const DEFAULT_LABEL: &str = "Default";

fn default_label(config_dir: &str) -> String {
    let name = basename(config_dir);
    let name = name.strip_prefix('.').unwrap_or(name);
    if name.is_empty() { config_dir.to_owned() } else { name.to_owned() }
}

/// A subscription whose login is being checked.
pub struct ProbeTarget {
    pub id: SubscriptionId,
    pub agent_id: AgentId,
    pub config_dir: String,
}

/// Subscriptions, the active one, and their last known login.
pub struct SubscriptionStore {
    db: Rc<Db>,
    specs: AccountSpecs,
    subs: Vec<Subscription>,
    active_id: Option<SubscriptionId>,
    home: std::path::PathBuf,
}

impl SubscriptionStore {
    /// `seed` is the agent's default config dir, added once as "Default" when it exists.
    pub fn new(db: Rc<Db>, specs: AccountSpecs, seed: Option<(AgentId, String)>) -> Result<SubscriptionStore> {
        Self::seed_once(&db, seed)?;
        let mut store = SubscriptionStore { db, specs, subs: vec![], active_id: None, home: home_dir() };
        // A probe in flight when Oniko quit never finished, so its "checking" means nothing now.
        store.subs = store
            .db
            .load_subscriptions()?
            .into_iter()
            .map(|mut s| {
                if s.account == AccountStatus::Checking {
                    s.account = AccountStatus::Unknown;
                }
                store.view(s)
            })
            .collect();
        let stored: Option<String> = store.db.get_setting(ACTIVE_KEY);
        store.active_id =
            store.subs.iter().find(|s| Some(&s.id) == stored.as_ref()).or(store.subs.first()).map(|s| s.id.clone());
        Ok(store)
    }

    #[cfg(test)]
    pub fn with_home(mut self, home: &Path) -> Self {
        self.home = home.to_path_buf();
        self
    }

    pub fn specs(&self) -> &AccountSpecs {
        &self.specs
    }

    pub fn state(&self) -> SubscriptionsState {
        SubscriptionsState { subscriptions: self.subs.clone(), active_subscription_id: self.active_id.clone() }
    }

    pub fn get(&self, id: &str) -> Option<&Subscription> {
        self.subs.iter().find(|s| s.id == id)
    }

    pub fn active(&self) -> Option<&Subscription> {
        self.active_id.as_deref().and_then(|id| self.get(id))
    }

    /// Validation failures come back as the message the dialog shows. On success, also says
    /// whether threads that predate subscriptions were just given this one.
    pub fn add(&mut self, agent_id: AgentId, config_dir: &str, label: Option<&str>) -> Result<(SubscriptionId, bool), String> {
        let Some(config_dir) = normalize_config_dir(config_dir, &self.home) else {
            return Err("Enter an absolute folder path, like ~/.claude-work".into());
        };
        if !is_directory(Path::new(&config_dir)) {
            return Err(format!("No folder at {config_dir}"));
        }
        if let Some(duplicate) = self.subs.iter().find(|s| s.agent_id == agent_id && s.config_dir == config_dir) {
            return Err(format!("This folder is already added as “{}”", duplicate.label));
        }
        let label = label.map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).unwrap_or_else(|| default_label(&config_dir));
        let sub = self.view(StoredSubscription { id: uuid(), agent_id, label, config_dir, created_at: now_ms(), account: AccountStatus::Unknown });
        self.db.save_subscription(&StoredSubscription::from(&sub)).map_err(|e| format!("{e:#}"))?;
        let id = sub.id.clone();
        self.subs.push(sub);
        if self.active_id.is_none() {
            self.set_active_id(Some(id.clone()));
        }
        let adopted = self.db.assign_orphan_threads(&id).map_err(|e| format!("{e:#}"))? > 0;
        Ok((id, adopted))
    }

    pub fn update(&mut self, id: &str, label: &str) -> Result<bool> {
        let Some(sub) = self.get(id).cloned() else { return Ok(false) };
        let label = label.trim();
        self.write(Subscription { label: if label.is_empty() { default_label(&sub.config_dir) } else { label.to_owned() }, ..sub })?;
        Ok(true)
    }

    /// The UI disables removal for a subscription in use; this is the guard behind it.
    pub fn remove(&mut self, id: &str, threads_using: usize) -> Result<()> {
        if threads_using > 0 {
            bail!("Used by {threads_using} thread{}", if threads_using == 1 { "" } else { "s" });
        }
        self.db.delete_subscription(id)?;
        self.subs.retain(|s| s.id != id);
        if self.active_id.as_deref() == Some(id) {
            self.set_active_id(self.subs.first().map(|s| s.id.clone()));
        }
        Ok(())
    }

    pub fn set_active(&mut self, id: &str) -> bool {
        if self.get(id).is_none() || self.active_id.as_deref() == Some(id) {
            return false;
        }
        self.set_active_id(Some(id.to_owned()));
        true
    }

    /// Marks the targets as being checked; the caller probes each and reports with `finish_probe`.
    pub fn begin_probe(&mut self, id: Option<&str>) -> Vec<ProbeTarget> {
        let mut targets = vec![];
        for sub in self.subs.iter_mut().filter(|s| id.is_none_or(|id| s.id == id)) {
            sub.account = AccountStatus::Checking;
            targets.push(ProbeTarget { id: sub.id.clone(), agent_id: sub.agent_id, config_dir: sub.config_dir.clone() });
        }
        targets
    }

    /// False when the subscription was removed while its probe ran.
    pub fn finish_probe(&mut self, id: &str, account: AccountStatus) -> Result<bool> {
        let Some(sub) = self.get(id).cloned() else { return Ok(false) };
        self.write(Subscription { account, ..sub })?;
        Ok(true)
    }

    pub fn detect(&self) -> Vec<String> {
        self.specs
            .iter()
            .flat_map(|(agent_id, spec)| {
                spec.default_dirs().into_iter().filter(|dir| !self.subs.iter().any(|s| s.agent_id == *agent_id && s.config_dir == *dir)).collect::<Vec<_>>()
            })
            .collect()
    }

    fn seed_once(db: &Db, seed: Option<(AgentId, String)>) -> Result<()> {
        if db.get_setting::<bool>(SEEDED_KEY) == Some(true) {
            return Ok(());
        }
        db.set_setting(SEEDED_KEY, &true)?;
        let Some((agent_id, config_dir)) = seed else { return Ok(()) };
        if !is_directory(Path::new(&config_dir)) || !db.load_subscriptions()?.is_empty() {
            return Ok(());
        }
        let sub = StoredSubscription {
            id: uuid(),
            agent_id,
            label: DEFAULT_LABEL.into(),
            config_dir,
            created_at: now_ms(),
            account: AccountStatus::Unknown,
        };
        db.save_subscription(&sub)?;
        db.assign_orphan_threads(&sub.id)?;
        db.set_setting(ACTIVE_KEY, &sub.id)?;
        Ok(())
    }

    fn view(&self, stored: StoredSubscription) -> Subscription {
        let login_command = self.specs.get(&stored.agent_id).map(|spec| spec.login_command(&stored.config_dir)).unwrap_or_default();
        Subscription {
            id: stored.id,
            agent_id: stored.agent_id,
            label: stored.label,
            config_dir: stored.config_dir,
            created_at: stored.created_at,
            account: stored.account,
            login_command,
        }
    }

    fn write(&mut self, sub: Subscription) -> Result<()> {
        self.db.save_subscription(&StoredSubscription::from(&sub))?;
        if let Some(slot) = self.subs.iter_mut().find(|s| s.id == sub.id) {
            *slot = sub;
        }
        Ok(())
    }

    fn set_active_id(&mut self, id: Option<SubscriptionId>) {
        self.active_id = id.clone();
        if let Err(error) = self.db.set_setting(ACTIVE_KEY, &id) {
            eprintln!("oniko: could not save the active subscription: {error:#}");
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use futures::future::BoxFuture;

    use super::*;
    use crate::registry::{AccountSpec, EnvLayer};

    pub struct FakeAccounts {
        pub dirs: Vec<String>,
    }

    impl AccountSpec for FakeAccounts {
        fn env_for(&self, config_dir: &str) -> EnvLayer {
            EnvLayer::from([("CLAUDE_CONFIG_DIR".to_owned(), Some(config_dir.to_owned()))])
        }
        fn probe_status(&self, _config_dir: &str) -> BoxFuture<'static, AccountStatus> {
            Box::pin(async {
                AccountStatus::LoggedIn { email: Some("dev@example.com".into()), org_name: None, plan: Some("max".into()), checked_at: 1 }
            })
        }
        fn default_dirs(&self) -> Vec<String> {
            self.dirs.clone()
        }
        fn login_command(&self, config_dir: &str) -> String {
            format!("login {config_dir}")
        }
    }

    pub fn fake_specs(dirs: Vec<String>) -> AccountSpecs {
        AccountSpecs::from([(AgentId::Claude, Arc::new(FakeAccounts { dirs }) as Arc<dyn AccountSpec>)])
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: std::path::PathBuf,
        default_dir: String,
        personal_dir: String,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let default_dir = root.join(".claude");
        let personal_dir = root.join(".claude-personal");
        std::fs::create_dir(&default_dir).unwrap();
        std::fs::create_dir(&personal_dir).unwrap();
        Fixture {
            _dir: dir,
            root,
            default_dir: default_dir.to_string_lossy().into_owned(),
            personal_dir: personal_dir.to_string_lossy().into_owned(),
        }
    }

    fn open(db: &Rc<Db>, f: &Fixture) -> SubscriptionStore {
        SubscriptionStore::new(db.clone(), fake_specs(vec![f.default_dir.clone(), f.personal_dir.clone()]), Some((AgentId::Claude, f.default_dir.clone())))
            .unwrap()
            .with_home(&f.root)
    }

    #[test]
    fn seeds_default_once_and_makes_it_active() {
        let f = fixture();
        let db = Rc::new(Db::in_memory().unwrap());
        let mut store = open(&db, &f);
        let state = store.state();
        assert_eq!(state.subscriptions.len(), 1);
        assert_eq!(state.subscriptions[0].label, DEFAULT_LABEL);
        assert_eq!(state.subscriptions[0].login_command, format!("login {}", f.default_dir));
        assert_eq!(state.active_subscription_id.as_deref(), Some(state.subscriptions[0].id.as_str()));
        let id = state.subscriptions[0].id.clone();
        store.remove(&id, 0).unwrap();
        // Reopening does not bring Default back.
        assert!(open(&db, &f).state().subscriptions.is_empty());
    }

    #[test]
    fn adds_a_folder_typed_with_tilde_and_rejects_bad_ones() {
        let f = fixture();
        let db = Rc::new(Db::in_memory().unwrap());
        let mut store = open(&db, &f);
        let (id, _) = store.add(AgentId::Claude, "~/.claude-personal/", None).unwrap();
        let added = store.get(&id).unwrap();
        assert_eq!(added.config_dir, f.personal_dir);
        assert_eq!(added.label, "claude-personal");
        assert!(store.add(AgentId::Claude, ".claude", None).unwrap_err().contains("absolute"));
        assert!(store.add(AgentId::Claude, "~/missing", None).unwrap_err().contains("No folder"));
        assert!(store.add(AgentId::Claude, &f.personal_dir, None).unwrap_err().contains("already added"));
        assert!(store.detect().is_empty());
    }

    #[test]
    fn refuses_to_remove_a_subscription_in_use_and_switches_active() {
        let f = fixture();
        let db = Rc::new(Db::in_memory().unwrap());
        let mut store = open(&db, &f);
        let default = store.state().subscriptions[0].id.clone();
        assert_eq!(store.remove(&default, 2).unwrap_err().to_string(), "Used by 2 threads");
        let (personal, _) = store.add(AgentId::Claude, &f.personal_dir, Some("  Work ")).unwrap();
        assert_eq!(store.get(&personal).unwrap().label, "Work");
        assert!(store.set_active(&personal));
        assert!(!store.set_active(&personal));
        store.update(&personal, " ").unwrap();
        assert_eq!(store.get(&personal).unwrap().label, "claude-personal");
        assert_eq!(open(&db, &f).state().active_subscription_id.as_deref(), Some(personal.as_str()));
    }

    #[test]
    fn probes_mark_checking_then_store_the_result() {
        let f = fixture();
        let db = Rc::new(Db::in_memory().unwrap());
        let mut store = open(&db, &f);
        let targets = store.begin_probe(None);
        assert_eq!(targets.len(), 1);
        assert_eq!(store.state().subscriptions[0].account, AccountStatus::Checking);
        // A restart mid-probe forgets the "checking".
        assert_eq!(open(&db, &f).state().subscriptions[0].account, AccountStatus::Unknown);
        assert!(store.finish_probe(&targets[0].id, AccountStatus::LoggedOut { checked_at: 3 }).unwrap());
        assert_eq!(open(&db, &f).state().subscriptions[0].account, AccountStatus::LoggedOut { checked_at: 3 });
    }
}
