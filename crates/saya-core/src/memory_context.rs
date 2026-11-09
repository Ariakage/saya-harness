//! Content-free bindings from actual memory exposure to durable source history.
use crate::{
    context_builder::{self, ItemRange},
    contract::{BoundaryError, ErrorInfo, HistoryItem, RunId, Session, SessionId},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryExposureOrigin {
    ModelRequest { request_id_sha256: String },
    NativeMemory { call_id: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryExposure {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub origin: MemoryExposureOrigin,
    pub memory_revision: String,
    pub policy_epoch: String,
    pub range: ItemRange,
    pub source_sha256: String,
}
impl MemoryExposure {
    pub fn validate_source(&self, session: &Session) -> Result<(), BoundaryError> {
        let fail = || {
            BoundaryError(
                ErrorInfo {
                    code: "memory_exposure".into(),
                    message: "Memory exposure source binding is invalid".into(),
                },
                None,
                None,
            )
        };
        let valid_digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if self.session_id != session.id
            || self.range.start >= self.range.end
            || self.range.end > session.history.len()
            || !valid_digest(&self.memory_revision)
            || !valid_digest(&self.policy_epoch)
            || context_builder::history_digest(&session.history[self.range.start..self.range.end])?
                != self.source_sha256
        {
            return Err(fail());
        }
        match &self.origin {
            MemoryExposureOrigin::ModelRequest { request_id_sha256 } => {
                if !valid_digest(request_id_sha256)
                    || session.history[self.range.start..self.range.end]
                        .iter()
                        .any(|item| {
                            matches!(
                                item,
                                HistoryItem::UserText(_) | HistoryItem::ToolResult { .. }
                            )
                        })
                {
                    return Err(fail());
                }
            }
            MemoryExposureOrigin::NativeMemory { call_id } => {
                let HistoryItem::ToolCall(call) = &session.history[self.range.start] else {
                    return Err(fail());
                };
                let HistoryItem::ToolResult {
                    call_id: result_id, ..
                } = &session.history[self.range.end - 1]
                else {
                    return Err(fail());
                };
                if call_id != result_id
                    || call_id != &call.call_id
                    || !matches!(
                        call.name.as_str(),
                        "memory_read"
                            | "memory_search"
                            | "memory_evidence"
                            | "memory_update"
                            | "memory_forget"
                    )
                {
                    return Err(fail());
                }
            }
        }
        Ok(())
    }
}
