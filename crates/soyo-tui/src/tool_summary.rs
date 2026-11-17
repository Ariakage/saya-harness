//! Short labels derived from canonical tool result payloads.
use crate::{
    editor::safe_text,
    i18n::{browser as browser_text, summary, tool as text},
    view::MAX_MESSAGE_BYTES,
};
use serde_json::Value;
use soyo_core::{
    contract::ToolResult,
    tools::todo::{TodoStatus, result_todos},
};
use unicode_segmentation::UnicodeSegmentation;

const MAX_SUMMARY_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolSummary {
    pub text: String,
    pub ok: bool,
    pub truncated: bool,
    pub running: bool,
}

/// Summarize supported, already-redacted ToolResult JSON for local display.
/// Unknown shapes or incomplete JSON retain the caller's original text view.
pub(crate) fn summarize(text: &str) -> Option<ToolSummary> {
    if text.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    serde_json::from_str::<ToolResult>(text).ok()?;
    let value: Value = serde_json::from_str(text).ok()?;
    if !fields(&value, &["ok", "data", "error", "truncated"]) {
        return None;
    }
    let ok = value["ok"].as_bool()?;
    let truncated = value["truncated"].as_bool()?;
    let (text, nested_truncated) = if ok {
        if !value["error"].is_null() {
            return None;
        }
        data_summary(&value["data"], truncated)?
    } else {
        let error = &value["error"];
        if !fields(error, &["code", "message"]) {
            return None;
        }
        let code = one_line(error["code"].as_str()?, 32);
        let message = one_line(error["message"].as_str()?, 64);
        let mut text = format!("{code} · {message}");
        let mut nested = false;
        if let Some((detail, cut)) = exec_summary(&value["data"]) {
            text.push_str(&format!(" · {detail}"));
            nested = cut;
        }
        (text, nested)
    };
    Some(ToolSummary {
        text: one_line(&text, 120),
        ok,
        truncated: truncated || nested_truncated,
        running: false,
    })
}

pub(crate) fn summarize_call(
    name: &str,
    arguments: &Value,
    result: Option<&ToolResult>,
    elapsed: Option<std::time::Duration>,
) -> Option<ToolSummary> {
    if name.starts_with("browser_") {
        return browser_call_summary(name, result);
    }
    if name != "exec" {
        return result.and_then(|result| {
            serde_json::to_string(result)
                .ok()
                .as_deref()
                .and_then(summarize)
        });
    }
    let command = crate::tool_render::command_text(arguments)?;
    let mut parts = vec![one_line(&command, 80)];
    let running = result.is_none();
    let mut ok = true;
    let mut truncated = false;
    if let Some(elapsed) = elapsed {
        parts.push(summary::duration(elapsed));
    }
    if let Some(result) = result {
        truncated = result.truncated;
        if let Some(data) = result.data.as_ref() {
            let exit = data.get("exit_code").and_then(Value::as_i64);
            if let Some(exit) = exit {
                parts.push(format!("{} {exit}", text::EXIT));
                ok &= exit == 0;
            }
            if data.get("timed_out").and_then(Value::as_bool) == Some(true) {
                parts.push(text::TIMEOUT.into());
                ok = false;
            }
            if data.get("cancelled").and_then(Value::as_bool) == Some(true) {
                parts.push(text::CANCELLED.into());
                ok = false;
            }
            truncated |= data.get("stdout_truncated").and_then(Value::as_bool) == Some(true)
                || data.get("stderr_truncated").and_then(Value::as_bool) == Some(true);
        }
        if !result.ok {
            ok = false;
            if let Some(error) = &result.error {
                parts.push(one_line(&error.code, 32));
            }
        }
    } else {
        parts.push(text::RUNNING.into());
    }
    Some(ToolSummary {
        text: one_line(&parts.join(" · "), 120),
        ok,
        truncated,
        running,
    })
}

fn browser_call_summary(name: &str, result: Option<&ToolResult>) -> Option<ToolSummary> {
    let action = browser_text::action(name);
    let Some(result) = result else {
        return Some(ToolSummary {
            text: format!("{action} · {}", text::RUNNING),
            ok: true,
            truncated: false,
            running: true,
        });
    };
    if !result.ok {
        let error = result.error.as_ref()?;
        return Some(ToolSummary {
            text: one_line(
                &format!(
                    "{action} · {} · {}",
                    one_line(&error.code, 32),
                    one_line(&error.message, 64)
                ),
                120,
            ),
            ok: false,
            truncated: result.truncated,
            running: false,
        });
    }
    let data = result.data.as_ref()?;
    let mut parts = vec![action.to_owned()];
    // Screenshot/download/upload records are nested; page and revision must be
    // read from the real object rather than assumed at the top level.
    let nested = ["screenshot", "download", "upload"]
        .iter()
        .find_map(|key| data.get(*key).map(|value| (*key, value)));
    let record = nested.map_or(data, |(_, value)| value);
    if let Some(page) = record.get("page").and_then(Value::as_str) {
        parts.push(format!("{} {}", browser_text::PAGE, one_line(page, 48)));
    }
    if let Some(revision) = record.get("document_revision").and_then(Value::as_u64) {
        parts.push(format!("{} {revision}", browser_text::REVISION));
    }
    if let Some(url) = record.get("url").and_then(Value::as_str) {
        parts.push(one_line(url, 80));
    }
    if let Some(job) = record.get("job_id").and_then(Value::as_str) {
        parts.push(format!("{} {}", browser_text::JOB, one_line(job, 40)));
    }
    if data.get("closed").and_then(Value::as_bool) == Some(true) {
        parts.push(browser_text::CLOSED.into());
    }
    if let Some((_, value)) = nested {
        if let Some(mime) = value.get("mime").and_then(Value::as_str) {
            parts.push(one_line(mime, 48));
        }
        if let Some(bytes) = value.get("bytes").and_then(Value::as_u64) {
            parts.push(format!("{bytes} {}", text::BYTES));
        }
        if let Some(bytes) = value.get("bytes_sent").and_then(Value::as_u64) {
            parts.push(format!("{bytes} {}", text::BYTES));
        }
        if value.get("masked").and_then(Value::as_bool) == Some(true) {
            parts.push(format!("{} {}", browser_text::MASKED, browser_text::YES));
        }
        if let Some(sha) = value.get("sha256").and_then(Value::as_str) {
            let short = sha.get(..12).unwrap_or(sha);
            parts.push(format!("{} {short}", browser_text::SHA256));
        }
        // The public artifact reference is needed to chain browser_upload; the
        // host-only media reference never participates in interface text.
        if let Some(artifact) = value.get("artifact_ref").and_then(Value::as_str) {
            parts.push(format!(
                "{} {}",
                browser_text::ARTIFACT,
                one_line(artifact, 24)
            ));
        }
    }
    if let Some(outputs) = data.get("output_refs").and_then(Value::as_array)
        && let Some(first) = outputs.first().and_then(|value| value.get("output_ref"))
        && let Some(reference) = first.as_str()
    {
        parts.push(format!(
            "{} {}",
            browser_text::OUTPUT,
            one_line(reference, 24)
        ));
    }
    Some(ToolSummary {
        text: one_line(&parts.join(" · "), 160),
        ok: true,
        truncated: result.truncated,
        running: false,
    })
}

fn data_summary(data: &Value, truncated: bool) -> Option<(String, bool)> {
    if data.get("todos").is_some() {
        let todos = result_todos(&ToolResult {
            ok: true,
            data: Some(data.clone()),
            error: None,
            truncated,
            media_refs: Vec::new(),
        })?;
        let done = todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Completed)
            .count();
        let active = todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::InProgress)
            .count();
        let kind = if todos.len() == 1 {
            text::TASK
        } else {
            text::TASKS
        };
        return Some((
            format!(
                "{} {kind} · {done} {}/{active} {}",
                todos.len(),
                text::DONE,
                text::ACTIVE
            ),
            false,
        ));
    }
    if data.get("exit_code").is_some() {
        let summary = exec_summary(data)?;
        if data["exit_code"].as_i64() != Some(0)
            || data["timed_out"].as_bool() != Some(false)
            || data["cancelled"].as_bool() != Some(false)
        {
            return None;
        }
        return Some(summary);
    }
    if fields(
        data,
        &["content", "start_line", "end_line", "version", "truncated"],
    ) {
        let content = data["content"].as_str()?;
        let start = data["start_line"].as_u64()?;
        let end = data["end_line"].as_u64()?;
        data["version"].as_str()?;
        let cut = data["truncated"].as_bool()?;
        if start == 0 || end < start - 1 {
            return None;
        }
        let range = if end < start {
            format!("0 {}", text::LINES)
        } else {
            format!("{} {start}–{end}", text::LINES)
        };
        return Some((format!("{range} · {} {}", content.len(), text::BYTES), cut));
    }
    if fields(data, &["path", "version", "created"]) {
        let path = one_line(data["path"].as_str()?, 80);
        data["version"].as_str()?;
        let action = if data["created"].as_bool()? {
            text::CREATED
        } else {
            text::UPDATED
        };
        return Some((format!("{action} {path}"), false));
    }
    if fields(data, &["items", "truncated"]) {
        let items = data["items"].as_array()?;
        let cut = data["truncated"].as_bool()?;
        let (singular, plural) = if items.is_empty() {
            (text::ITEM, text::ITEMS)
        } else if items.iter().all(Value::is_string) {
            (text::FILE, text::FILES)
        } else if items.iter().all(|item| {
            fields(item, &["path", "line", "text"])
                && item["path"].is_string()
                && item["line"].as_u64().is_some_and(|line| line > 0)
                && item["text"].is_string()
        }) {
            (text::MATCH, text::MATCHES)
        } else if items.iter().all(|item| {
            fields(item, &["name", "kind"])
                && item["name"].is_string()
                && matches!(
                    item["kind"].as_str(),
                    Some("file" | "directory" | "symlink" | "other")
                )
        }) {
            (text::ENTRY, text::ENTRIES)
        } else {
            return None;
        };
        let kind = if items.len() == 1 { singular } else { plural };
        return Some((format!("{} {kind}", items.len()), cut));
    }
    None
}

fn exec_summary(data: &Value) -> Option<(String, bool)> {
    if !fields(
        data,
        &[
            "exit_code",
            "stdout",
            "stderr",
            "stdout_truncated",
            "stderr_truncated",
            "timed_out",
            "cancelled",
        ],
    ) {
        return None;
    }
    let exit = if data["exit_code"].is_null() {
        "—".into()
    } else {
        i32::try_from(data["exit_code"].as_i64()?).ok()?.to_string()
    };
    data["stdout"].as_str()?;
    data["stderr"].as_str()?;
    let cut = data["stdout_truncated"].as_bool()? | data["stderr_truncated"].as_bool()?;
    let timed_out = data["timed_out"].as_bool()?;
    let cancelled = data["cancelled"].as_bool()?;
    let mut summary = format!("{} {exit}", text::EXIT);
    if timed_out {
        summary.push_str(&format!(" · {}", text::TIMEOUT));
    }
    if cancelled {
        summary.push_str(&format!(" · {}", text::CANCELLED));
    }
    Some((summary, cut))
}

fn fields(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
    })
}

fn one_line(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let safe = safe_text(text).replace('\n', "\\n");
    let mut text = String::new();
    let mut count = 0;
    for grapheme in safe.graphemes(true) {
        if count == limit || text.len().saturating_add(grapheme.len()) > MAX_SUMMARY_BYTES {
            while count == limit || text.len() + '…'.len_utf8() > MAX_SUMMARY_BYTES {
                let Some((offset, _)) = text.grapheme_indices(true).next_back() else {
                    break;
                };
                text.truncate(offset);
                count -= 1;
            }
            text.push('…');
            break;
        }
        text.push_str(grapheme);
        count += 1;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(data: Value) -> String {
        json!({"ok": true, "data": data, "error": null, "truncated": false}).to_string()
    }

    fn exec(exit: Value, timed_out: bool, cancelled: bool) -> Value {
        json!({
            "exit_code": exit,
            "stdout": "PRIVATE_STDOUT",
            "stderr": "PRIVATE_STDERR",
            "stdout_truncated": false,
            "stderr_truncated": false,
            "timed_out": timed_out,
            "cancelled": cancelled
        })
    }

    #[test]
    fn todo_counts_use_the_canonical_statuses_and_preserve_the_source() {
        let text = record(json!({"todos": [
            {"id": "one", "content": "PRIVATE_TASK_ONE", "status": "completed"},
            {"id": "two", "content": "PRIVATE_TASK_TWO", "status": "in_progress"}
        ]}));
        let original = text.clone();
        assert_eq!(
            summarize(&text),
            Some(ToolSummary {
                text: "2 tasks · 1 done/1 active".into(),
                ok: true,
                truncated: false,
                running: false,
            })
        );
        assert_eq!(text, original);
        assert_eq!(
            summarize(&record(json!({"todos": []}))).unwrap().text,
            "0 tasks · 0 done/0 active"
        );
        assert_eq!(
            summarize(&record(
                json!({"todos": [{"id":"one","content":"item","status":"pending"}]})
            ))
            .unwrap()
            .text,
            "1 task · 0 done/0 active"
        );
        assert!(
            summarize(&record(
                json!({"todos": [{"id":"one","content":"item","status":"invalid"}]})
            ))
            .is_none()
        );
    }

    #[test]
    fn exec_summarizes_exit_and_truncation_without_copying_output() {
        let mut data = exec(json!(0), false, false);
        data["stderr_truncated"] = json!(true);
        let summary = summarize(&record(data)).unwrap();
        assert_eq!(summary.text, "exit 0");
        assert!(summary.ok && summary.truncated);
        assert!(!summary.text.contains("PRIVATE"));
        let mut invalid = exec(json!(0), false, false);
        invalid["stdout"] = json!(false);
        assert!(summarize(&record(invalid)).is_none());
        assert!(summarize(&record(exec(json!(i64::MAX), false, false))).is_none());
        assert!(summarize(&record(exec(json!(1), false, false))).is_none());
        assert!(summarize(&record(exec(json!(0), true, false))).is_none());
        assert!(summarize(&record(exec(json!(0), false, true))).is_none());
    }

    #[test]
    fn exec_call_summary_keeps_the_command_timing_and_exit_status() {
        let arguments = json!({
            "argv": ["/usr/bin/swift", ".tmp_vision.swift", "reference.png"],
            "cwd": "/path/to/project"
        });
        let running = summarize_call("exec", &arguments, None, None).unwrap();
        assert!(running.running);
        assert!(running.text.contains("/usr/bin/swift"));
        assert!(running.text.ends_with("running"));

        let result = ToolResult {
            ok: true,
            data: Some(exec(json!(0), false, false)),
            error: None,
            truncated: false,
            media_refs: Vec::new(),
        };
        let finished = summarize_call(
            "exec",
            &arguments,
            Some(&result),
            Some(std::time::Duration::from_millis(1240)),
        )
        .unwrap();
        assert!(!finished.running);
        assert!(finished.text.contains("/usr/bin/swift"));
        assert!(finished.text.contains("1.2s"));
        assert!(finished.text.contains("exit 0"));
        assert!(!finished.text.contains("PRIVATE"));
    }

    #[test]
    fn failed_commands_keep_actual_errors_and_timeout_or_cancel_metadata() {
        for (code, message, timed_out, cancelled, detail) in [
            (
                "command_failed",
                "command exited unsuccessfully",
                false,
                false,
                "exit 2",
            ),
            (
                "timeout",
                "command timed out",
                true,
                false,
                "exit — · timeout",
            ),
            (
                "cancelled",
                "command cancelled",
                false,
                true,
                "exit — · cancelled",
            ),
        ] {
            let text = json!({
                "ok": false,
                "data": exec(if timed_out || cancelled { Value::Null } else { json!(2) }, timed_out, cancelled),
                "error": {"code": code, "message": message},
                "truncated": false
            }).to_string();
            let summary = summarize(&text).unwrap();
            assert_eq!(summary.text, format!("{code} · {message} · {detail}"));
            assert!(!summary.ok);
            assert!(!summary.text.contains("PRIVATE"));
        }
        let text = json!({"ok":false,"data":null,"error":{"code":"permission_denied","message":"operation denied"},"truncated":false}).to_string();
        assert_eq!(
            summarize(&text).unwrap().text,
            "permission_denied · operation denied"
        );
    }

    #[test]
    fn file_reads_use_observed_ranges_bytes_and_empty_pages() {
        let mut data = json!({"content":"中文\n", "start_line":4, "end_line":4, "version":"version", "truncated":true});
        let summary = summarize(&record(data.clone())).unwrap();
        assert_eq!(summary.text, "lines 4–4 · 7 bytes");
        assert!(summary.truncated);
        assert!(!summary.text.contains("中文"));
        data["content"] = json!("");
        data["end_line"] = json!(3);
        assert_eq!(
            summarize(&record(data.clone())).unwrap().text,
            "0 lines · 0 bytes"
        );
        data["end_line"] = json!(2);
        assert!(summarize(&record(data)).is_none());
    }

    #[test]
    fn pages_count_only_supported_item_shapes_and_do_not_copy_hit_text() {
        for (items, expected) in [
            (json!([]), "0 items"),
            (json!(["a.rs"]), "1 file"),
            (json!(["a.rs", "b.rs"]), "2 files"),
            (json!([{"name":"a.rs", "kind":"file"}]), "1 entry"),
            (
                json!([{"name":"a.rs", "kind":"file"}, {"name":"src", "kind":"directory"}]),
                "2 entries",
            ),
            (
                json!([{"path":"a.rs", "line":3, "text":"PRIVATE_MATCH"}]),
                "1 match",
            ),
            (
                json!([{"path":"a.rs", "line":3, "text":"PRIVATE_MATCH"}, {"path":"b.rs", "line":4, "text":"PRIVATE_MATCH"}]),
                "2 matches",
            ),
        ] {
            let summary = summarize(&record(json!({"items":items,"truncated":true}))).unwrap();
            assert_eq!(summary.text, expected);
            assert!(summary.truncated);
            assert!(!summary.text.contains("PRIVATE"));
        }
        for items in [
            json!(["path", {"name":"mixed", "kind":"file"}]),
            json!([{"name":"item", "kind":"unknown"}]),
            json!([{"path":"a.rs", "line":0, "text":"invalid"}]),
            json!([{"path":"a.rs", "line":3, "text":"x", "extra":true}]),
        ] {
            assert!(summarize(&record(json!({"items":items,"truncated":false}))).is_none());
        }
    }

    #[test]
    fn paths_and_errors_are_single_line_terminal_safe_and_grapheme_bounded() {
        let path = format!(
            "/tmp/unsafe\u{1b}]2;injected\u{7}\n\u{202e}{}",
            "e\u{301}👩‍💻".repeat(100)
        );
        let summary = summarize(&record(
            json!({"path":path,"version":"version","created":true}),
        ))
        .unwrap();
        assert!(
            summary
                .text
                .starts_with("created /tmp/unsafe\\u{1b}]2;injected\\u{7}\\n\\u{202e}")
        );
        assert!(summary.text.ends_with('…'));
        assert!(summary.text.graphemes(true).count() <= 120);
        assert!(!summary.text.chars().any(char::is_control));
        assert!(!summary.text.contains('\u{202e}'));
        assert!(
            !summary.truncated,
            "cosmetic cropping does not change the recorded tool outcome"
        );
        assert_eq!(one_line("e\u{301}👩‍💻中文", 3), "e\u{301}👩‍💻…");
        let error = json!({"ok":false,"data":null,"error":{"code":"x".repeat(300),"message":format!("\u{1b}[2J\n{}", "文👩‍💻".repeat(300))},"truncated":true}).to_string();
        let summary = summarize(&error).unwrap();
        assert!(!summary.ok && summary.truncated);
        assert!(summary.text.contains("\\u{1b}[2J\\n"));
        assert!(summary.text.graphemes(true).count() <= 120);
        assert!(!summary.text.chars().any(char::is_control));
    }

    #[test]
    fn byte_limit_handles_giant_clusters_and_expanded_controls_without_splitting_unicode() {
        for cluster in [
            format!("a{}", "\u{301}".repeat(10_000)),
            format!("👩{}", "\u{200d}👩".repeat(1_000)),
        ] {
            assert_eq!(one_line(&cluster, 120), "…");
            let summary = summarize(&record(
                json!({"path":cluster,"version":"version","created":false}),
            ))
            .unwrap();
            assert_eq!(summary.text, "updated …");
            assert!(summary.text.len() <= MAX_SUMMARY_BYTES);
        }
        for source in [
            format!("{}b", "a\u{301}".repeat(200)),
            "\u{1b}\n\u{202e}".repeat(500),
        ] {
            let summary = one_line(&source, 10_000);
            assert!(summary.len() <= MAX_SUMMARY_BYTES);
            assert!(summary.ends_with('…'));
            assert!(!summary.chars().any(char::is_control));
            assert!(!summary.contains('\u{202e}'));
        }
        assert_eq!(
            one_line(&"x".repeat(MAX_SUMMARY_BYTES), 1_000).len(),
            MAX_SUMMARY_BYTES
        );
        assert_eq!(
            one_line(&"x".repeat(MAX_SUMMARY_BYTES + 1), 1_000).len(),
            MAX_SUMMARY_BYTES
        );
        assert!(one_line("text", 0).is_empty());
    }

    #[test]
    fn browser_summaries_show_the_action_source_revision_and_artifact_metadata() {
        let result = ToolResult {
            ok: true,
            data: Some(json!({
                "source": "untrusted_browser",
                "screenshot": {
                    "page": "page_demo",
                    "document_revision": 3,
                    "url": "https://a.test/",
                    "mime": "image/png",
                    "bytes": 42,
                    "sha256": "0123456789abcdef0123456789abcdef",
                    "artifact_ref": "media_demo",
                    "masked": true,
                    "model_image_capability": "disabled",
                    "model_image_status": "artifact_saved"
                }
            })),
            error: None,
            truncated: false,
            media_refs: Vec::new(),
        };
        let summary = summarize_call(
            "browser_screenshot",
            &json!({"page": "page_demo"}),
            Some(&result),
            None,
        )
        .unwrap();
        assert!(summary.ok && !summary.running);
        for expected in [
            "screenshot",
            "Page page_demo",
            "Revision 3",
            "https://a.test/",
            "image/png",
            "42 bytes",
            "SHA-256",
            "Masked yes",
            "Artifact media_demo",
        ] {
            assert!(
                summary.text.contains(expected),
                "{expected}: {}",
                summary.text
            );
        }
        assert!(!summary.text.contains("media_ref"));

        let running = summarize_call("browser_navigate", &json!({}), None, None).unwrap();
        assert!(running.running);
        assert!(running.text.starts_with("navigate · running"));

        let unknown = ToolResult {
            ok: false,
            data: None,
            error: Some(soyo_core::contract::ErrorInfo {
                code: "outcome_unknown".into(),
                message: "may have started".into(),
            }),
            truncated: false,
            media_refs: Vec::new(),
        };
        let summary = summarize_call("browser_click", &json!({}), Some(&unknown), None).unwrap();
        assert!(!summary.ok);
        assert!(summary.text.contains("outcome_unknown"));
    }

    #[test]
    fn browser_summary_shows_each_output_reference_once() {
        let result = ToolResult {
            ok: true,
            data: Some(
                json!({"source":"untrusted_browser","page":"page_demo","document_revision":3,"output_refs":[{"output_ref":"output_demo"}]}),
            ),
            error: None,
            truncated: true,
            media_refs: Vec::new(),
        };
        let summary = summarize_call(
            "browser_snapshot",
            &json!({"page":"page_demo"}),
            Some(&result),
            None,
        )
        .unwrap();
        assert!(summary.truncated);
        assert_eq!(summary.text.matches("Output output_demo").count(), 1);
    }

    #[test]
    fn noncanonical_envelopes_unknown_shapes_and_partial_json_fall_back() {
        for value in [
            json!({"content":"plain tool body"}),
            json!({"ok":true,"data":null,"error":null,"truncated":false}),
            json!({"ok":true,"data":{"unknown":"shape"},"error":null,"truncated":false}),
            json!({"ok":true,"data":{"todos":[]},"truncated":false}),
            json!({"ok":true,"error":null,"truncated":false}),
            json!({"ok":"true","data":{"todos":[]},"error":null,"truncated":false}),
            json!({"ok":true,"data":{"todos":[]},"error":null,"truncated":"false"}),
            json!({"ok":false,"data":null,"error":null,"truncated":false}),
            json!({"ok":true,"data":{"todos":[]},"error":{"code":"failed","message":"failed"},"truncated":false}),
            json!({"ok":false,"data":null,"error":{"code":"failed","message":false},"truncated":false}),
            json!({"ok":true,"data":{"todos":[]},"error":null,"truncated":false,"extra":true}),
            json!([]),
        ] {
            assert!(summarize(&value.to_string()).is_none(), "{value}");
        }
        for text in [
            "plain tool body",
            "{\"ok\":true,\"data\":",
            r#"{"ok":false,"ok":true,"data":{"todos":[]},"error":null,"truncated":false}"#,
            r#"{"ok":false,"data":null,"error":{"code":"one","code":"two","message":"failed"},"truncated":false}"#,
        ] {
            assert!(summarize(text).is_none());
        }
        assert!(summarize(&"x".repeat(MAX_MESSAGE_BYTES + 1)).is_none());
    }
}
