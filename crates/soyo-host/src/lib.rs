//! Runtime service for the `soyo helper` daemon: session/run management,
//! streaming sinks, peripheral ops, and the NDJSON socket server.

pub mod attachment;
pub mod peripheral;
pub mod prefs;
pub mod server;
pub mod service;

pub use server::{HelperServer, ServerConfig};
pub use service::{HostService, StreamSendError, StreamSink};
pub use soyo_ipc::tool_format;
mod server_tests;
mod service_tests;
