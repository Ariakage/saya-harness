use saya_core::contract::*;
use serde_json::{json, to_value};

fn result() -> RunResult {
    RunResult {
        schema_version: SCHEMA_VERSION,
        session_id: None,
        run_id: None,
        status: RunStatus::NeedsConfirmation,
        stop_reason: Some("approval_required".into()),
        text: Some("需要确认命令执行".into()),
        error: None,
        usage: Some(Usage {
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
            estimated: true,
        }),
        artifacts: vec![],
        persistence: Some(Persistence {
            saved: true,
            error: None,
        }),
    }
}

#[test]
fn result_matches_baseline_shape_including_nulls() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        to_value(result())?,
        json!({
            "schema_version": 1, "session_id": null, "run_id": null,
            "status": "needs_confirmation", "stop_reason": "approval_required",
            "text": "需要确认命令执行", "error": null,
            "usage": {"input_tokens": null, "output_tokens": null, "estimated": true},
            "artifacts": [], "persistence": {"saved": true, "error": null}
        })
    );
    Ok(())
}

#[test]
fn final_event_contains_the_same_result_and_required_envelope()
-> Result<(), Box<dyn std::error::Error>> {
    let session = SessionId::new();
    let run = RunId::new();
    let final_result = RunResult {
        session_id: Some(session),
        run_id: Some(run),
        ..result()
    };
    let event = Event {
        schema_version: SCHEMA_VERSION,
        session_id: session,
        run_id: Some(run),
        seq: 7,
        timestamp: "2026-11-06T00:00:00Z".into(),
        payload: EventPayload::RunFinished(final_result.clone()),
    };
    let value = to_value(&event)?;
    assert_eq!(
        value,
        json!({"schema_version":1, "session_id":session, "run_id":run,
        "seq":7, "timestamp":"2026-11-06T00:00:00Z", "type":"run.finished", "data":final_result})
    );
    assert_eq!(serde_json::from_value::<Event>(value)?, event);
    Ok(())
}

#[test]
fn model_history_retains_order_call_identity_and_opaque_items()
-> Result<(), Box<dyn std::error::Error>> {
    let history = vec![
        HistoryItem::ProviderItem {
            protocol: ModelProtocol::Responses,
            value: json!({"encrypted_content":"opaque"}),
        },
        HistoryItem::ToolCall(ToolCall {
            call_id: "provider-call-1".into(),
            name: "read_file".into(),
            arguments: "{invalid".into(),
        }),
        HistoryItem::ToolResult {
            call_id: "provider-call-1".into(),
            result: ToolResult {
                ok: false,
                data: None,
                error: Some(ErrorInfo {
                    code: "invalid_args".into(),
                    message: "invalid JSON".into(),
                }),
                truncated: false,
            },
        },
    ];
    let encoded = serde_json::to_vec(&history)?;
    assert_eq!(
        serde_json::from_slice::<Vec<HistoryItem>>(&encoded)?,
        history
    );
    Ok(())
}

#[test]
fn generated_ids_and_mode_defaults_match_contract() -> Result<(), Box<dyn std::error::Error>> {
    let session = SessionId::new();
    let run = RunId::new();
    assert_eq!(session.0.get_version_num(), 7);
    assert_eq!(run.0.get_version_num(), 7);
    assert_eq!(to_value(WorkMode::default())?, "work");
    assert_eq!(to_value(PermissionMode::default())?, "auto");
    assert_eq!(to_value(WorkMode::Plan)?, "plan");
    Ok(())
}

#[test]
fn tool_event_preserves_call_id_and_structured_failure() -> Result<(), Box<dyn std::error::Error>> {
    let payload = EventPayload::ToolFinished {
        call_id: "call-42".into(),
        name: "write_file".into(),
        result: ToolResult {
            ok: false,
            data: None,
            error: Some(ErrorInfo {
                code: "denied".into(),
                message: "not authorized".into(),
            }),
            truncated: false,
        },
    };
    assert_eq!(
        to_value(payload)?,
        json!({"type":"tool.finished", "data":{
            "call_id":"call-42", "name":"write_file", "result":{"ok":false,"data":null,
            "error":{"code":"denied","message":"not authorized"},"truncated":false}
        }})
    );
    Ok(())
}

#[test]
fn unknown_tool_state_is_preserved_for_recovery() -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string(&ToolState::Unknown)?;
    assert_eq!(json, "\"unknown\"");
    assert_eq!(
        serde_json::from_str::<ToolState>(&json)?,
        ToolState::Unknown
    );
    Ok(())
}

#[test]
fn manual_permission_uses_new_name_and_reads_legacy_review()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::contract::PermissionMode;
    assert_eq!(
        serde_json::to_string(&PermissionMode::Manual)?,
        "\"manual\""
    );
    assert_eq!(
        serde_json::from_str::<PermissionMode>("\"review\"")?,
        PermissionMode::Manual
    );
    Ok(())
}
