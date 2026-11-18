//! Runs real turns against the installed Claude adapter: a prompt, then a second agent process that
//! reattaches to the same ACP session and must remember the first turn.
//! `cargo run -p oniko-core --example session_smoke`
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oniko_core::model::{ThreadEventBody, ThreadStatus, TimelineItem};
use oniko_core::model::reducer::{ThreadMap, reduce_threads};
use oniko_core::model::{AgentId, Thread, ThreadEvent};
use oniko_core::registry::{EnvLayer, claude_env_for};
use oniko_core::session::{SessionHandle, SessionOptions};
use tokio::sync::mpsc;

fn thread(cwd: &str) -> Thread {
    Thread {
        id: "smoke".into(),
        agent_id: AgentId::Claude,
        subscription_id: "s".into(),
        cwd: cwd.into(),
        title: String::new(),
        created_at: 0,
        acp_session_id: None,
        status: ThreadStatus::Dormant,
        timeline: vec![],
        config: vec![],
        commands: vec![],
    }
}

async fn turn(handle: &SessionHandle, rx: &mut mpsc::UnboundedReceiver<ThreadEventBody>, map: &Arc<Mutex<ThreadMap>>, text: &str) {
    handle.prompt(text.into());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let body = tokio::time::timeout_at(deadline, rx.recv()).await.expect("turn timed out").expect("session ended");
        match &body {
            ThreadEventBody::Status(s) => println!("  status {s:?}"),
            ThreadEventBody::SessionAttached { acp_session_id } => println!("  attached {acp_session_id}"),
            ThreadEventBody::Config(c) => println!("  config {:?}", c.iter().map(|c| format!("{}={}", c.id, c.value)).collect::<Vec<_>>()),
            ThreadEventBody::Notice { text, level } => {
                println!("  notice {text}");
                if *level == oniko_core::model::NoticeLevel::Error {
                    panic!("the turn failed");
                }
            }
            ThreadEventBody::TurnEnd { stop_reason, duration_ms } => println!("  turn_end {stop_reason} in {duration_ms}ms"),
            _ => {}
        }
        let done = matches!(body, ThreadEventBody::TurnEnd { .. });
        let failed = matches!(body, ThreadEventBody::Status(ThreadStatus::Error { .. } | ThreadStatus::Exited { .. } | ThreadStatus::AuthRequired { .. }));
        reduce_threads(&mut map.lock().unwrap(), &ThreadEvent { thread_id: "smoke".into(), body });
        if failed {
            panic!("session failed");
        }
        if done {
            break;
        }
    }
}

fn last_assistant(map: &Arc<Mutex<ThreadMap>>) -> String {
    map.lock().unwrap()["smoke"]
        .timeline
        .iter()
        .rev()
        .find_map(|i| match i {
            TimelineItem::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn spawn(cwd: &str, session: Option<String>, tx: mpsc::UnboundedSender<ThreadEventBody>) -> SessionHandle {
    let home = dirs::home_dir().unwrap();
    let dir = std::env::var("ONIKO_SMOKE_CONFIG_DIR").unwrap_or_else(|_| home.join(".claude").to_string_lossy().into_owned());
    let env: EnvLayer = claude_env_for(&dir, &home);
    SessionHandle::spawn(SessionOptions {
        agent_id: AgentId::Claude,
        cwd: cwd.into(),
        account_env: env,
        acp_session_id: session,
        initial_config: vec![],
        log_path: std::env::temp_dir().join("oniko-session-smoke.log"),
        emit: Arc::new(move |body| {
            let _ = tx.send(body);
        }),
    })
}

#[tokio::main]
async fn main() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let map = Arc::new(Mutex::new(ThreadMap::from([("smoke".to_owned(), thread(&cwd))])));

    println!("first process");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let first = spawn(&cwd, None, tx);
    turn(&first, &mut rx, &map, "Remember the word 'tangerine'. Reply with just: ok").await;
    println!("  assistant: {}", last_assistant(&map));
    let session = map.lock().unwrap()["smoke"].acp_session_id.clone();
    first.dispose();

    println!("second process, reattaching {session:?}");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let second = spawn(&cwd, session, tx);
    turn(&second, &mut rx, &map, "What word did I ask you to remember? Reply with just the word.").await;
    let answer = last_assistant(&map);
    println!("  assistant: {answer}");
    second.dispose();
    assert!(answer.to_lowercase().contains("tangerine"), "the reattached session forgot the first turn");
    println!("ok - reattached session remembers the first turn");
}
