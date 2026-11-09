//! Runtime contracts shared by Saya's interfaces and adapters.
//!
//! These development-stage interfaces are not a stable SDK. They define data
//! and boundaries plus a volatile store and a permission-checked local tool
//! dispatcher, provider adapters, reusable run coordinator, and SQLite durable
//! storage. The CLI uses these services for task execution and session recovery.
//! SDK stability is tracked as a future capability.

pub mod approvals;
pub mod background;
pub mod boundary;
pub mod command_analysis;
pub mod config;
pub mod contract;
pub mod dispatch;
pub mod export;
pub mod input;
pub mod logging;

pub mod memory;
pub mod metrics;
pub mod mode;
pub mod permissions;
pub mod project_context;
pub mod prompt;
pub mod provider;
pub mod redaction;
pub mod registry;
pub mod review;
pub mod runtime;
pub mod sqlite;
pub mod text;
pub mod tools;
mod wal_recovery;

#[cfg(test)]
mod prototype;
