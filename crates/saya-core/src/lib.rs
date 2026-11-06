//! Runtime contracts shared by Saya's interfaces and adapters.
//!
//! These development-stage interfaces are not a stable SDK. They define data
//! and boundaries plus a volatile store. The loop prototype is test-only; no
//! production agent loop or permission enforcement is provided.

pub mod boundary;
pub mod contract;

pub mod memory;
pub mod mode;
pub mod permissions;
pub mod tools;

#[cfg(test)]
mod prototype;
