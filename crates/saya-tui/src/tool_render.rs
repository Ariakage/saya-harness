//! Human-readable projections of canonical tool records and results.
use saya_core::contract::{ToolCall, ToolResult};
use serde_json::Value;

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        _ => String::new(),
    }
}

fn append_value(lines: &mut Vec<String>, label: &str, value: &Value, indent: usize) {
    let prefix = " ".repeat(indent);
    match value {
        Value::Object(values) => {
            lines.push(format!("{prefix}{label}:"));
            for (key, value) in values {
                append_value(lines, key, value, indent + 2);
            }
        }
        Value::Array(values)
            if values
                .iter()
                .all(|value| !value.is_object() && !value.is_array()) =>
        {
            lines.push(format!(
                "{prefix}{label}: {}",
                values.iter().map(scalar).collect::<Vec<_>>().join(", ")
            ));
        }
        Value::Array(values) => {
            lines.push(format!("{prefix}{label}: {} items", values.len()));
            for (index, value) in values.iter().enumerate() {
                append_value(lines, &format!("[{index}]"), value, indent + 2);
            }
        }
        value => lines.push(format!("{prefix}{label}: {}", scalar(value))),
    }
}

pub(crate) fn result(result: &ToolResult) -> String {
    let mut lines = vec![format!(
        "Status: {}",
        if result.ok { "completed" } else { "failed" }
    )];
    if result.truncated {
        lines.push("Output truncated by configured limits.".into());
    }
    if let Some(error) = &result.error {
        lines.push(format!("Error: {}", error.code));
        lines.push(format!("Message: {}", error.message));
    }
    if let Some(data) = &result.data {
        append_value(&mut lines, "Output", data, 0);
    }
    lines.join("\n")
}

pub(crate) fn record_lines(records: &[Value]) -> Vec<String> {
    if records.is_empty() {
        return vec!["No committed tool calls.".into()];
    }
    let mut lines = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let call = record
            .get("call")
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolCall>(value).ok());
        let state = record
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let name = call
            .as_ref()
            .map_or("unknown tool", |call| call.name.as_str());
        lines.push(format!("#{} {name} · {state}", index + 1));
        lines.push(format!(
            "  run {} · call {}",
            record.get("run_id").and_then(Value::as_str).unwrap_or("?"),
            record.get("call_id").and_then(Value::as_str).unwrap_or("?")
        ));
        if let Some(call) = &call {
            match serde_json::from_str::<Value>(&call.arguments) {
                Ok(arguments) => append_value(&mut lines, "arguments", &arguments, 2),
                Err(_) => lines.push(format!("  arguments: {}", call.arguments)),
            }
        }
        let parsed_result = record
            .get("result")
            .filter(|value| !value.is_null())
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolResult>(value).ok());
        if let Some(tool_result) = parsed_result {
            lines.extend(result(&tool_result).lines().map(|line| format!("  {line}")));
        }
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use saya_core::contract::ErrorInfo;
    use serde_json::json;

    #[test]
    fn expanded_result_uses_fields_and_output_structure() {
        let text = result(&ToolResult {
            ok: false,
            data: Some(json!({
                "stdout": "first\nsecond",
                "files": ["a.rs", "b.rs"],
                "nested": {"exit_code": 2}
            })),
            error: Some(ErrorInfo {
                code: "fixture".into(),
                message: "controlled failure".into(),
            }),
            truncated: true,
        });
        assert!(text.contains("Status: failed"));
        assert!(text.contains("Error: fixture"));
        assert!(text.contains("Output:"));
        assert!(text.contains("files: a.rs, b.rs"));
        assert!(text.contains("exit_code: 2"));
        assert!(!text.contains('{'));
    }
}
