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
        assert!(!text.contains("opaque-secret"));
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
