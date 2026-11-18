//! Anonymous usage counts and crash reports, sent to the Oniko web server when one is configured.

pub mod client;
pub mod contract;
pub mod device;
pub mod scrub;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use serde_json::{Value, json};

use client::{ClientDeps, ErrorInput, Http, Identity, SettingsStore, TelemetryClient, settings};
use contract::{DisplayInfo, ErrorSource, Props};
use scrub::{ScrubContext, scrub};

use crate::db::Db;
use crate::util::{iso_now, uuid};

const FLUSH_EVERY: Duration = Duration::from_secs(30);
const QUIT_FLUSH: Duration = Duration::from_millis(1500);

/// `available` is false when this build or environment has no telemetry server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryState {
    pub enabled: bool,
    pub available: bool,
}

/// Telemetry keeps its own connection to the database file, so it can write settings from any thread.
impl SettingsStore for Mutex<Db> {
    fn get(&self, key: &str) -> Option<Value> {
        self.lock().ok()?.get_setting::<Value>(key).filter(|v| !v.is_null())
    }

    fn set(&self, key: &str, value: Value) {
        if let Ok(db) = self.lock() {
            let _ = db.set_setting(key, &value);
        }
    }
}

struct ReqwestHttp {
    base: String,
    client: reqwest::Client,
}

impl Http for ReqwestHttp {
    fn post(&self, path: &str, body: Value, token: Option<String>) -> BoxFuture<'static, Result<(u16, Value), String>> {
        let mut request = self.client.post(format!("{}{path}", self.base.trim_end_matches('/'))).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        Box::pin(async move {
            let response = request.send().await.map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            let body = response.json::<Value>().await.unwrap_or(Value::Null);
            Ok((status, body))
        })
    }
}

fn base_url() -> Option<String> {
    if std::env::var("ONIKO_TELEMETRY").as_deref() == Ok("0") || std::env::var_os("ONIKO_SMOKE_CWD").is_some() {
        return None;
    }
    std::env::var("ONIKO_TELEMETRY_URL")
        .ok()
        .or_else(|| option_env!("ONIKO_TELEMETRY_URL").map(str::to_owned))
        .or_else(|| cfg!(debug_assertions).then(|| "http://localhost:3000".to_owned()))
        .filter(|url| !url.is_empty())
}

/// The core's view of telemetry. Every call is cheap and never fails.
#[derive(Clone)]
pub struct Telemetry {
    client: Option<Arc<TelemetryClient>>,
    store: Arc<dyn SettingsStore>,
    started_at: Instant,
}

pub struct TelemetryConfig {
    pub db_path: PathBuf,
    pub app_version: String,
    /// Where the app runs from, stripped from stacks.
    pub app_path: String,
    pub displays: Vec<DisplayInfo>,
}

impl Telemetry {
    /// Telemetry that records nothing, for tests and builds without a server.
    pub fn disabled(store: Arc<dyn SettingsStore>) -> Telemetry {
        Telemetry { client: None, store, started_at: Instant::now() }
    }

    /// Call inside the Tokio runtime: it starts the periodic flush.
    pub fn start(config: TelemetryConfig) -> anyhow::Result<Telemetry> {
        let store: Arc<dyn SettingsStore> = Arc::new(Mutex::new(Db::open(&config.db_path)?));
        let Some(base) = base_url() else { return Ok(Telemetry::disabled(store)) };
        let home = crate::util::home_dir().to_string_lossy().into_owned();
        let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
        let ctx = ScrubContext { home, user, app_path: config.app_path };
        let id_store = store.clone();
        let version = config.app_version.clone();
        let displays = config.displays;
        let client = Arc::new(TelemetryClient::new(ClientDeps {
            app_version: config.app_version,
            store: store.clone(),
            http: Arc::new(ReqwestHttp { base, client: reqwest::Client::builder().timeout(Duration::from_secs(10)).build()? }),
            identify: Arc::new(move || {
                let store = id_store.clone();
                let version = version.clone();
                let displays = displays.clone();
                Box::pin(async move { Identity { device_id: device_id(&*store).await, device: device::collect_device_info(&version, displays).await } })
            }),
            scrub: Arc::new(move |text: &str| scrub(text, &ctx)),
            now: Arc::new(iso_now),
            session_id: uuid(),
        }));
        let telemetry = Telemetry { client: Some(client.clone()), store, started_at: Instant::now() };
        telemetry.track("app.start", Some(Props::from([("coldStartMs".into(), json!(0))])));
        let starter = client.clone();
        tokio::spawn(async move {
            starter.start().await;
            let mut every = tokio::time::interval(FLUSH_EVERY);
            every.tick().await;
            loop {
                every.tick().await;
                starter.flush().await;
            }
        });
        Ok(telemetry)
    }

    pub fn state(&self) -> TelemetryState {
        match &self.client {
            Some(client) => TelemetryState { enabled: client.is_enabled(), available: true },
            None => TelemetryState { enabled: false, available: false },
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        match &self.client {
            Some(client) => client.set_enabled(enabled),
            None => self.store.set(settings::ENABLED, json!(enabled)),
        }
    }

    pub fn track(&self, name: &str, props: Option<Props>) {
        if let Some(client) = &self.client {
            client.track(name, props);
        }
    }

    /// One coalesced usage count per command per flush. Never sees the arguments.
    pub fn invoked(&self, command: &str) {
        if let Some(client) = &self.client {
            client.count(&format!("ipc:{command}"));
        }
    }

    pub fn failed(&self, command: &str, message: &str) {
        self.error(ErrorSource::Ipc, "Error", message, None, Some(Props::from([("channel".into(), json!(command))])));
    }

    pub fn error(&self, source: ErrorSource, name: &str, message: &str, stack: Option<String>, context: Option<Props>) {
        if let Some(client) = &self.client {
            client.error(ErrorInput { source, name: name.into(), message: message.into(), stack, context });
        }
    }

    /// A short, best-effort flush on quit; what does not make it is sent next launch.
    pub async fn quit(&self) {
        let Some(client) = &self.client else { return };
        if !client.is_enabled() {
            return;
        }
        client.track("app.quit", Some(Props::from([("sessionSeconds".into(), json!(self.started_at.elapsed().as_secs()))])));
        let _ = tokio::time::timeout(QUIT_FLUSH, client.flush()).await;
        client.persist();
    }
}

async fn device_id(store: &dyn SettingsStore) -> String {
    if let Some(hardware) = device::read_hardware_id().await {
        return device::hash_device_id(&hardware);
    }
    if let Some(stored) = store.get(settings::FALLBACK_ID).and_then(|v| v.as_str().map(str::to_owned)) {
        return device::hash_device_id(&stored);
    }
    let fresh = uuid();
    store.set(settings::FALLBACK_ID, json!(fresh));
    device::hash_device_id(&fresh)
}
