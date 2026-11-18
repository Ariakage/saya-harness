//! Shared IPC protocol and DTO types between `soyo` clients and the `soyo helper` daemon.

pub mod client;
pub mod framing;
pub mod protocol;
pub mod redaction;
pub mod socket;
pub mod tool_format;
pub mod types;

pub use client::{HelperClient, HelperCompat};
pub use protocol::{ClientFrame, PROTOCOL_VERSION, ServerFrame};
pub use socket::{
    HelperEndpoint, HelperPidRecord, HelperTarget, helper_pid_path, process_exe_path,
    process_started_id, verify_peer_euid,
};
