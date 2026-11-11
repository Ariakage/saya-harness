use saya_core::{
    contract::{HistoryItem, ModelProtocol, Session, SessionId, ToolCall, ToolResult, WorkMode},
    export::{ExportOptions, ExportScope, SessionExport},
    sqlite::SessionSnapshot,
};
use serde_json::json;
fn fixture() -> SessionSnapshot {
    let call = ToolCall {
        call_id: "same-call".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"tool-body-value"}"#.into(),
    };
    let result = ToolResult {
        ok: false,
        data: Some(json!({"text":"tool-body-value"})),
        error: Some(saya_core::contract::ErrorInfo {
            code: "unknown".into(),
            message: "tool-body-value".into(),
        }),
        truncated: true,
    };
    SessionSnapshot {
        created_at: "2026-11-08T00:00:00Z".into(),
        source_database_schema: 2,
        last_change_seq: 8,
        last_event_seq: 3,
        session: Session {
            id: SessionId::new(),
            workspace: "/workspace".into(),
            work_mode: WorkMode::Work,
            profile: "fixture".into(),
            protocol: ModelProtocol::Responses,
            model: "fake".into(),
            history: vec![
                HistoryItem::UserText("你好 🌸".into()),
                HistoryItem::ProviderItem {
                    protocol: ModelProtocol::Responses,
                    value: json!({"encrypted_reasoning":"opaque-secret"}),
                },
                HistoryItem::AssistantPartial("partial".into()),
                HistoryItem::ToolCall(call.clone()),
                HistoryItem::ToolResult {
                    call_id: call.call_id.clone(),
                    result: result.clone(),
                },
            ],
        },
        runs: vec![json!({"run_id":"run-1","run":{"id":"run-1"},"result":null,"finished_at":null})],
        tool_calls: vec![
            json!({"run_id":"run-1","call_id":call.call_id,"call":call,"state":"unknown","result":result}),
            json!({"run_id":"run-2","call_id":"same-call","call":{"call_id":"same-call","name":"read_file","arguments":"{}"},"state":"denied","result":null}),
        ],
        metrics: vec![json!({"run_id":"run-1","data":{"tool_calls":2}})],
        background_jobs: vec![
            json!({"job_id":"job-1","run_id":"run-1","parent_call_id":"parent","state":"unknown","result":{"ok":true,"data":{"text":"tool-body-value"},"truncated":false}}),
        ],
        events: vec![
            json!({"seq":3,"type":"tool.requested","data":{"call_id":"same-call","args":{"text":"tool-body-value"}}}),
            json!({"seq":4,"type":"tool.finished","data":{"result":{"ok":true,"data":{"text":"tool-body-value"},"truncated":false}}}),
        ],
        changes: vec![
            json!({"seq":1,"run_id":"run-1","kind":"activity","reference":json!({"type":"tool.requested","data":{"args":{"text":"tool-body-value"}}}).to_string()}),
        ],
        project_context: vec![],
        approval_requests: vec![
            json!({"request":{"arguments":{"text":"tool-body-value"},"operation_summary":"tool-body-value","tool":"read_file"}}),
        ],
    }
}
#[test]
fn transcript_omits_internal_state_and_tool_bodies_everywhere()
-> Result<(), Box<dyn std::error::Error>> {
    let snapshot = fixture();
    let export = SessionExport::from_snapshot(&snapshot, ExportOptions::default(), &[])?;
    let bytes = export.to_json()?;
    let text = String::from_utf8(bytes.clone())?;
    assert!(!text.contains("opaque-secret"));
    assert!(!text.contains("tool-body-value"));
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["export_type"], "saya.session");
    assert_eq!(value["snapshot"]["source_database_schema"], 2);
    assert_eq!(value["snapshot"]["last_change_seq"], 8);
    assert_eq!(value["history"][0]["text"], "你好 🌸");
    assert_eq!(value["history"][2]["ordinal"], 3);
    assert_eq!(value["history"][2]["run_id"], "run-1");
    assert_eq!(value["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(value["tool_calls"][0]["state"], "unknown");
    assert_eq!(value["tool_calls"][1]["run_id"], "run-2");
    assert!(value["events"].as_array().unwrap().is_empty());
    Ok(())
}
#[test]
fn full_selection_retains_safe_structure_and_optional_tool_content()
-> Result<(), Box<dyn std::error::Error>> {
    for include_tools in [false, true] {
        let export = SessionExport::from_snapshot(
            &fixture(),
            ExportOptions {
                scope: ExportScope::Full,
                include_tools,
                ..ExportOptions::default()
            },
            &[],
        )?;
        let text = String::from_utf8(export.to_json()?)?;
        assert_eq!(text.contains("tool-body-value"), include_tools);
        assert!(text.contains("opaque-secret"));
        assert!(export.history.iter().any(|item| {
            item["kind"] == "provider_item"
                && item["protocol"] == "responses"
                && item["value"]["encrypted_reasoning"] == "opaque-secret"
        }));
        assert_eq!(export.omitted_fields.is_empty(), include_tools);
        assert_eq!(export.events.len(), 2);
        assert_eq!(export.activities.len(), 1);
        assert_eq!(export.metrics.len(), 1);
        assert_eq!(export.background_jobs[0]["state"], "unknown");
    }
    Ok(())
}
#[test]
fn redaction_precedes_utf8_truncation_and_reports_bounded_coverage()
-> Result<(), Box<dyn std::error::Error>> {
    let mut snapshot = fixture();
    let secret = "fixture\"secret\\tail".to_owned();
    let mut nested = json!({"token":secret}).to_string();
    for _ in 0..10 {
        nested = serde_json::to_string(&nested)?;
    }
    snapshot
        .session
        .history
        .push(HistoryItem::AssistantText(nested));
    snapshot
        .session
        .history
        .push(HistoryItem::UserText("🌸".repeat(100)));
    let export = SessionExport::from_snapshot(
        &snapshot,
        ExportOptions {
            max_field_bytes: 41,
            ..ExportOptions::default()
        },
        std::slice::from_ref(&secret),
    )?;
    let text = String::from_utf8(export.to_json()?)?;
    assert!(!saya_core::redaction::contains_secret(
        &text,
        std::slice::from_ref(&secret)
    ));
    assert!(
        export
            .truncated_fields
            .iter()
            .any(|f| f.original_bytes == 400 && f.retained_bytes == 40)
    );
    assert_eq!(
        export.history.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .len(),
        40
    );
    Ok(())
}
#[test]
fn ambiguous_legacy_history_links_are_explicit_and_limits_fail()
-> Result<(), Box<dyn std::error::Error>> {
    let mut snapshot = fixture();
    snapshot.tool_calls[1]["call"] = snapshot.tool_calls[0]["call"].clone();
    let export = SessionExport::from_snapshot(&snapshot, ExportOptions::default(), &[])?;
    assert!(export.history[2]["run_id"].is_null());
    assert!(
        export
            .omitted_fields
            .iter()
            .any(|f| f == "unavailable_history_run_association")
    );
    assert!(
        SessionExport::from_snapshot(
            &snapshot,
            ExportOptions {
                max_output_bytes: 10,
                ..ExportOptions::default()
            },
            &[]
        )
        .is_err()
    );
    assert!(
        SessionExport::from_snapshot(
            &snapshot,
            ExportOptions {
                max_field_bytes: 0,
                ..ExportOptions::default()
            },
            &[]
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn redacted_failed_calls_link_only_when_their_compound_identity_is_unique()
-> Result<(), Box<dyn std::error::Error>> {
    let mut snapshot = fixture();
    snapshot.tool_calls.truncate(1);
    snapshot.tool_calls[0]["state"] = json!("failed");
    snapshot.tool_calls[0]["call"]["arguments"] = json!(r#"{"redacted":true}"#);
    let export = SessionExport::from_snapshot(&snapshot, ExportOptions::default(), &[])?;
    assert_eq!(export.history[2]["run_id"], "run-1");
    assert_eq!(export.history[3]["run_id"], "run-1");
    assert!(
        !export
            .omitted_fields
            .iter()
            .any(|field| field == "unavailable_history_run_association")
    );
    let mut duplicate = snapshot.tool_calls[0].clone();
    duplicate["run_id"] = json!("run-2");
    for other in [duplicate.clone(), {
        duplicate["call"]["arguments"] = json!("{}");
        duplicate
    }] {
        let mut ambiguous = snapshot.clone();
        ambiguous.tool_calls.push(other);
        let export = SessionExport::from_snapshot(&ambiguous, ExportOptions::default(), &[])?;
        assert!(export.history[2]["run_id"].is_null());
        assert!(export.history[3]["run_id"].is_null());
        assert!(
            export
                .omitted_fields
                .iter()
                .any(|field| field == "unavailable_history_run_association")
        );
    }
    for (field, value) in [
        ("state", json!("unknown")),
        ("run_id", serde_json::Value::Null),
        ("run_id", json!("")),
        ("call_id", json!("different-call")),
        ("result", json!({"ok":true})),
        (
            "call",
            json!({"call_id":"same-call","name":"read_file","arguments":"{}"}),
        ),
        (
            "call",
            json!({"call_id":"same-call","name":"read_file","arguments":r#"{"redacted":true,"other":1}"#}),
        ),
    ] {
        let mut unrelated = snapshot.clone();
        unrelated.tool_calls[0][field] = value;
        let export = SessionExport::from_snapshot(&unrelated, ExportOptions::default(), &[])?;
        assert!(
            export.history[2]["run_id"].is_null(),
            "matched malformed {field}"
        );
    }
    Ok(())
}

#[test]
fn all_correlation_identity_fields_refuse_truncation() -> Result<(), Box<dyn std::error::Error>> {
    for field in ["session_id", "approval_id", "request_id", "input_id"] {
        let mut snapshot = fixture();
        snapshot.events[0][field] = json!("x".repeat(129));
        let error = SessionExport::from_snapshot(
            &snapshot,
            ExportOptions {
                scope: ExportScope::Full,
                max_field_bytes: 128,
                ..ExportOptions::default()
            },
            &[],
        )
        .unwrap_err();
        assert_eq!(error.0.code, "export_limit", "truncated {field}");
    }
    Ok(())
}

#[test]
fn redacted_fallback_does_not_guess_between_repeated_legacy_history_identities()
-> Result<(), Box<dyn std::error::Error>> {
    for redacted in [false, true] {
        let mut snapshot = fixture();
        snapshot.tool_calls.truncate(1);
        snapshot.tool_calls[0]["state"] = json!("failed");
        if redacted {
            snapshot.tool_calls[0]["call"]["arguments"] = json!(r#"{"redacted":true}"#);
        }
        snapshot
            .session
            .history
            .push(HistoryItem::UserText("next run".into()));
        snapshot
            .session
            .history
            .push(HistoryItem::ToolCall(ToolCall {
                call_id: "same-call".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"different-file"}"#.into(),
            }));
        snapshot.session.history.push(HistoryItem::ToolResult {
            call_id: "same-call".into(),
            result: ToolResult {
                ok: false,
                data: None,
                error: Some(saya_core::contract::ErrorInfo {
                    code: "not_executed".into(),
                    message: "run ended before dispatch".into(),
                }),
                truncated: false,
            },
        });
        let export = SessionExport::from_snapshot(&snapshot, ExportOptions::default(), &[])?;
        let first_run = if redacted {
            serde_json::Value::Null
        } else {
            json!("run-1")
        };
        assert_eq!(export.history[2]["run_id"], first_run);
        assert_eq!(export.history[3]["run_id"], first_run);
        assert!(export.history[5]["run_id"].is_null());
        assert!(export.history[6]["run_id"].is_null());
        assert!(
            export
                .omitted_fields
                .iter()
                .any(|field| field == "unavailable_history_run_association")
        );
    }
    Ok(())
}

#[test]
fn markdown_fences_html_controls_and_long_unicode_use_the_same_projection()
-> Result<(), Box<dyn std::error::Error>> {
    let mut snapshot = fixture();
    let text = "你好 🌸\n``````\n<script>bad & worse</script>\u{1b}[31m\u{202e}\r";
    snapshot
        .session
        .history
        .push(HistoryItem::AssistantText(text.into()));
    let export = SessionExport::from_snapshot(
        &snapshot,
        ExportOptions {
            scope: ExportScope::Full,
            ..ExportOptions::default()
        },
        &[],
    )?;
    let markdown = String::from_utf8(export.to_markdown()?)?;
    assert!(markdown.contains("你好 🌸"));
    assert!(markdown.contains("```````text\n"));
    assert!(markdown.contains("&lt;script&gt;bad &amp; worse&lt;/script&gt;"));
    assert!(!markdown.contains('<'));
    assert!(!markdown.contains('\u{1b}'));
    assert!(!markdown.contains('\u{202e}'));
    assert!(!markdown.contains('\r'));
    assert!(markdown.contains("\\u{1b}[31m"));
    assert!(!markdown.contains("tool-body-value"));
    assert!(markdown.contains("opaque-secret"));
    assert!(markdown.contains("\"kind\": \"provider_item\""));
    assert!(markdown.contains("run-1"));
    assert!(markdown.contains("run-2"));
    assert!(markdown.contains("unknown"));
    assert_eq!(export.history.last().unwrap()["text"], text);
    Ok(())
}
#[test]
fn markdown_and_json_share_truncation_and_output_limit() -> Result<(), Box<dyn std::error::Error>> {
    let mut snapshot = fixture();
    snapshot
        .session
        .history
        .push(HistoryItem::UserText("🌸".repeat(100)));
    let export = SessionExport::from_snapshot(
        &snapshot,
        ExportOptions {
            max_field_bytes: 41,
            ..ExportOptions::default()
        },
        &[],
    )?;
    let markdown = String::from_utf8(export.to_markdown()?)?;
    let retained = "🌸".repeat(10);
    assert!(markdown.contains(&retained));
    assert!(!markdown.contains(&"🌸".repeat(11)));
    assert!(markdown.contains("Truncated fields"));
    assert!(markdown.contains("original_bytes"));
    // JSON and Markdown have independent encoded sizes under the same limit.
    let mut long = fixture();
    long.session.history = vec![HistoryItem::UserText("<&>".repeat(3000))];
    let export = SessionExport::from_snapshot(
        &long,
        ExportOptions {
            max_output_bytes: 20_000,
            ..ExportOptions::default()
        },
        &[],
    )?;
    assert!(export.to_json()?.len() < 20_000);
    assert_eq!(export.to_markdown().unwrap_err().0.code, "export_limit");
    Ok(())
}
