//! The UI's copy of the app state: a snapshot from the core, then every broadcast event applied with
//! the same reducer the core runs, as the Electron renderer did.

use gpui::{App, AsyncApp, Context, Entity, EventEmitter, Global, WeakEntity};
use oniko_core::core::CoreHandle;
use oniko_core::model::reducer::AppState;
use oniko_core::model::{AppEvent, SequencedEvent, ThreadId};
/// A tokio receiver works on any executor, so gpui's can drive it.
use oniko_core::util::UnboundedReceiver;

pub struct Store {
    pub state: AppState,
    pub home: String,
    pub core: CoreHandle,
    /// The last event applied; a snapshot already covers events up to its own seq.
    seq: u64,
    pub loaded: bool,
    /// Errors from commands, shown until dismissed.
    pub errors: Vec<String>,
}

pub enum StoreEvent {
    OpenThread(ThreadId),
}

impl EventEmitter<StoreEvent> for Store {}

pub struct GlobalStore(pub Entity<Store>);

impl Global for GlobalStore {}

pub fn store(cx: &App) -> Entity<Store> {
    cx.global::<GlobalStore>().0.clone()
}

impl Store {
    pub fn new(core: CoreHandle, events: UnboundedReceiver<SequencedEvent>, cx: &mut Context<Self>) -> Store {
        let snapshot = core.snapshot();
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let mut events = events;
            match snapshot.await {
                Ok(snapshot) => {
                    let _ = this.update(cx, |store, cx| {
                        store.state = AppState {
                            threads: snapshot.threads.into_iter().map(|t| (t.id.clone(), t)).collect(),
                            subscriptions: snapshot.subscriptions,
                            tasks: snapshot.tasks,
                        };
                        store.home = snapshot.home;
                        store.seq = snapshot.seq;
                        store.loaded = true;
                        cx.notify();
                    });
                }
                Err(error) => {
                    let _ = this.update(cx, |store, cx| store.report(error, cx));
                }
            }
            while let Some(first) = events.recv().await {
                // Everything already queued goes in one update, so a burst of chunks is one render.
                let mut batch = vec![first];
                while let Ok(next) = events.try_recv() {
                    batch.push(next);
                }
                if this.update(cx, |store, cx| store.apply(batch, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        Store { state: AppState::default(), home: String::new(), core, seq: 0, loaded: false, errors: vec![] }
    }

    fn apply(&mut self, batch: Vec<SequencedEvent>, cx: &mut Context<Self>) {
        for SequencedEvent { seq, event } in batch {
            if seq <= self.seq {
                continue;
            }
            self.seq = seq;
            if let AppEvent::OpenThread(id) = &event {
                cx.emit(StoreEvent::OpenThread(id.clone()));
            }
            self.state.apply(&event);
        }
        cx.notify();
    }

    pub fn report(&mut self, error: String, cx: &mut Context<Self>) {
        eprintln!("oniko: {error}");
        if !self.errors.contains(&error) {
            self.errors.push(error);
        }
        cx.notify();
    }

    pub fn dismiss_error(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.errors.len() {
            self.errors.remove(index);
        }
        cx.notify();
    }
}

/// Runs a core command and shows its failure, if any, as an error banner.
pub fn run<T: 'static, F: std::future::Future<Output = Result<T, String>> + 'static>(cx: &mut App, command: impl FnOnce(&CoreHandle) -> F) {
    let store = store(cx);
    let command = command(&store.read(cx).core);
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = command.await {
            let _ = store.update(cx, |store, cx| store.report(error, cx));
        }
    })
    .detach();
}

/// Like [`run`], then hands the value to `then` on the UI thread.
pub fn run_then<T: 'static, F: std::future::Future<Output = Result<T, String>> + 'static>(
    cx: &mut App,
    command: impl FnOnce(&CoreHandle) -> F,
    then: impl FnOnce(T, &mut App) + 'static,
) {
    let store = store(cx);
    let command = command(&store.read(cx).core);
    cx.spawn(async move |cx: &mut AsyncApp| match command.await {
        Ok(value) => {
            let _ = cx.update(|cx| then(value, cx));
        }
        Err(error) => {
            let _ = store.update(cx, |store, cx| store.report(error, cx));
        }
    })
    .detach();
}

pub fn core(cx: &App) -> CoreHandle {
    store(cx).read(cx).core.clone()
}
