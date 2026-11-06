//! Runtime contracts shared by Saya's interfaces and adapters.
//!
//! These development-stage interfaces are not a stable SDK. They define data
//! and boundaries plus a volatile store and a permission-checked local tool
//! dispatcher. The loop prototype is test-only; no production agent loop or
//! durable store is provided.

pub mod approvals;
pub mod boundary;
pub mod contract;
pub mod dispatch;

pub mod memory;
pub mod mode;
pub mod permissions;
pub mod review;
pub mod tools;

#[cfg(test)]
mod prototype;
