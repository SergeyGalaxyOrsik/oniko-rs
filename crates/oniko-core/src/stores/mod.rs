//! The authoritative state, one store per concern. Each writes through to the database; the core
//! broadcasts what changed.

pub mod subscriptions;
pub mod tasks;
