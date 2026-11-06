//! Runtime contracts shared by Saya's interfaces and adapters.
//!
//! These development-stage interfaces are not a stable SDK. They define data
//! and boundaries plus a volatile store and a permission-checked local tool
//! dispatcher and reusable run coordinator. Provider protocols, durable storage,
//! CLI task execution, and the stable SDK are not yet implemented.

pub mod approvals;
pub mod background;
pub mod boundary;
pub mod contract;
pub mod dispatch;

pub mod memory;
pub mod mode;
pub mod permissions;
pub mod prompt;
pub mod provider;
pub mod review;
pub mod runtime;
pub mod tools;

#[cfg(test)]
mod prototype;
