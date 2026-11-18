use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default()
}

/// `2026-11-04T12:00:00.000Z`, the shape of JavaScript's `toISOString`.
pub fn iso_now() -> String {
    humantime::format_rfc3339_millis(SystemTime::now()).to_string()
}

pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Lexically resolves `.` and `..` and drops trailing slashes, like Node's `path.resolve` on an absolute path.
pub fn resolve(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn basename(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path)
}

pub fn is_directory(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

pub fn message_of(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

/// The core's event stream type, re-exported so the UI needs no tokio dependency of its own.
pub use tokio::sync::mpsc::UnboundedReceiver;
