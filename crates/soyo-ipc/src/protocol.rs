//! Wire protocol: newline-delimited JSON frames over a local socket.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bump when frame shapes or method semantics change incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// Frames sent from a client to the helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Invoke a service method. `params` is the JSON object of named arguments.
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    /// Best-effort cancel of an in-flight streaming request.
    CancelRequest { id: u64 },
    /// Ask the helper daemon to shut down gracefully.
    Shutdown,
}

/// Frames sent from the helper to a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerFrame {
    /// First frame on every connection.
    Hello { version: u32, pid: u32 },
    /// Stream item for a streaming `Request` (serialized `DesktopStreamEnvelope`).
    Event { id: u64, data: Value },
    /// Terminal success for `id`.
    Response { id: u64, result: Value },
    /// Terminal failure for `id`.
    Error {
        id: u64,
        code: String,
        message: String,
    },
    /// Sent before the daemon exits (response to `Shutdown` or signal).
    Bye,
}
