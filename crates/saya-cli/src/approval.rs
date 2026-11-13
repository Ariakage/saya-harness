//! Terminal-safe approval rendering shared by the chat and `run --interactive`
//! front ends. Browser actions never replay raw model arguments: fill text is
//! reduced to a byte count, and only the fields needed to understand the action
//! are shown.

use serde_json::Value;

/// Replace terminal control characters (including bidi overrides) so provider
/// or model text cannot inject escape sequences into an approval prompt.
pub(crate) fn safe_terminal_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if saya_core::text::is_terminal_control(character)
                && character != '\n'
                && character != '\t'
            {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

/// Build a safe action summary for a browser approval request. Returns `None`
/// for non-browser tools so callers keep their existing generic rendering.
///
/// `browser_fill` with `text` contributes only its byte count. Unknown keys are
/// listed by name without their values so an unfamiliar shape cannot leak
/// model-supplied content into the terminal.
pub(crate) fn browser_action(tool: &str, arguments: &Value) -> Option<String> {
    let action = tool.strip_prefix("browser_")?;
    let Some(object) = arguments.as_object() else {
        return Some(format!("{action} · arguments withheld"));
    };
    let mut parts = vec![action.to_owned()];
    let mut unknown = Vec::new();
    for (key, value) in object {
        match key.as_str() {
            "page" => push_scalar(&mut parts, "page", value),
            "document" => push_scalar(&mut parts, "document", value),
            "element_ref" => push_scalar(&mut parts, "element", value),
            "url" => push_scalar(&mut parts, "url", value),
            "key" => push_scalar(&mut parts, "key", value),
            "button" => push_scalar(&mut parts, "button", value),
            "milliseconds" => push_scalar(&mut parts, "milliseconds", value),
            "artifact_ref" => push_scalar(&mut parts, "artifact", value),
            "credential_slot" => push_scalar(&mut parts, "credential slot", value),
            "source" => push_scalar(&mut parts, "source", value),
            "text" => match value.as_str() {
                Some(text) => parts.push(format!("text {} bytes", text.len())),
                None => parts.push("text <structured>".into()),
            },
            "background" => {
                if value.as_bool() == Some(true) {
                    parts.push("background".into());
                }
            }
            other => unknown.push(other.to_owned()),
        }
    }
    if !unknown.is_empty() {
        unknown.sort();
        parts.push(format!("additional fields: {}", unknown.join(", ")));
    }
    Some(parts.join(" · "))
}

fn push_scalar(parts: &mut Vec<String>, label: &str, value: &Value) {
    match value {
        Value::String(value) => parts.push(format!("{label} {value}")),
        Value::Number(value) => parts.push(format!("{label} {value}")),
        Value::Bool(value) => parts.push(format!("{label} {value}")),
        _ => parts.push(format!("{label} <structured>")),
    }
}

/// Render the approval argument block. Browser tools never include the raw
/// JSON; unknown and non-browser tools keep a sanitized generic rendering.
pub(crate) fn argument_lines(tool: &str, arguments: &Value) -> String {
    match browser_action(tool, arguments) {
        Some(summary) => {
            let summary = safe_terminal_text(&summary)
                .replace('\n', "\\n")
                .replace('\t', "\\t");
            format!("Action: {summary}\n")
        }
        None => {
            let arguments =
                serde_json::to_string(arguments).unwrap_or_else(|_| "<unavailable>".into());
            format!("Arguments: {}\n", safe_terminal_text(&arguments))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn browser_fill_text_is_only_a_byte_count() {
        let lines = argument_lines(
            "browser_fill",
            &json!({
                "page": "page_demo",
                "document": "doc_demo",
                "element_ref": "el_demo",
                "text": "secret-password\u{1b}[31m"
            }),
        );
        assert!(lines.contains("Action: fill"));
        assert!(lines.contains("page page_demo"));
        assert!(lines.contains("document doc_demo"));
        assert!(lines.contains("element el_demo"));
        assert!(lines.contains("text 20 bytes"));
        assert!(!lines.contains("Arguments:"));
        assert!(!lines.contains("secret-password"));
        assert!(!lines.contains('\u{1b}'));
    }

    #[test]
    fn other_browser_actions_show_only_the_needed_fields() {
        let lines = argument_lines(
            "browser_press",
            &json!({
                "page": "page_demo",
                "document": "doc_demo",
                "key": "Enter",
                "unknown": {"nested": "value"}
            }),
        );
        assert!(lines.contains("Action: press"));
        assert!(lines.contains("key Enter"));
        assert!(lines.contains("additional fields: unknown"));
        assert!(!lines.contains("nested"));
        assert!(!lines.contains("Arguments:"));

        let lines = argument_lines(
            "browser_wait",
            &json!({"page": "page_demo", "milliseconds": 250, "background": true}),
        );
        assert!(lines.contains("milliseconds 250"));
        assert!(lines.contains("background"));

        let lines = argument_lines(
            "browser_upload",
            &json!({
                "page": "page_demo",
                "document": "doc_demo",
                "element_ref": "el_demo",
                "artifact_ref": "media_demo"
            }),
        );
        assert!(lines.contains("artifact media_demo"));
    }

    #[test]
    fn non_browser_tools_keep_a_sanitized_generic_rendering() {
        let lines = argument_lines("exec", &json!({"argv": ["echo", "\u{1b}[2J"]}));
        assert!(lines.starts_with("Arguments: "));
        assert!(!lines.contains('\u{1b}'));
    }

    #[test]
    fn structured_browser_arguments_are_not_rendered_as_raw_json() {
        let lines = argument_lines(
            "browser_fill",
            &json!({
                "page": "page_demo",
                "credential_slot": "login",
                "text": null
            }),
        );
        assert!(lines.contains("credential slot login"));
        assert!(lines.contains("text <structured>"));
        assert!(!lines.contains("Arguments:"));
    }
}
