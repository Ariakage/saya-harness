//! Runtime contracts shared by Saya's interfaces and adapters.
//!
//! These development-stage interfaces are not a stable SDK. They define data
//! and boundaries plus a volatile store and a permission-checked local tool
//! dispatcher, provider adapters and reusable run coordinator. Durable storage,
//! CLI task execution, and the stable SDK are not yet implemented.

pub mod approvals;
pub mod background;
pub mod boundary;
pub mod config;
pub mod contract;
pub mod dispatch;
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
pub mod review;
pub mod runtime;
pub mod sqlite;
pub mod tools;
mod wal_recovery;

#[cfg(test)]
mod prototype;
