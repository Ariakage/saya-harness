//! Human-readable projections of canonical tool records and results.
use crate::i18n::browser as browser_text;
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
    if name.starts_with("browser_") {
        return browser_call(name, arguments, elapsed);
    }
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

fn browser_call(name: &str, arguments: &Value, elapsed: Option<Duration>) -> String {
    let action = browser_text::action(name);
    let mut lines = vec![format!("{}: {action}", browser_text::ACTION)];
    for (key, label) in [
        ("page", browser_text::PAGE),
        ("document", browser_text::DOCUMENT),
        ("element_ref", browser_text::ELEMENT),
        ("url", browser_text::URL),
        ("key", browser_text::KEY),
        ("button", browser_text::BUTTON),
        ("milliseconds", browser_text::MILLISECONDS),
        ("artifact_ref", browser_text::ARTIFACT),
        ("credential_slot", browser_text::CREDENTIAL_SLOT),
    ] {
        if let Some(value) = arguments.get(key) {
            lines.push(format!(
                "  {label}: {}",
                crate::editor::safe_text(&scalar(value))
            ));
        }
    }
    if let Some(value) = arguments.get("text").and_then(Value::as_str) {
        lines.push(format!(
            "  {}: {} {}",
            browser_text::TEXT_LABEL,
            value.len(),
            browser_text::TEXT_BYTES
        ));
    } else if arguments.get("text").is_some() {
        lines.push(format!("  {}: <structured>", browser_text::TEXT_LABEL));
    }
    if arguments.get("background").and_then(Value::as_bool) == Some(true) {
        lines.push(format!("  {}", browser_text::BACKGROUND));
    }
    if let Some(elapsed) = elapsed {
        lines.push(String::new());
        lines.push(format!(
            "{}: {}",
            text::OBSERVED_DURATION,
            crate::i18n::summary::duration(elapsed)
        ));
    }
    lines.join("\n")
}

/// One-line action label for approval prompts. Fill text is represented by its
/// length so a model-supplied value is not replayed into the interface.
pub(crate) fn approval_action(name: &str, arguments: &Value) -> Option<String> {
    if !name.starts_with("browser_") {
        return None;
    }
    let action = browser_text::action(name);
    let Some(object) = arguments.as_object() else {
        return Some(format!("{action} · arguments withheld"));
    };
    let mut parts = vec![action.to_owned()];
    let mut unknown = Vec::new();
    for (key, value) in object {
        match key.as_str() {
            "page" => parts.push(format!("{} {}", browser_text::PAGE, scalar(value))),
            "document" => parts.push(format!("{} {}", browser_text::DOCUMENT, scalar(value))),
            "element_ref" => parts.push(format!("{} {}", browser_text::ELEMENT, scalar(value))),
            "url" => parts.push(scalar(value)),
            "key" => parts.push(format!("{} {}", browser_text::KEY, scalar(value))),
            "button" => parts.push(format!("{} {}", browser_text::BUTTON, scalar(value))),
            "milliseconds" => {
                parts.push(format!("{} {}", browser_text::MILLISECONDS, scalar(value)))
            }
            "artifact_ref" => parts.push(format!("{} {}", browser_text::ARTIFACT, scalar(value))),
            "credential_slot" => parts.push(format!(
                "{} {}",
                browser_text::CREDENTIAL_SLOT,
                scalar(value)
            )),
            "text" => match value.as_str() {
                Some(text) => parts.push(format!(
                    "{} {} {}",
                    browser_text::TEXT_LABEL,
                    text.len(),
                    browser_text::TEXT_BYTES
                )),
                None => parts.push(format!("{} <structured>", browser_text::TEXT_LABEL)),
            },
            "background" => {
                if value.as_bool() == Some(true) {
                    parts.push(browser_text::BACKGROUND.into());
                }
            }
            other => unknown.push(other.to_owned()),
        }
    }
    if !unknown.is_empty() {
        unknown.sort();
        parts.push(format!("{}: {}", text::ARGUMENTS, unknown.join(",")));
    }
    Some(
        crate::editor::safe_text(&parts.join(" · "))
            .replace('\n', " ")
            .replace('\t', "    "),
    )
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

pub(crate) fn result_named(name: &str, result: &ToolResult) -> String {
    if name.starts_with("browser_") {
        return browser_result(name, result);
    }
    crate::tool_render::result(result)
}

fn browser_result(name: &str, result: &ToolResult) -> String {
    let safe = crate::editor::safe_text;
    let mut lines = vec![format!(
        "{} · {} · {}",
        browser_text::ACTION,
        browser_text::action(name),
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
        lines.push(format!("{}: {}", text::ERROR, safe(&error.code)));
        lines.push(format!("{}: {}", text::MESSAGE, safe(&error.message)));
    }
    let Some(data) = result.data.as_ref() else {
        return lines.join("\n");
    };
    if let Some(source) = data.get("source").and_then(Value::as_str) {
        lines.push(format!("  {}: {}", browser_text::SOURCE, safe(source)));
    }
    // Screenshot/download/upload results keep their record in a nested object;
    // page and revision live there, not at the top level.
    let nested = ["screenshot", "download", "upload"]
        .iter()
        .find_map(|key| data.get(*key).map(|value| (*key, value)));
    let record = nested.map_or(data, |(_, value)| value);
    if nested.is_none() {
        for (key, label) in [
            ("page", browser_text::PAGE),
            ("document", browser_text::DOCUMENT),
            ("document_revision", browser_text::REVISION),
            ("url", browser_text::URL),
            ("title", browser_text::TITLE),
        ] {
            if let Some(value) = record.get(key) {
                lines.push(format!("  {label}: {}", safe(&scalar(value))));
            }
        }
        for (key, label) in [
            ("detail", browser_text::DETAIL),
            ("status", browser_text::STATUS),
        ] {
            if let Some(value) = record.get(key) {
                lines.push(format!("  {label}: {}", safe(&scalar(value))));
            }
        }
        if record.get("document_stale").and_then(Value::as_bool) == Some(true) {
            lines.push(format!("  {}", browser_text::DOCUMENT_STALE));
        }
        if record.get("background").and_then(Value::as_bool) == Some(true) {
            lines.push(format!("  {}", browser_text::BACKGROUND));
        }
    }
    if data.get("closed").and_then(Value::as_bool) == Some(true) {
        lines.push(format!("  {}", browser_text::CLOSED));
    }
    if let Some(job) = record.get("job_id").and_then(Value::as_str) {
        lines.push(format!("  {}: {}", browser_text::JOB, safe(job)));
    }
    if let Some(elements) = record.get("elements").and_then(Value::as_array) {
        lines.push(format!(
            "  {}: {} {}",
            browser_text::ELEMENT,
            elements.len(),
            browser_text::ELEMENTS
        ));
    }
    if let Some(outputs) = data.get("output_refs").and_then(Value::as_array) {
        lines.push(format!(
            "  {}: {}",
            browser_text::OUTPUT_REFS,
            outputs.len()
        ));
        for output in outputs {
            if let Some(reference) = output.get("output_ref").and_then(Value::as_str) {
                lines.push(format!("    {}", safe(reference)));
            }
        }
    }
    for (key, label) in [
        ("preview", browser_text::PREVIEW),
        ("text", browser_text::TEXT),
    ] {
        if let Some(value) = data.get(key).and_then(Value::as_str) {
            let value = bound_inline(value, 2048);
            lines.push(format!("  {label}: {}", safe(&value)));
        }
    }
    if let Some((label, value)) = nested {
        lines.push(String::new());
        lines.push(format!("{label}:"));
        for (field, field_label) in [
            ("page", browser_text::PAGE),
            ("url", browser_text::URL),
            ("url_observation", browser_text::URL_OBSERVATION),
            ("document_revision", browser_text::REVISION),
            ("filename", browser_text::FILENAME),
            ("mime", browser_text::MIME),
            ("bytes", browser_text::SIZE),
            ("bytes_sent", browser_text::BYTES_SENT),
            ("width", browser_text::WIDTH),
            ("height", browser_text::HEIGHT),
            ("sha256", browser_text::SHA256),
            ("artifact_ref", browser_text::ARTIFACT),
        ] {
            if let Some(field_value) = value.get(field) {
                lines.push(format!("  {field_label}: {}", safe(&scalar(field_value))));
            }
        }
        if let Some(stored) = value.get("artifact_stored").and_then(Value::as_bool) {
            lines.push(format!(
                "  {}: {}",
                browser_text::ARTIFACT_STORED,
                if stored {
                    browser_text::YES
                } else {
                    browser_text::NO
                }
            ));
        }
        if let Some(masked) = value.get("masked").and_then(Value::as_bool) {
            lines.push(format!(
                "  {}: {}",
                browser_text::MASKED,
                if masked {
                    browser_text::YES
                } else {
                    browser_text::NO
                }
            ));
        }
        if let Some(capability) = value.get("model_image_capability").and_then(Value::as_str) {
            lines.push(format!(
                "  {}: {}",
                browser_text::MODEL_IMAGE_CAPABILITY,
                safe(capability)
            ));
        }
        if let Some(status) = value.get("model_image_status").and_then(Value::as_str) {
            lines.push(format!("  {}: {}", browser_text::MODEL_IMAGE, safe(status)));
        }
    }
    if record.get("truncated").and_then(Value::as_bool) == Some(true) {
        lines.push(format!("  {}", browser_text::TRUNCATED));
    }
    lines.join("\n")
}

fn bound_inline(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
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
        result_named(name, result)
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
            lines.extend(
                result_named(name, &tool_result)
                    .lines()
                    .map(|line| format!("  {line}")),
            );
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
            media_refs: Vec::new(),
        });
        assert!(text.contains("Result · failed"));
        assert!(text.contains("Error: fixture"));
        assert!(text.contains("Output"));
        assert!(text.contains("files: a.rs, b.rs"));
        assert!(text.contains("exit_code: 2"));
        assert!(!text.contains('{'));
    }

    #[test]
    fn browser_call_shows_key_button_wait_artifact_and_hides_fill_text() {
        let rendered = call(
            "browser_press",
            &json!({
                "page": "page_demo",
                "document": "doc_demo",
                "key": "Enter"
            }),
            None,
        );
        assert!(rendered.contains("Key: Enter"));
        assert!(rendered.contains("Document: doc_demo"));

        let rendered = call(
            "browser_click",
            &json!({
                "page": "page_demo",
                "document": "doc_demo",
                "element_ref": "el_demo",
                "button": "right"
            }),
            None,
        );
        assert!(rendered.contains("Button: right"));

        let rendered = call(
            "browser_wait",
            &json!({"page": "page_demo", "milliseconds": 250}),
            None,
        );
        assert!(rendered.contains("Milliseconds: 250"));

        let rendered = call(
            "browser_upload",
            &json!({
                "page": "page_demo",
                "document": "doc_demo",
                "element_ref": "el_demo",
                "artifact_ref": "media_demo"
            }),
            None,
        );
        assert!(rendered.contains("Artifact: media_demo"));

        let rendered = call(
            "browser_fill",
            &json!({
                "page": "page_demo",
                "document": "doc_demo",
                "element_ref": "el_demo",
                "text": "secret\u{1b}[31m"
            }),
            None,
        );
        assert!(rendered.contains("Text: 11 text bytes"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn browser_call_and_result_use_specific_labels_and_hide_host_fields() {
        let call = call(
            "browser_fill",
            &json!({
                "page": "page_demo",
                "document": "doc_demo",
                "element_ref": "el_demo",
                "credential_slot": "login"
            }),
            None,
        );
        assert!(call.contains("Action: fill"));
        assert!(call.contains("Page: page_demo"));
        assert!(call.contains("Credential slot: login"));
        assert!(!call.contains("Argument"));

        let result = result_named(
            "browser_screenshot",
            &ToolResult {
                ok: true,
                data: Some(json!({
                    "source": "untrusted_browser",
                    "screenshot": {
                        "page": "page_demo",
                        "document_revision": 2,
                        "url": "https://allowed.example/final",
                        "url_observation": "last_confirmed",
                        "mime": "image/png",
                        "bytes": 42,
                        "sha256": "abcdef",
                        "artifact_ref": "media_demo",
                        "masked": true,
                        "model_image_capability": "disabled",
                        "model_image_status": "artifact_saved: disabled",
                        "media_ref": "media_demo"
                    }
                })),
                error: None,
                truncated: false,
                media_refs: vec!["media_demo".into()],
            },
        );
        for expected in [
            "Action · screenshot · completed",
            "Page: page_demo",
            "Revision: 2",
            "URL: https://allowed.example/final",
            "URL observation: last_confirmed",
            "MIME: image/png",
            "Size: 42",
            "SHA-256: abcdef",
            "Artifact: media_demo",
            "Masked: yes",
            "Model image capability: disabled",
            "Model image: artifact_saved: disabled",
        ] {
            assert!(result.contains(expected), "{expected}: {result}");
        }
        assert!(!result.contains("media_ref"));
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
                media_refs: Vec::new(),
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
