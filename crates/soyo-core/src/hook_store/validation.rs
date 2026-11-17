use super::*;
use crate::hooks::{
    CleanupState, HookDecision, HookEvent, HookExecutionState, HookKind, HookStatus,
};
use serde::Serialize;

pub(super) fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub(super) fn digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn text(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(crate::text::is_terminal_control)
}
fn name(value: &str, max: usize) -> bool {
    text(value, max)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}
fn encode<T: Serialize>(value: &T, max: usize) -> Result<String, BoundaryError> {
    let text = serde_json::to_string(value)
        .map_err(|_| error("hook_journal_input", "Hook journal data cannot be encoded"))?;
    if text.len() > max {
        return Err(error(
            "hook_journal_input",
            "Hook journal data exceeds its byte limit",
        ));
    }
    Ok(text)
}
fn no_secrets(text: &str, secrets: &SecretRegistry) -> Result<(), BoundaryError> {
    if crate::redaction::contains_secret(text, &secrets.snapshot()?) {
        return Err(error(
            "hook_journal_secret",
            "Hook journal identity contains a known credential",
        ));
    }
    Ok(())
}

pub(super) fn intent(
    intent: &HookIntent,
    secrets: &SecretRegistry,
) -> Result<String, BoundaryError> {
    let tool_event = matches!(
        intent.event,
        HookEvent::ToolBeforeDispatch | HookEvent::ToolFinished
    );
    if !digest(&intent.invocation_id)
        || !digest(&intent.fingerprint)
        || !digest(&intent.package_digest)
        || !digest(&intent.entry_sha256)
        || !digest(&intent.payload_sha256)
        || !text(&intent.event_id, 256)
        || !text(&intent.root_event_id, 256)
        || !name(&intent.plugin_id, 256)
        || !name(&intent.entry_id, 128)
        || !text(&intent.lease_id, 256)
        || intent.run.id.0.is_nil()
        || intent.run.session_id.0.is_nil()
        || intent.enable_revision == 0
        || intent.depth > 8
        || (intent.depth == 0 && intent.event_id != intent.root_event_id)
        || tool_event != intent.parent_call_id.is_some()
        || intent
            .parent_call_id
            .as_ref()
            .is_some_and(|id| !text(id, 256))
        || (intent.kind == HookKind::Check && intent.event != HookEvent::ToolBeforeDispatch)
        || intent.timeout_ms == 0
        || intent.timeout_ms > 5000
        || intent.output_max_bytes == 0
        || intent.output_max_bytes > 64 * 1024
        || intent.run_invocation_limit == 0
        || intent.run_invocation_limit > 4096
    {
        return Err(error(
            "hook_journal_input",
            "Hook journal intent fields are invalid",
        ));
    }
    let encoded = encode(intent, MAX_INTENT_BYTES)?;
    no_secrets(&encoded, secrets)?;
    Ok(encoded)
}

pub(super) fn record(
    record: &HookRecord,
    secrets: &SecretRegistry,
) -> Result<(HookRecord, String), BoundaryError> {
    intent(&record.intent, secrets)?;
    let valid_state = match record.status {
        HookStatus::Succeeded => {
            record.execution == HookExecutionState::Started
                && record.cleanup == CleanupState::Cleaned
                && record.error.is_none()
                && match record.intent.kind {
                    HookKind::Observer => record.decision.is_none(),
                    HookKind::Check => matches!(
                        record.decision,
                        Some(HookDecision::Pass | HookDecision::Abstain)
                    ),
                }
        }
        HookStatus::Denied => {
            record.cleanup == CleanupState::Cleaned
                && ((record.execution == HookExecutionState::NotStarted
                    && record.decision.is_none())
                    || (record.execution == HookExecutionState::Started
                        && record.intent.kind == HookKind::Check
                        && record.decision == Some(HookDecision::Deny)))
        }
        HookStatus::Unknown => {
            record.cleanup == CleanupState::Unknown
                && record.execution != HookExecutionState::NotStarted
                && record.decision.is_none()
        }
        HookStatus::Failed | HookStatus::Cancelled | HookStatus::TimedOut => {
            record.cleanup == CleanupState::Cleaned
                && record.execution != HookExecutionState::Unknown
                && record.decision.is_none()
        }
    };
    if !valid_state
        || record.annotations.len() > 16
        || record
            .annotations
            .iter()
            .any(|annotation| !text(&annotation.message, 1024))
        || record
            .error
            .as_ref()
            .is_some_and(|error| !name(&error.code, 128) || !text(&error.message, 4096))
    {
        return Err(error(
            "hook_journal_input",
            "Hook journal outcome fields are invalid",
        ));
    }
    if let Some(info) = &record.error {
        no_secrets(&encode(&info.code, 1024)?, secrets)?;
    }
    let original = encode(record, MAX_RECORD_BYTES)?;
    let mut value: serde_json::Value = serde_json::from_str(&original).map_err(|_| {
        error(
            "hook_journal_input",
            "Hook journal outcome cannot be decoded",
        )
    })?;
    crate::redaction::redact(&mut value, &secrets.snapshot()?)?;
    let sanitized: HookRecord = serde_json::from_value(value).map_err(|_| {
        error(
            "hook_journal_secret",
            "Hook journal outcome cannot be redacted safely",
        )
    })?;
    if sanitized.intent != record.intent {
        return Err(error(
            "hook_journal_secret",
            "Hook journal identity cannot be redacted safely",
        ));
    }
    if sanitized
        .annotations
        .iter()
        .any(|annotation| !text(&annotation.message, 1024))
    {
        return Err(error(
            "hook_journal_input",
            "Redacted hook annotations exceed their limit",
        ));
    }
    if sanitized
        .error
        .as_ref()
        .is_some_and(|info| !name(&info.code, 128) || !text(&info.message, 4096))
    {
        return Err(error(
            "hook_journal_input",
            "Redacted hook error exceeds its limit",
        ));
    }
    let encoded = encode(&sanitized, MAX_RECORD_BYTES)?;
    no_secrets(&encoded, secrets)?;
    Ok((sanitized, encoded))
}

pub(super) fn diagnostic(
    note: &HookDiagnostic,
    secrets: &SecretRegistry,
) -> Result<(HookDiagnostic, String), BoundaryError> {
    if !digest(&note.invocation_id)
        || !text(&note.event_id, 256)
        || !name(&note.code, 128)
        || !text(&note.message, 2048)
    {
        return Err(error(
            "hook_journal_input",
            "Hook journal diagnostic fields are invalid",
        ));
    }
    no_secrets(
        &encode(&(&note.invocation_id, &note.event_id, &note.code), 4096)?,
        secrets,
    )?;
    let encoded = encode(note, MAX_DIAGNOSTIC_BYTES)?;
    let mut value: serde_json::Value = serde_json::from_str(&encoded).map_err(|_| {
        error(
            "hook_journal_input",
            "Hook journal diagnostic cannot be decoded",
        )
    })?;
    crate::redaction::redact(&mut value, &secrets.snapshot()?)?;
    let sanitized: HookDiagnostic = serde_json::from_value(value).map_err(|_| {
        error(
            "hook_journal_secret",
            "Hook journal diagnostic cannot be redacted safely",
        )
    })?;
    if sanitized.invocation_id != note.invocation_id || sanitized.event_id != note.event_id {
        return Err(error(
            "hook_journal_secret",
            "Hook diagnostic identity cannot be redacted safely",
        ));
    }
    if !text(&sanitized.message, 2048) {
        return Err(error(
            "hook_journal_input",
            "Redacted hook diagnostic exceeds its limit",
        ));
    }
    let encoded = encode(&sanitized, MAX_DIAGNOSTIC_BYTES)?;
    no_secrets(&encoded, secrets)?;
    Ok((sanitized, encoded))
}
