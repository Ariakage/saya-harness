//! Human-readable projection of tool calls and results for desktop cards.

use serde_json::Value;
use soyo_core::{contract::ToolResult, tools::todo::result_todos};
use std::time::Duration;

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        let secs = duration.as_secs_f64();
        if secs < 60.0 {
            format!("{secs:.1}s")
        } else {
            let mins = duration.as_secs() / 60;
            let rem = duration.as_secs() % 60;
            format!("{mins}m {rem}s")
        }
    }
}

fn one_line(raw: &str, max_chars: usize) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = collapsed.chars().take(max_chars).collect();
    if collapsed.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => v.clone(),
        _ => String::new(),
    }
}

fn shell_argument(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:=@%+-".contains(c))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub fn command_text(arguments: &Value) -> Option<String> {
    let argv = arguments.get("argv")?.as_array()?;
    if argv.is_empty() {
        return None;
    }
    Some(
        argv.iter()
            .map(|arg| arg.as_str().map(shell_argument))
            .collect::<Option<Vec<_>>>()?
            .join(" "),
    )
}

pub fn approval_action(name: &str, arguments: &Value) -> Option<String> {
    if !name.starts_with("browser_") {
        return None;
    }
    let detail = match name {
        "browser_open" | "browser_navigate" => arguments
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "browser_fill" => {
            if let Some(slot) = arguments.get("credential_slot").and_then(Value::as_str) {
                Some(format!("credential_slot={slot}"))
            } else {
                arguments
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|t| format!("{} bytes", t.len()))
            }
        }
        _ => arguments
            .get("element")
            .and_then(Value::as_str)
            .map(str::to_owned),
    };
    Some(match detail {
        Some(detail) => format!("{name} ({detail})"),
        None => name.to_owned(),
    })
}

fn append_value(lines: &mut Vec<String>, label: &str, value: &Value, indent: usize) {
    let prefix = " ".repeat(indent);
    match value {
        Value::Object(map) => {
            lines.push(format!("{prefix}{label}:"));
            for (k, v) in map {
                if !k.starts_with("__soyo_") && !k.starts_with("__saya_") {
                    append_value(lines, k, v, indent + 2);
                }
            }
        }
        Value::Array(items) if items.iter().all(|v| !v.is_object() && !v.is_array()) => {
            lines.push(format!(
                "{prefix}{label}: {}",
                items.iter().map(scalar).collect::<Vec<_>>().join(", ")
            ));
        }
        Value::Array(items) => {
            lines.push(format!("{prefix}{label}: {} item(s)", items.len()));
            for (idx, item) in items.iter().enumerate() {
                append_value(lines, &format!("[{idx}]"), item, indent + 2);
            }
        }
        other => lines.push(format!("{prefix}{label}: {}", scalar(other))),
    }
}

pub fn call(name: &str, arguments: &Value, elapsed: Option<Duration>) -> String {
    let mut lines = Vec::new();
    if name == "exec" {
        lines.push("Command:".into());
        lines.push(format!(
            "  {}",
            command_text(arguments).unwrap_or_else(|| "unavailable".into())
        ));
        lines.push(String::new());
        lines.push("Working directory:".into());
        lines.push(format!(
            "  {}",
            arguments
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("session workspace")
        ));
        if let Some(timeout) = arguments.get("timeout_seconds").and_then(Value::as_u64) {
            lines.push(String::new());
            lines.push("Timeout:".into());
            lines.push(format!("  {timeout}s"));
        }
        if let Some(env) = arguments.get("env").and_then(Value::as_object)
            && !env.is_empty()
        {
            lines.push(String::new());
            lines.push("Environment variables:".into());
            lines.extend(env.keys().map(|k| format!("  {k}")));
        }
    } else if name == "browser_fill" {
        lines.push("Arguments:".into());
        if let Some(map) = arguments.as_object() {
            for (k, v) in map {
                if k == "text" {
                    if let Some(text) = v.as_str() {
                        lines.push(format!("  text: <{} bytes>", text.len()));
                    }
                } else if !k.starts_with("__soyo_") && !k.starts_with("__saya_") {
                    append_value(&mut lines, k, v, 2);
                }
            }
        }
    } else if let Some(map) = arguments.as_object() {
        if !map.is_empty() {
            lines.push("Arguments:".into());
            for (k, v) in map {
                if !k.starts_with("__soyo_") && !k.starts_with("__saya_") {
                    append_value(&mut lines, k, v, 2);
                }
            }
        }
    } else {
        append_value(&mut lines, "Arguments", arguments, 0);
    }

    if let Some(elapsed) = elapsed {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(format!("Observed duration: {}", format_duration(elapsed)));
    }
    lines.join("\n")
}

pub fn result(result: &ToolResult) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "Status: {}{}",
        if result.ok { "succeeded" } else { "failed" },
        if result.truncated { " (truncated)" } else { "" }
    ));
    if let Some(error) = &result.error {
        lines.push(format!("Error: {} — {}", error.code, error.message));
    }
    if let Some(data) = &result.data {
        if let Some(map) = data.as_object() {
            for (k, v) in map {
                if !k.starts_with("__soyo_") && !k.starts_with("__saya_") {
                    append_value(&mut lines, k, v, 0);
                }
            }
        } else {
            append_value(&mut lines, "Output", data, 0);
        }
    }
    if !result.media_refs.is_empty() {
        lines.push(format!("Artifacts: {}", result.media_refs.join(", ")));
    }
    lines.join("\n")
}

pub fn call_and_result(
    name: &str,
    arguments: &Value,
    tool_result: &ToolResult,
    elapsed: Option<Duration>,
) -> String {
    let call_part = call(name, arguments, elapsed);
    let result_part = result(tool_result);
    if call_part.is_empty() {
        result_part
    } else {
        format!("{call_part}\n\n{result_part}")
    }
}

pub fn summarize_call(
    name: &str,
    arguments: Option<&Value>,
    tool_result: Option<&ToolResult>,
    elapsed: Option<Duration>,
) -> Option<String> {
    if name == "exec" {
        let cmd = arguments.and_then(command_text)?;
        let mut parts = vec![one_line(&cmd, 64)];
        if let Some(elapsed) = elapsed {
            parts.push(format_duration(elapsed));
        }
        if let Some(res) = tool_result {
            if let Some(exit) = res
                .data
                .as_ref()
                .and_then(|d| d.get("exit_code"))
                .and_then(Value::as_i64)
            {
                parts.push(format!("exit {exit}"));
            }
            if !res.ok
                && let Some(err) = &res.error
            {
                parts.push(one_line(&err.code, 24));
            }
        } else {
            parts.push("running".into());
        }
        return Some(one_line(&parts.join(" · "), 120));
    }

    if name == "todo_write"
        && let Some(res) = tool_result
        && let Some(todos) = result_todos(res)
    {
        let done = todos
            .iter()
            .filter(|t| t.status == soyo_core::tools::todo::TodoStatus::Completed)
            .count();
        return Some(format!("{done}/{} tasks completed", todos.len()));
    }

    if let Some(res) = tool_result {
        if !res.ok
            && let Some(err) = &res.error
        {
            return Some(one_line(&format!("{} · {}", err.code, err.message), 120));
        }
        if let Some(data) = &res.data {
            if let Some(path) = data.get("path").and_then(Value::as_str) {
                if let (Some(start), Some(end)) = (
                    data.get("start_line").and_then(Value::as_u64),
                    data.get("end_line").and_then(Value::as_u64),
                ) {
                    return Some(one_line(&format!("{path}:{start}-{end}"), 120));
                }
                return Some(one_line(path, 120));
            }
            if let Some(count) = data
                .get("entries")
                .or_else(|| data.get("matches"))
                .or_else(|| data.get("files"))
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                return Some(format!("{count} item(s)"));
            }
        }
        return Some(if res.ok {
            "completed".into()
        } else {
            "failed".into()
        });
    }

    arguments
        .and_then(|v| v.get("path").and_then(Value::as_str))
        .map(|p| one_line(p, 120))
}
