//! Everything Oniko does that is not drawing: agent sessions over ACP, the SQLite store, subscriptions,
//! tasks with their git worktrees, and telemetry. The UI talks to it only through [`core::CoreHandle`]
//! and the [`model::AppEvent`] stream, the way the Electron renderer talked to main over IPC.

pub mod core;
pub mod db;
pub mod model;
pub mod registry;
pub mod review;
pub mod runner;
pub mod session;
pub mod stores;
pub mod telemetry;
pub mod util;

/// The ACP message types Oniko stores and shows, re-exported so the UI does not depend on the SDK.
pub use agent_client_protocol::v1::schema as acp;
