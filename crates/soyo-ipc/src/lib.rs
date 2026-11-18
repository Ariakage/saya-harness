//! Shared IPC protocol and DTO types between `soyo` clients and the `soyo helper` daemon.

pub mod client;
pub mod framing;
pub mod protocol;
pub mod redaction;
pub mod socket;
pub mod tool_format;
pub mod types;

pub use client::HelperClient;
pub use protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
pub use socket::{HelperEndpoint, helper_pid_path, helper_socket_name};
