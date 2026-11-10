//! Human-readable projections of canonical tool records and results.
use crate::i18n::tool as text;
use saya_core::contract::{ToolCall, ToolResult};
use serde_json::Value;
use std::time::Duration;

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => text::NULL.into(),
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
            lines.push(format!("{prefix}{label}: {} {}", values.len(), text::ITEMS));
            for (index, value) in values.iter().enumerate() {
                append_value(lines, &format!("[{index}]"), value, indent + 2);
            }
        }
        value => lines.push(format!("{prefix}{label}: {}", scalar(value))),
    }
}

fn append_object(lines: &mut Vec<String>, value: &Value, indent: usize) {
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if !key.starts_with("__saya_") {
                    append_value(lines, key, value, indent);
                }
            }
        }
        value => append_value(lines, text::ARGUMENTS, value, indent),
    }
}

fn shell_argument(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_./:=@%+-".contains(character))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(crate) fn command_text(arguments: &Value) -> Option<String> {
    let argv = arguments.get("argv")?.as_array()?;
    if argv.is_empty() {
        return None;
    }
    Some(
        argv.iter()
            .map(|argument| argument.as_str().map(shell_argument))
            .collect::<Option<Vec<_>>>()?
            .join(" "),
    )
}

pub(crate) fn call(name: &str, arguments: &Value, elapsed: Option<Duration>) -> String {
    let mut lines = Vec::new();
    if name == "exec" {
        lines.push(text::COMMAND.into());
        lines.push(format!(
            "  {}",
            command_text(arguments).unwrap_or_else(|| text::UNAVAILABLE.into())
        ));
        lines.push(String::new());
        lines.push(text::WORKING_DIRECTORY.into());
        lines.push(format!(
            "  {}",
            arguments
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or(text::SESSION_WORKSPACE)
        ));
        if let Some(timeout) = arguments.get("timeout_seconds").and_then(Value::as_u64) {
            lines.push(String::new());
            lines.push(text::TIMEOUT_LABEL.into());
            lines.push(format!("  {timeout}s"));
        }
        if let Some(environment) = arguments.get("env").and_then(Value::as_object)
            && !environment.is_empty()
        {
            lines.push(String::new());
            lines.push(text::ENVIRONMENT.into());
            lines.extend(environment.keys().map(|name| format!("  {name}")));
        }
        if let Some(elapsed) = elapsed {
            lines.push(String::new());
            lines.push(text::OBSERVED_DURATION.into());
            lines.push(format!("  {}", crate::i18n::summary::duration(elapsed)));
        }
        return lines.join("\n");
    }
    lines.push(text::ARGUMENTS.into());
    append_object(&mut lines, arguments, 2);
    lines.join("\n")
}

fn exec_result(data: &Value) -> Option<Vec<String>> {
    let object = data.as_object()?;
    if !object.contains_key("exit_code")
        || !object.contains_key("stdout")
        || !object.contains_key("stderr")
        || !object.contains_key("timed_out")
        || !object.contains_key("cancelled")
    {
        return None;
    }
    let exit = object
        .get("exit_code")
        .and_then(Value::as_i64)
        .map_or_else(|| "—".into(), |code| code.to_string());
    let mut lines = vec![format!("  {}: {exit}", text::EXIT_CODE)];
    if object.get("timed_out").and_then(Value::as_bool) == Some(true) {
        lines.push(format!("  {}", text::TIMEOUT));
    }
    if object.get("cancelled").and_then(Value::as_bool) == Some(true) {
        lines.push(format!("  {}", text::CANCELLED));
    }
    for (key, label) in [("stdout", text::STDOUT), ("stderr", text::STDERR)] {
        let output = object.get(key).and_then(Value::as_str).unwrap_or("");
        lines.push(format!("  {label}:"));
        if output.is_empty() {
            lines.push(format!("    {}", text::EMPTY));
        } else {
            lines.extend(output.lines().map(|line| format!("    {line}")));
        }
    }
    Some(lines)
}

pub(crate) fn result(result: &ToolResult) -> String {
    if let Some(execution) = result.data.as_ref().and_then(exec_result) {
        let mut lines = vec![format!(
            "{} · {}",
            text::EXECUTION_RESULT,
            if result.ok {
                text::COMPLETED
            } else {
                text::FAILED
            }
        )];
        if result.truncated {
            lines.push(text::OUTPUT_TRUNCATED.into());
        }
        if let Some(error) = &result.error {
            lines.push(format!("{}: {}", text::ERROR, error.code));
            lines.push(format!("{}: {}", text::MESSAGE, error.message));
        }
        lines.extend(execution);
        return lines.join("\n");
    }
    let mut lines = vec![format!(
        "{} · {}",
        text::RESULT,
        if result.ok {
            text::COMPLETED
        } else {
            text::FAILED
        }
    )];
    if result.truncated {
        lines.push(text::OUTPUT_TRUNCATED.into());
    }
    if let Some(error) = &result.error {
        lines.push(format!("{}: {}", text::ERROR, error.code));
        lines.push(format!("{}: {}", text::MESSAGE, error.message));
    }
    if let Some(data) = &result.data {
        lines.push(String::new());
        lines.push(text::OUTPUT.into());
        append_object(&mut lines, data, 2);
    }
    lines.join("\n")
}

pub(crate) fn call_and_result(
    name: &str,
    arguments: &Value,
    result: &ToolResult,
    elapsed: Option<Duration>,
) -> String {
    format!(
        "{}\n\n{}",
        call(name, arguments, elapsed),
        crate::tool_render::result(result)
    )
}

pub(crate) fn record_lines(records: &[Value]) -> Vec<String> {
    if records.is_empty() {
        return vec![text::NO_CALLS.into()];
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
            .unwrap_or(text::UNKNOWN);
        let name = call
            .as_ref()
            .map_or(text::UNKNOWN_TOOL, |call| call.name.as_str());
        lines.push(format!("#{} {name} · {state}", index + 1));
        lines.push(format!(
            "  {} {} · {} {}",
            text::RUN,
            record.get("run_id").and_then(Value::as_str).unwrap_or("?"),
            text::CALL,
            record.get("call_id").and_then(Value::as_str).unwrap_or("?")
        ));
        if let Some(call) = &call {
            match serde_json::from_str::<Value>(&call.arguments) {
                Ok(arguments) => lines.extend(
                    crate::tool_render::call(&call.name, &arguments, None)
                        .lines()
                        .map(|line| format!("  {line}")),
                ),
                Err(_) => lines.push(format!("  {}: {}", text::ARGUMENTS, call.arguments)),
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
        assert!(text.contains("Result · failed"));
        assert!(text.contains("Error: fixture"));
        assert!(text.contains("Output"));
        assert!(text.contains("files: a.rs, b.rs"));
        assert!(text.contains("exit_code: 2"));
        assert!(!text.contains('{'));
    }

    #[test]
    fn exec_call_and_result_show_command_context_and_output_without_internal_json() {
        let arguments = json!({
            "argv": ["/usr/bin/swift", ".tmp_vision.swift", "reference image.png"],
            "cwd": "/path/to/project",
            "timeout_seconds": 180,
            "__saya_command_analysis": {"schema_version": 1}
        });
        let text = call_and_result(
            "exec",
            &arguments,
            &ToolResult {
                ok: true,
                data: Some(json!({
                    "exit_code": 0,
                    "stdout": "vision ok\n",
                    "stderr": "",
                    "stdout_truncated": false,
                    "stderr_truncated": false,
                    "timed_out": false,
                    "cancelled": false
                })),
                error: None,
                truncated: false,
            },
            Some(Duration::from_millis(1240)),
        );
        assert!(text.contains("Command"));
        assert!(text.contains("/usr/bin/swift .tmp_vision.swift 'reference image.png'"));
        assert!(text.contains("Working directory"));
        assert!(text.contains("Timeout"));
        assert!(text.contains("Observed duration"));
        assert!(text.contains("Execution result"));
        assert!(text.contains("Exit code"));
        assert!(text.contains("vision ok"));
        assert!(!text.contains("__saya_command_analysis"));
        assert!(!text.contains('{'));
    }
}
