//! Queues analytics events and error reports and ships them in batches. Every failure is
//! swallowed: telemetry must never surface in the app.

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::contract::{DeviceInfo, ErrorSource, Props, RegisterOutput, TelemetryError, TelemetryEvent, limits};
use super::scrub::truncate;

pub mod settings {
    pub const ENABLED: &str = "telemetry.enabled";
    pub const TOKEN: &str = "telemetry.token";
    /// Digest of the DeviceInfo the server last accepted, so it is resent only when it changes.
    pub const DEVICE_HASH: &str = "telemetry.deviceHash";
    /// Random id used when the OS hardware id cannot be read.
    pub const FALLBACK_ID: &str = "telemetry.fallbackId";
    pub const PENDING: &str = "telemetry.pending";
}

/// Where the client keeps its token and unsent batches: the app's settings table.
pub trait SettingsStore: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    fn set(&self, key: &str, value: Value);
}

/// One POST; resolves to the status and the JSON body (null when there is none).
pub trait Http: Send + Sync {
    fn post(&self, path: &str, body: Value, token: Option<String>) -> BoxFuture<'static, Result<(u16, Value), String>>;
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub device_id: String,
    pub device: DeviceInfo,
}

pub type Identify = Arc<dyn Fn() -> BoxFuture<'static, Identity> + Send + Sync>;
pub type Scrub = Arc<dyn Fn(&str) -> String + Send + Sync>;
pub type Now = Arc<dyn Fn() -> String + Send + Sync>;

pub struct ClientDeps {
    pub app_version: String,
    pub store: Arc<dyn SettingsStore>,
    pub http: Arc<dyn Http>,
    pub identify: Identify,
    pub scrub: Scrub,
    pub now: Now,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Batch {
    session_id: String,
    app_version: String,
    events: Vec<TelemetryEvent>,
    errors: Vec<TelemetryError>,
}

/// Unsent batches kept across restarts, one per past session.
const MAX_CARRIED: usize = 3;

pub fn hash_json<T: Serialize>(value: &T) -> String {
    hex(&Sha256::digest(serde_json::to_string(value).unwrap_or_default().as_bytes()))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn event_name(raw: &str) -> String {
    let name: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "_.:-".contains(c) { c } else { '_' })
        .take(limits::EVENT_NAME)
        .collect();
    if name.is_empty() { "unknown".into() } else { name }
}

pub fn clean_props(props: &Props, scrub: &dyn Fn(&str) -> String) -> Props {
    props
        .iter()
        .take(limits::PROPS)
        .map(|(key, value)| {
            let key: String = key.chars().take(limits::KEY).collect();
            let value = match value {
                Value::String(text) => Value::String(truncate(&scrub(text), limits::TEXT)),
                other => other.clone(),
            };
            (key, value)
        })
        .collect()
}

/// Keeps the newest `max`; the queue drops its oldest items first.
fn capped<T>(mut items: Vec<T>, max: usize) -> Vec<T> {
    if items.len() > max {
        items.drain(..items.len() - max);
    }
    items
}

#[derive(Default)]
struct Queue {
    events: Vec<TelemetryEvent>,
    errors: Vec<TelemetryError>,
    /// In first-seen order, coalesced into one event per name per flush.
    counts: Vec<(String, u64)>,
    carried: Vec<Batch>,
    token: Option<String>,
    enabled: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Sent {
    Ok,
    Retry,
    /// A batch the server will never take, so it is not retried.
    Drop,
}

pub struct ErrorInput {
    pub source: ErrorSource,
    pub name: String,
    pub message: String,
    pub stack: Option<String>,
    pub context: Option<Props>,
}

pub struct TelemetryClient {
    deps: ClientDeps,
    queue: Mutex<Queue>,
    identity: tokio::sync::OnceCell<Identity>,
    /// Concurrent flushes run one after another, so a batch is never sent twice.
    sending: tokio::sync::Mutex<()>,
}

impl TelemetryClient {
    pub fn new(deps: ClientDeps) -> TelemetryClient {
        let enabled = deps.store.get(settings::ENABLED).and_then(|v| v.as_bool()).unwrap_or(true);
        let token = deps.store.get(settings::TOKEN).and_then(|v| v.as_str().map(str::to_owned));
        let carried = if enabled {
            deps.store.get(settings::PENDING).and_then(|v| serde_json::from_value::<Vec<Batch>>(v).ok()).unwrap_or_default()
        } else {
            vec![]
        };
        deps.store.set(settings::PENDING, Value::Null);
        TelemetryClient {
            queue: Mutex::new(Queue { enabled, token, carried, ..Queue::default() }),
            deps,
            identity: tokio::sync::OnceCell::new(),
            sending: tokio::sync::Mutex::new(()),
        }
    }

    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().expect("telemetry queue")
    }

    pub fn is_enabled(&self) -> bool {
        self.queue().enabled
    }

    /// Turning it off also drops whatever is still queued.
    pub fn set_enabled(&self, enabled: bool) {
        let mut q = self.queue();
        q.enabled = enabled;
        self.deps.store.set(settings::ENABLED, json!(enabled));
        if !enabled {
            q.events.clear();
            q.errors.clear();
            q.counts.clear();
            q.carried.clear();
            self.deps.store.set(settings::PENDING, Value::Null);
        }
    }

    /// Registers up front when no token is stored yet.
    pub async fn start(&self) {
        let needs_token = {
            let q = self.queue();
            q.enabled && q.token.is_none()
        };
        if needs_token {
            let identity = self.identity().await;
            self.register(&identity).await;
        }
    }

    fn event(&self, name: &str, props: Option<&Props>) -> TelemetryEvent {
        TelemetryEvent { name: event_name(name), props: props.map(|p| clean_props(p, &*self.deps.scrub)), at: (self.deps.now)() }
    }

    pub fn track(&self, name: &str, props: Option<Props>) {
        let event = self.event(name, props.as_ref());
        let mut q = self.queue();
        if !q.enabled {
            return;
        }
        let events = std::mem::take(&mut q.events);
        q.events = capped([events, vec![event]].concat(), limits::EVENTS);
    }

    pub fn count(&self, name: &str) {
        let mut q = self.queue();
        if !q.enabled {
            return;
        }
        match q.counts.iter_mut().find(|(n, _)| n == name) {
            Some((_, count)) => *count += 1,
            None => q.counts.push((name.to_owned(), 1)),
        }
    }

    pub fn error(&self, input: ErrorInput) {
        let scrub = &*self.deps.scrub;
        let name = truncate(&scrub(&input.name), limits::NAME);
        let entry = TelemetryError {
            source: input.source,
            name: if name.is_empty() { "Error".into() } else { name },
            message: truncate(&scrub(&input.message), limits::MESSAGE),
            stack: input.stack.filter(|s| !s.is_empty()).map(|s| truncate(&scrub(&s), limits::STACK)),
            context: input.context.map(|c| clean_props(&c, scrub)),
            at: (self.deps.now)(),
        };
        let mut q = self.queue();
        if !q.enabled {
            return;
        }
        let errors = std::mem::take(&mut q.errors);
        q.errors = capped([errors, vec![entry]].concat(), limits::ERRORS);
    }

    fn drain_counts(&self, q: &mut Queue) {
        if q.counts.is_empty() {
            return;
        }
        let coalesced: Vec<TelemetryEvent> =
            std::mem::take(&mut q.counts).into_iter().map(|(name, count)| self.event(&name, Some(&Props::from([("count".into(), json!(count))])))).collect();
        let events = std::mem::take(&mut q.events);
        q.events = capped([events, coalesced].concat(), limits::EVENTS);
    }

    fn current(&self, q: &Queue) -> Batch {
        Batch {
            session_id: self.deps.session_id.clone(),
            app_version: self.deps.app_version.clone(),
            events: q.events.clone(),
            errors: q.errors.clone(),
        }
    }

    /// Saves what is still queued so the next run can send it.
    pub fn persist(&self) {
        let mut q = self.queue();
        if !q.enabled {
            return;
        }
        self.drain_counts(&mut q);
        let mut all = q.carried.clone();
        if !q.events.is_empty() || !q.errors.is_empty() {
            all.push(self.current(&q));
        }
        self.deps.store.set(settings::PENDING, serde_json::to_value(capped(all, MAX_CARRIED)).unwrap_or(Value::Null));
    }

    /// What is queued right now, counters included. Exposed for tests.
    pub fn queued(&self) -> (Vec<TelemetryEvent>, Vec<TelemetryError>, Vec<(String, u64)>) {
        let q = self.queue();
        (q.events.clone(), q.errors.clone(), q.counts.clone())
    }

    async fn identity(&self) -> Identity {
        self.identity.get_or_init(|| (self.deps.identify)()).await.clone()
    }

    /// Never fails.
    pub async fn flush(&self) {
        let _one_at_a_time = self.sending.lock().await;
        let carried = {
            let mut q = self.queue();
            if !q.enabled {
                return;
            }
            self.drain_counts(&mut q);
            std::mem::take(&mut q.carried)
        };
        let identity = self.identity().await;
        for batch in carried {
            if self.ingest(&identity, &batch).await == Sent::Retry {
                let mut q = self.queue();
                if q.enabled {
                    q.carried.push(batch);
                }
            }
        }
        let batch = {
            let mut q = self.queue();
            if q.events.is_empty() && q.errors.is_empty() {
                return;
            }
            let batch = self.current(&q);
            q.events.clear();
            q.errors.clear();
            batch
        };
        if self.ingest(&identity, &batch).await == Sent::Retry {
            let mut q = self.queue();
            if q.enabled {
                // Anything queued meanwhile is newer, so it stays last and survives the cap.
                let events = std::mem::take(&mut q.events);
                let errors = std::mem::take(&mut q.errors);
                q.events = capped([batch.events, events].concat(), limits::EVENTS);
                q.errors = capped([batch.errors, errors].concat(), limits::ERRORS);
            }
        }
    }

    async fn ingest(&self, identity: &Identity, batch: &Batch) -> Sent {
        let stored = self.queue().token.clone();
        let mut token = match stored {
            Some(token) => token,
            None => match self.register(identity).await {
                Some(token) => token,
                None => return Sent::Retry,
            },
        };
        let device_hash = hash_json(&identity.device);
        let send_device = self.deps.store.get(settings::DEVICE_HASH).and_then(|v| v.as_str().map(str::to_owned)) != Some(device_hash.clone());
        let mut body = serde_json::to_value(batch).unwrap_or_default();
        body["deviceId"] = json!(identity.device_id);
        if send_device {
            body["device"] = serde_json::to_value(&identity.device).unwrap_or_default();
        }
        let mut result = self.deps.http.post("/api/telemetry/ingest", body.clone(), Some(token.clone())).await;
        if matches!(result, Ok((401, _))) {
            token = match self.register(identity).await {
                Some(token) => token,
                None => return Sent::Retry,
            };
            result = self.deps.http.post("/api/telemetry/ingest", body, Some(token)).await;
        }
        match result {
            Err(_) => Sent::Retry,
            Ok((status, _)) if (200..300).contains(&status) => {
                if send_device {
                    self.deps.store.set(settings::DEVICE_HASH, json!(device_hash));
                }
                Sent::Ok
            }
            Ok((status, _)) if status >= 500 || status == 401 || status == 429 => Sent::Retry,
            Ok(_) => Sent::Drop,
        }
    }

    /// Registering again for a known device rotates its token.
    async fn register(&self, identity: &Identity) -> Option<String> {
        let body = json!({ "deviceId": identity.device_id, "device": identity.device });
        let (status, body) = self.deps.http.post("/api/telemetry/register", body, None).await.ok()?;
        if !(200..300).contains(&status) {
            return None;
        }
        let RegisterOutput { token } = serde_json::from_value(body).ok()?;
        self.queue().token = Some(token.clone());
        self.deps.store.set(settings::TOKEN, json!(token));
        self.deps.store.set(settings::DEVICE_HASH, json!(hash_json(&identity.device)));
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::contract::{AppInfo, CpuInfo, OsInfo};
    use super::*;

    #[derive(Default)]
    pub struct MemoryStore(Mutex<HashMap<String, Value>>);

    impl SettingsStore for MemoryStore {
        fn get(&self, key: &str) -> Option<Value> {
            self.0.lock().unwrap().get(key).cloned().filter(|v| !v.is_null())
        }
        fn set(&self, key: &str, value: Value) {
            self.0.lock().unwrap().insert(key.to_owned(), value);
        }
    }

    #[derive(Debug, Clone)]
    struct Call {
        path: String,
        auth: Option<String>,
        body: Value,
    }

    type Respond = Arc<dyn Fn(&Call, usize) -> Result<(u16, Value), String> + Send + Sync>;

    struct FakeHttp {
        calls: Arc<Mutex<Vec<Call>>>,
        respond: Respond,
    }

    impl Http for FakeHttp {
        fn post(&self, path: &str, body: Value, token: Option<String>) -> BoxFuture<'static, Result<(u16, Value), String>> {
            let call = Call { path: path.to_owned(), auth: token.map(|t| format!("Bearer {t}")), body };
            let n = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(call.clone());
                calls.len()
            };
            let result = (self.respond)(&call, n);
            Box::pin(async move { result })
        }
    }

    fn device() -> DeviceInfo {
        DeviceInfo {
            os: OsInfo { platform: "darwin".into(), release: "25.3.0".into(), version: "Darwin".into(), arch: "arm64".into() },
            cpu: CpuInfo { model: "M4".into(), cores: 10, speed_mhz: None },
            memory_bytes: 1,
            gpus: vec![],
            displays: vec![],
            locale: "en-US".into(),
            app: AppInfo { version: "0.1.0".into(), electron: String::new(), chrome: String::new(), node: String::new(), packaged: false },
        }
    }

    const SESSION: &str = "00000000-0000-4000-8000-000000000000";

    struct Setup {
        store: Arc<MemoryStore>,
        calls: Arc<Mutex<Vec<Call>>>,
        respond: Respond,
    }

    impl Setup {
        fn new(respond: impl Fn(&Call, usize) -> Result<(u16, Value), String> + Send + Sync + 'static) -> Setup {
            Setup { store: Arc::new(MemoryStore::default()), calls: Arc::default(), respond: Arc::new(respond) }
        }

        fn make(&self, session: &str) -> TelemetryClient {
            TelemetryClient::new(ClientDeps {
                app_version: "0.1.0".into(),
                store: self.store.clone(),
                http: Arc::new(FakeHttp { calls: self.calls.clone(), respond: self.respond.clone() }),
                identify: Arc::new(|| Box::pin(async { Identity { device_id: "a".repeat(64), device: device() } })),
                scrub: Arc::new(|t: &str| t.replace("/Users/alice", "~")),
                now: Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
                session_id: session.into(),
            })
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn ok(call: &Call, _n: usize) -> Result<(u16, Value), String> {
        Ok((200, if call.path.ends_with("/register") { json!({ "token": "tok-1" }) } else { json!({ "events": 0 }) }))
    }

    #[test]
    fn names_and_props_are_shaped_for_the_server() {
        assert_eq!(event_name("ipc:threads:create"), "ipc:threads:create");
        assert_eq!(event_name("bad name/!"), "bad_name__");
        assert_eq!(event_name(&"x".repeat(80)).len(), 64);
        let many: Props = (0..40).map(|i| (format!("k{i:02}"), json!("v".repeat(300)))).collect();
        let out = clean_props(&many, &|t| t.to_owned());
        assert_eq!(out.len(), 32);
        assert_eq!(out["k00"].as_str().unwrap().chars().count(), 256);
    }

    #[tokio::test]
    async fn coalesces_counters_into_one_event_per_name_per_flush() {
        let setup = Setup::new(ok);
        let client = setup.make(SESSION);
        client.count("ipc:threads:prompt");
        client.count("ipc:threads:prompt");
        client.count("ipc:app:snapshot");
        client.flush().await;
        let ingest = setup.calls().into_iter().find(|c| c.path == "/api/telemetry/ingest").unwrap();
        assert_eq!(ingest.auth.as_deref(), Some("Bearer tok-1"));
        assert_eq!(
            ingest.body["events"],
            json!([
                { "name": "ipc:threads:prompt", "props": { "count": 2 }, "at": "2026-01-01T00:00:00.000Z" },
                { "name": "ipc:app:snapshot", "props": { "count": 1 }, "at": "2026-01-01T00:00:00.000Z" },
            ])
        );
        assert_eq!(client.queued(), (vec![], vec![], vec![]));
    }

    #[test]
    fn drops_the_oldest_items_beyond_the_batch_limits_and_scrubs_errors() {
        let client = Setup::new(ok).make(SESSION);
        for i in 0..510 {
            client.track(&format!("e{i}"), None);
        }
        for i in 0..105 {
            client.error(ErrorInput { source: ErrorSource::Main, name: "Error".into(), message: format!("m{i}"), stack: None, context: None });
        }
        client.error(ErrorInput {
            source: ErrorSource::Ipc,
            name: "Error".into(),
            message: format!("/Users/alice {}", "x".repeat(3000)),
            stack: Some("s".into()),
            context: Some(Props::from([("channel".into(), json!("threads:prompt"))])),
        });
        let (events, errors, _) = client.queued();
        assert_eq!((events.len(), events[0].name.as_str()), (500, "e10"));
        assert_eq!((errors.len(), errors[0].message.as_str()), (100, "m6"));
        let last = errors.last().unwrap();
        assert!(last.message.starts_with("~ x"));
        assert_eq!(last.message.chars().count(), 2000);
    }

    #[tokio::test]
    async fn sends_the_device_only_when_it_changed() {
        let setup = Setup::new(ok);
        let client = setup.make(SESSION);
        client.track("app.start", None);
        client.flush().await;
        let paths: Vec<String> = setup.calls().iter().map(|c| c.path.clone()).collect();
        assert_eq!(paths, ["/api/telemetry/register", "/api/telemetry/ingest"]);
        assert!(setup.calls()[1].body.get("device").is_none());
        setup.store.set(settings::DEVICE_HASH, json!("stale"));
        client.track("again", None);
        client.flush().await;
        assert_eq!(setup.calls()[2].body["device"], serde_json::to_value(device()).unwrap());
    }

    #[tokio::test]
    async fn re_registers_once_on_401() {
        let setup = Setup::new(|call, n| {
            if call.path.ends_with("/register") {
                Ok((200, json!({ "token": "tok-2" })))
            } else if n == 1 {
                Ok((401, json!({ "error": "unauthorized" })))
            } else {
                Ok((200, json!({})))
            }
        });
        setup.store.set(settings::TOKEN, json!("old"));
        let client = setup.make(SESSION);
        client.track("app.start", None);
        client.flush().await;
        let calls: Vec<(String, Option<String>)> = setup.calls().into_iter().map(|c| (c.path, c.auth)).collect();
        assert_eq!(
            calls,
            [
                ("/api/telemetry/ingest".to_owned(), Some("Bearer old".to_owned())),
                ("/api/telemetry/register".to_owned(), None),
                ("/api/telemetry/ingest".to_owned(), Some("Bearer tok-2".to_owned())),
            ]
        );
        assert_eq!(setup.store.get(settings::TOKEN), Some(json!("tok-2")));
    }

    #[tokio::test]
    async fn keeps_items_when_offline_and_sends_them_next_run_under_the_old_session() {
        let offline = Setup::new(|_, _| Err("offline".into()));
        offline.store.set(settings::TOKEN, json!("tok"));
        let first = offline.make(SESSION);
        first.track("app.quit", None);
        first.flush().await;
        assert_eq!(first.queued().0.len(), 1);
        first.count("ipc:app:snapshot");
        first.persist();

        let online = Setup { store: offline.store.clone(), calls: Arc::default(), respond: Arc::new(ok) };
        let next = online.make("11111111-1111-4111-8111-111111111111");
        assert_eq!(online.store.get(settings::PENDING), None);
        next.flush().await;
        let sent = online.calls().last().unwrap().body.clone();
        assert_eq!(sent["sessionId"], json!(SESSION));
        let names: Vec<&str> = sent["events"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["app.quit", "ipc:app:snapshot"]);
    }

    #[tokio::test]
    async fn stops_recording_and_clears_the_queue_when_disabled() {
        let setup = Setup::new(ok);
        let client = setup.make(SESSION);
        client.track("app.start", None);
        client.count("ipc:x");
        client.set_enabled(false);
        client.track("ignored", None);
        client.flush().await;
        assert!(setup.calls().is_empty());
        assert_eq!(client.queued(), (vec![], vec![], vec![]));
        assert!(!setup.make(SESSION).is_enabled());
    }
}
