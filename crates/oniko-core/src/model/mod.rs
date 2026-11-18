//! Shared domain types. Their JSON shapes match what the Electron app stored, so an existing
//! `oniko.db` opens unchanged.

pub mod agents;
pub mod config;
pub mod events;
pub mod reducer;
pub mod subscriptions;
pub mod tasks;
pub mod thread;

pub use agents::*;
pub use events::*;
pub use subscriptions::*;
pub use tasks::*;
pub use thread::*;
