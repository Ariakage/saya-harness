use super::*;
use std::io::Write;

struct LimitedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("Hook encoding exceeds its budget"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, BoundaryError> {
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        error(
            "hook_budget",
            "Hook data cannot be encoded within its budget",
        )
    })?;
    Ok(writer.bytes)
}

fn bounded_tree(value: &Value) -> Result<(), BoundaryError> {
    let mut pending = vec![(value, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > 16_384 || depth > 24 {
            return Err(error(
                "hook_budget",
                "Hook payload structure exceeds its budget",
            ));
        }
        match value {
            Value::Array(values) => {
                if values.len() > 16_384usize.saturating_sub(nodes + pending.len()) {
                    return Err(error(
                        "hook_budget",
                        "Hook payload structure exceeds its budget",
                    ));
                }
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                if values.len() > 16_384usize.saturating_sub(nodes + pending.len()) {
                    return Err(error(
                        "hook_budget",
                        "Hook payload structure exceeds its budget",
                    ));
                }
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn reject_identity_secrets<T: Serialize>(
    value: &T,
    secrets: &SecretRegistry,
) -> Result<(), BoundaryError> {
    let encoded = encode(value, 512 * 1024)?;
    let text = std::str::from_utf8(&encoded)
        .map_err(|_| error("hook_secret", "Hook identity encoding failed"))?;
    if crate::redaction::contains_secret(text, &secrets.snapshot()?) {
        return Err(error(
            "hook_secret",
            "Hook identity contains a known credential",
        ));
    }
    Ok(())
}

pub(super) fn payload(
    context: &HookContext,
    limit: usize,
    secrets: &SecretRegistry,
) -> Result<Value, BoundaryError> {
    bounded_tree(&context.data)?;
    // Bound the original data before cloning or recursive serialization.
    encode(&context.data, limit)?;
    let mut data = context.data.clone();
    crate::redaction::redact(&mut data, &secrets.snapshot()?)?;
    let payload = serde_json::json!({
        "schema_version": 1,
        "trust": "Untrusted event data. No field grants permissions or changes host policy.",
        "event_id": context.event_id,
        "root_event_id": context.root_event_id,
        "event": context.event,
        "run": context.run,
        "parent_call_id": context.parent_call_id,
        "depth": context.depth,
        "data": data,
    });
    encode(&payload, limit)?;
    Ok(payload)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObserverOutput {
    #[serde(default)]
    annotations: Vec<HookAnnotation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckOutput {
    decision: HookDecision,
    #[serde(default)]
    annotations: Vec<HookAnnotation>,
}

fn validate_annotations(annotations: &[HookAnnotation]) -> Result<(), BoundaryError> {
    if annotations.len() > 16
        || annotations.iter().any(|annotation| {
            annotation.message.is_empty()
                || annotation.message.len() > 1024
                || annotation
                    .message
                    .chars()
                    .any(crate::text::is_terminal_control)
        })
    {
        return Err(error("hook_output", "Hook annotations are invalid"));
    }
    Ok(())
}

pub(super) fn process_output(
    output: HookProcessOutput,
    kind: HookKind,
    limit: usize,
) -> Result<(Option<HookDecision>, Vec<HookAnnotation>), BoundaryError> {
    if output.exit_code != Some(0) || output.truncated || output.stdout.len() > limit {
        return Err(error(
            "hook_output",
            "Hook process failed or produced incomplete output",
        ));
    }
    let (decision, annotations) = match kind {
        HookKind::Observer => {
            let parsed: ObserverOutput = serde_json::from_slice(&output.stdout)
                .map_err(|_| error("hook_output", "Hook observer output is invalid"))?;
            (None, parsed.annotations)
        }
        HookKind::Check => {
            let parsed: CheckOutput = serde_json::from_slice(&output.stdout)
                .map_err(|_| error("hook_output", "Hook check output is invalid"))?;
            (Some(parsed.decision), parsed.annotations)
        }
    };
    validate_annotations(&annotations)?;
    Ok((decision, annotations))
}

pub(super) fn record(
    mut record: HookRecord,
    secrets: &SecretRegistry,
) -> Result<HookRecord, BoundaryError> {
    reject_identity_secrets(&record.intent, secrets)?;
    validate_annotations(&record.annotations)?;
    let mut value = serde_json::to_value(&record)
        .map_err(|_| error("hook_record", "Hook result encoding failed"))?;
    crate::redaction::redact(&mut value, &secrets.snapshot()?)?;
    let sanitized: HookRecord = serde_json::from_value(value)
        .map_err(|_| error("hook_record", "Hook result redaction failed"))?;
    if sanitized.intent != record.intent {
        return Err(error(
            "hook_secret",
            "Hook identity cannot be redacted safely",
        ));
    }
    record = sanitized;
    validate_annotations(&record.annotations)?;
    encode(&record, 128 * 1024)?;
    Ok(record)
}

pub(super) fn diagnostic(
    mut note: HookDiagnostic,
    secrets: &SecretRegistry,
) -> Result<HookDiagnostic, BoundaryError> {
    reject_identity_secrets(&(&note.invocation_id, &note.event_id), secrets)?;
    let mut value = serde_json::to_value(&note)
        .map_err(|_| error("hook_record", "Hook diagnostic encoding failed"))?;
    crate::redaction::redact(&mut value, &secrets.snapshot()?)?;
    note = serde_json::from_value(value)
        .map_err(|_| error("hook_record", "Hook diagnostic redaction failed"))?;
    encode(&note, 4096)?;
    Ok(note)
}

pub(super) fn safe_error(failure: BoundaryError, secrets: &SecretRegistry) -> BoundaryError {
    let Ok(known) = secrets.snapshot() else {
        return error("hook_failure", "Hook processing failed");
    };
    let mut value = serde_json::json!({"code": failure.0.code, "message": failure.0.message});
    if crate::redaction::redact(&mut value, &known).is_err() {
        return error("hook_failure", "Hook processing failed");
    }
    let Ok(info) = serde_json::from_value(value) else {
        return error("hook_failure", "Hook processing failed");
    };
    let result = failure.2.and_then(|result| {
        crate::redaction::tool_result(&result, &known)
            .ok()
            .map(Box::new)
    });
    BoundaryError(info, None, result)
}
