//! Bounded newline-delimited JSON protocol between the host and the
//! `__soyo-js-worker` subprocess. Each line is one complete message; no frame
//! may exceed `MAX_WIRE_BYTES`.

use crate::contract::{BoundaryError, ErrorInfo, ToolResult};
use serde::{Deserialize, Serialize};

/// Hard cap for one protocol message on the wire in either direction.
pub const MAX_WIRE_BYTES: usize = 16 * 1024 * 1024;

/// First host message carrying the script and engine limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerInit {
    pub schema_version: u8,
    pub script: String,
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub max_calls: u32,
}

/// Host → worker messages after initialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostMessage {
    /// Answer to a pending `call`. `result` is the dispatched `ToolResult`.
    Result { seq: u32, result: ToolResult },
    /// Orderly shutdown request (defensive; the host kills on cleanup anyway).
    Abort { message: String },
}

/// Worker → host messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerMessage {
    /// Script-issued tool call awaiting a `result` for the same `seq`.
    Call {
        seq: u32,
        name: String,
        /// Serialized JSON object matching the target tool's input schema.
        arguments: String,
    },
    /// Script finished; `value` is its JSON-serializable return value.
    Done { value: serde_json::Value },
    /// Script threw, violated a limit, or the engine failed to evaluate.
    Failed { message: String },
}

fn failure(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

/// Serialize one message plus the trailing newline, rejecting oversized wires.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, BoundaryError> {
    let mut bytes = serde_json::to_vec(message)
        .map_err(|_| failure("repl_protocol_error", "message cannot be encoded"))?;
    if bytes.len() + 1 > MAX_WIRE_BYTES {
        return Err(failure(
            "worker_rpc_too_large",
            "repl message exceeds the wire limit",
        ));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

/// Parse one line (without the newline) into a message.
pub fn decode<'a, T: Deserialize<'a>>(line: &'a [u8]) -> Result<T, BoundaryError> {
    serde_json::from_slice(line).map_err(|_| {
        failure(
            "repl_protocol_error",
            "message is not a valid protocol object",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_round_trip_and_bounds() {
        let init = WorkerInit {
            schema_version: 1,
            script: "return 1;".into(),
            memory_bytes: 1,
            stack_bytes: 1,
            max_calls: 1,
        };
        let wire = encode(&init).unwrap();
        assert!(wire.ends_with(b"\n"));
        let decoded: WorkerInit = decode(&wire[..wire.len() - 1]).unwrap();
        assert_eq!(decoded.script, "return 1;");
        let call = WorkerMessage::Call {
            seq: 7,
            name: "read_file".into(),
            arguments: "{}".into(),
        };
        let wire = encode(&call).unwrap();
        match decode::<WorkerMessage>(&wire[..wire.len() - 1]).unwrap() {
            WorkerMessage::Call { seq, name, .. } => {
                assert_eq!(seq, 7);
                assert_eq!(name, "read_file");
            }
            _ => panic!("expected call"),
        }
        assert!(decode::<WorkerMessage>(b"not json").is_err());
        let huge = serde_json::json!({"blob": "x".repeat(MAX_WIRE_BYTES)});
        assert_eq!(
            encode(&huge).unwrap_err().0.code.as_str(),
            "worker_rpc_too_large"
        );
    }
}
