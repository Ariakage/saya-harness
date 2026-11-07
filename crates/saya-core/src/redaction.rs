//! Known-credential redaction for serialized data. Unknown secrets are not inferred.
use crate::contract::{BoundaryError, ErrorInfo, ToolResult};
use serde_json::Value;

fn failure() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "sensitive_data".into(),
            message: "sensitive content could not be safely redacted".into(),
        },
        None,
        None,
    )
}
fn variants(secret: &str) -> Vec<String> {
    let mut out = Vec::new();
    if secret.is_empty() {
        return out;
    }
    let mut candidate = secret.to_owned();
    for _ in 0..6 {
        if out.contains(&candidate) {
            break;
        }
        out.push(candidate.clone());
        let Ok(encoded) = serde_json::to_string(&candidate) else {
            break;
        };
        let Some(fragment) = encoded.get(1..encoded.len().saturating_sub(1)) else {
            break;
        };
        candidate = fragment.to_owned();
    }
    out.sort_by_key(|item| std::cmp::Reverse(item.len()));
    out
}
pub fn contains_secret(text: &str, secrets: &[String]) -> bool {
    secrets
        .iter()
        .flat_map(|secret| variants(secret))
        .any(|candidate| text.contains(&candidate))
}
fn redact_string(text: &mut String, secrets: &[String], depth: usize) -> Result<(), BoundaryError> {
    if !contains_secret(text, secrets) {
        return Ok(());
    }
    if depth < 6
        && let Ok(mut nested) = serde_json::from_str::<Value>(text)
        && !matches!(&nested,Value::String(inner) if inner==text)
    {
        redact_inner(&mut nested, secrets, depth + 1)?;
        *text = serde_json::to_string(&nested).map_err(|_| failure())?;
    }
    for candidate in secrets.iter().flat_map(|secret| variants(secret)) {
        *text = text.replace(&candidate, "[REDACTED]");
    }
    if contains_secret(text, secrets) {
        return Err(failure());
    }
    Ok(())
}
fn redact_inner(value: &mut Value, secrets: &[String], depth: usize) -> Result<(), BoundaryError> {
    match value {
        Value::String(text) => redact_string(text, secrets, depth)?,
        Value::Array(values) => {
            for item in values {
                redact_inner(item, secrets, depth)?
            }
        }
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (mut key, mut item) in original {
                redact_string(&mut key, secrets, depth)?;
                redact_inner(&mut item, secrets, depth)?;
                if values.insert(key, item).is_some() {
                    return Err(failure());
                }
            }
        }
        _ => {}
    }
    Ok(())
}
/// Redact recursively, including nested JSON and escaped credential variants.
/// Returns an error instead of leaking or silently merging object keys.
pub fn redact(value: &mut Value, secrets: &[String]) -> Result<(), BoundaryError> {
    redact_inner(value, secrets, 0)?;
    let serialized = serde_json::to_string(value).map_err(|_| failure())?;
    if contains_secret(&serialized, secrets) {
        return Err(failure());
    }
    Ok(())
}
/// Return a sanitized tool result, preserving its schema.
pub fn tool_result(result: &ToolResult, secrets: &[String]) -> Result<ToolResult, BoundaryError> {
    let mut value = serde_json::to_value(result).map_err(|_| failure())?;
    redact(&mut value, secrets)?;
    serde_json::from_value(value).map_err(|_| failure())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn redacts_nested_json_and_escaped_credentials_without_collisions() {
        let secret = "fixture\"secret\\tail".to_string();
        let inner = json!({"value":secret}).to_string();
        let mut value = json!({"outer":inner});
        redact(&mut value, std::slice::from_ref(&secret)).unwrap();
        assert!(!contains_secret(
            &value.to_string(),
            std::slice::from_ref(&secret)
        ));
        assert!(value.to_string().contains("[REDACTED]"));
        let mut collision = json!({"token":"one","[REDACTED]":"two"});
        assert!(redact(&mut collision, &["token".into()]).is_err());
    }
}
