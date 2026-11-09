use super::*;
use crate::{
    context_builder::ItemRange,
    contract::{ModelProtocol, WorkMode},
    credentials::SecretRegistry,
    output_store::{
        CaptureCompletion, OutputCoverage, OutputLimits, OutputScope, OutputStore, OutputStream,
        ReadOptions,
    },
};
use std::sync::Arc;

const FORGOTTEN: &str = "retired unique project observation";

fn producer(id: &str, name: &str) -> ToolCall {
    ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments: "{}".into(),
    }
}

fn observed() -> ToolResult {
    ToolResult {
        ok: false,
        data: Some(json!({
            "stdout": FORGOTTEN,
            "stderr": FORGOTTEN,
            "nested": {"text": FORGOTTEN},
            "exit_code": 7,
            "timed_out": false,
            "cancelled": true,
        })),
        error: Some(ErrorInfo {
            code: FORGOTTEN.into(),
            message: FORGOTTEN.into(),
        }),
        truncated: true,
    }
}

fn result(id: &str) -> HistoryItem {
    HistoryItem::ToolResult {
        call_id: id.into(),
        result: observed(),
    }
}

fn session(protocol: ModelProtocol) -> Session {
    Session {
        id: SessionId::new(),
        workspace: "/trusted-workspace".into(),
        work_mode: WorkMode::Plan,
        profile: "fixture".into(),
        protocol,
        model: "fixture".into(),
        history: vec![
            HistoryItem::UserText(FORGOTTEN.into()),
            HistoryItem::AssistantText(FORGOTTEN.into()),
            HistoryItem::ToolCall(producer("retired-call", "exec")),
            result("retired-call"),
            HistoryItem::UserText("Continue with independent work".into()),
            HistoryItem::ToolCall(producer("live-call", "read_file")),
            result("live-call"),
            HistoryItem::UserText("Check current status".into()),
        ],
    }
}

fn retirement(session: &Session, producer_id: &str) -> GroupRetirement {
    let group = group_history(session.protocol, &session.history)
        .unwrap()
        .groups
        .into_iter()
        .find(|group| group.call_ids.iter().any(|id| id == producer_id))
        .unwrap();
    GroupRetirement {
        range: group.range,
        source_sha256: history_digest(&session.history[group.range.start..group.range.end])
            .unwrap(),
        current_memory_revision: "c".repeat(64),
        origins: vec![RetirementOrigin::InjectedMemoryTurn {
            request_id_sha256: "a".repeat(64),
            memory_revision: "b".repeat(64),
        }],
        observations: vec![],
    }
}

fn metadata(session: &Session, call_id: &str) -> OutputMetadata {
    OutputMetadata {
        output_ref: "out_fixture".into(),
        scope: OutputScope {
            session_id: session.id,
            run_id: RunId::new(),
            call_id: call_id.into(),
            stream: OutputStream::Stdout,
        },
        capture_limit_bytes: 4096,
        total_bytes_seen: 123,
        retained_bytes: 100,
        retained_source_bytes: 100,
        retained_ranges: vec![],
        gaps: vec![],
        artifact_complete: false,
        completion: CaptureCompletion::Cancelled,
        coverage: OutputCoverage::RetainedRangesOnly,
        withheld_reason: None,
        preview_head: FORGOTTEN.into(),
        preview_tail: FORGOTTEN.into(),
        sha256: "d".repeat(64),
    }
}

fn retrieval(name: &str) -> ToolCall {
    ToolCall {
        call_id: "current-query".into(),
        name: name.into(),
        arguments: json!({
            "output_ref": "out_fixture",
            "pattern": FORGOTTEN,
        })
        .to_string(),
    }
}

fn status_call() -> ToolCall {
    ToolCall {
        call_id: "current-status".into(),
        name: "background_status".into(),
        arguments: json!({"job_id":"host-job"}).to_string(),
    }
}

fn code<T>(result: Result<T, BoundaryError>) -> String {
    match result {
        Ok(_) => panic!("provenance check unexpectedly succeeded"),
        Err(error) => error.0.code,
    }
}

#[test]
fn read_and_search_withhold_retired_previews_and_preserve_only_mechanical_metadata() {
    let session = session(ModelProtocol::ChatCompletions);
    let before = session.clone();
    let acl = RetiredOutputAcl::build(&session, &[retirement(&session, "retired-call")]).unwrap();
    for tool in ["output_read", "output_search"] {
        let projected = acl
            .output_result(
                &session,
                &retrieval(tool),
                &[metadata(&session, "retired-call")],
            )
            .unwrap()
            .unwrap();
        assert!(!projected.ok);
        assert!(projected.truncated);
        assert_eq!(
            projected.error.as_ref().unwrap().code,
            "memory_source_retired"
        );
        let data = projected.data.as_ref().unwrap();
        assert_eq!(data["body_withheld"], true);
        assert_eq!(data["total_bytes_seen"], 123);
        assert_eq!(data["completion"], "cancelled");
        assert!(
            !serde_json::to_string(&projected)
                .unwrap()
                .contains(FORGOTTEN)
        );
        assert!(data.get("preview_head").is_none());
        assert!(data.get("preview_tail").is_none());
        assert!(data.get("call_id").is_none());
    }
    assert_eq!(session, before);
    assert!(matches!(&session.history[0], HistoryItem::UserText(text) if text == FORGOTTEN));
}

#[test]
fn real_published_output_remains_in_host_store_while_model_body_is_withheld() {
    let directory = tempfile::tempdir().unwrap();
    let store = OutputStore::open(
        directory.path().join("outputs"),
        OutputLimits::default(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    let session = session(ModelProtocol::ChatCompletions);
    let mut capture = store
        .begin(metadata(&session, "retired-call").scope, 4096)
        .unwrap();
    capture.push(FORGOTTEN.as_bytes()).unwrap();
    let sealed = capture.seal(CaptureCompletion::Eof).unwrap();
    store
        .publish(session.id, &sealed.metadata.output_ref)
        .unwrap();
    let raw_audit = store
        .read(
            session.id,
            &sealed.metadata.output_ref,
            ReadOptions::default(),
        )
        .unwrap();
    assert_eq!(raw_audit.chunks[0].text, FORGOTTEN);
    let acl = RetiredOutputAcl::build(&session, &[retirement(&session, "retired-call")]).unwrap();
    let mut call = retrieval("output_read");
    call.arguments = json!({"output_ref":sealed.metadata.output_ref}).to_string();
    let projected = acl
        .output_result(&session, &call, &[sealed.metadata])
        .unwrap()
        .unwrap();
    assert!(
        !serde_json::to_string(&projected)
            .unwrap()
            .contains(FORGOTTEN)
    );
    assert_eq!(store.inventory().unwrap().len(), 1);
}

#[test]
fn only_registered_current_session_known_sources_can_bypass_the_retirement_projection() {
    let session = session(ModelProtocol::ChatCompletions);
    let acl = RetiredOutputAcl::build(&session, &[retirement(&session, "retired-call")]).unwrap();
    let call = retrieval("output_search");
    assert!(
        acl.output_result(&session, &call, &[metadata(&session, "live-call")])
            .unwrap()
            .is_none()
    );
    for registered in [vec![], vec![metadata(&session, "unknown-producer")]] {
        let result = acl
            .output_result(&session, &call, &registered)
            .unwrap()
            .unwrap();
        assert_eq!(result.error.unwrap().code, "output_provenance_unavailable");
        assert!(result.data.is_none());
    }
    let mut foreign = metadata(&session, "live-call");
    foreign.scope.session_id = SessionId::new();
    assert_eq!(
        code(acl.output_result(&session, &call, &[foreign])),
        "output_provenance"
    );
    let first = metadata(&session, "live-call");
    let mut conflicting = first.clone();
    conflicting.scope.run_id = RunId::new();
    assert_eq!(
        code(acl.output_result(&session, &call, &[first, conflicting])),
        "output_provenance"
    );
}

#[test]
fn source_and_session_binding_are_rechecked_even_after_append_only_history_changes() {
    let session = session(ModelProtocol::ChatCompletions);
    let retired = retirement(&session, "retired-call");
    let acl = RetiredOutputAcl::build(&session, std::slice::from_ref(&retired)).unwrap();
    let mut stale = session.clone();
    stale
        .history
        .push(HistoryItem::ToolCall(retrieval("output_read")));
    assert_eq!(
        code(acl.output_result(&stale, &retrieval("output_read"), &[])),
        "stale_approval"
    );
    let rebuilt = RetiredOutputAcl::build(&stale, std::slice::from_ref(&retired)).unwrap();
    assert!(
        rebuilt
            .output_result(
                &stale,
                &retrieval("output_read"),
                &[metadata(&session, "retired-call")]
            )
            .unwrap()
            .is_some()
    );
    let mut foreign = session.clone();
    foreign.id = SessionId::new();
    assert_eq!(
        code(acl.output_result(&foreign, &retrieval("output_read"), &[])),
        "stale_approval"
    );
    let mut modified = session.clone();
    modified.history[1] = HistoryItem::AssistantText("Different source bytes".into());
    assert_eq!(
        code(RetiredOutputAcl::build(&modified, &[retired])),
        "output_provenance"
    );
}

#[test]
fn retired_groups_require_complete_assistant_source_ranges_and_current_revision_agreement() {
    let session = session(ModelProtocol::ChatCompletions);
    let original = retirement(&session, "retired-call");
    let mut user = original.clone();
    user.range = ItemRange { start: 0, end: 1 };
    user.source_sha256 = history_digest(&session.history[..1]).unwrap();
    assert_eq!(
        code(RetiredOutputAcl::build(&session, &[user])),
        "output_provenance"
    );
    let mut partial = original.clone();
    partial.range.end -= 1;
    partial.source_sha256 =
        history_digest(&session.history[partial.range.start..partial.range.end]).unwrap();
    assert_eq!(
        code(RetiredOutputAcl::build(&session, &[partial])),
        "output_provenance"
    );
    let mut empty = original.clone();
    empty.origins.clear();
    assert_eq!(
        code(RetiredOutputAcl::build(&session, &[empty])),
        "output_provenance"
    );
    assert_eq!(
        code(RetiredOutputAcl::build(
            &session,
            &[original.clone(), original.clone()]
        )),
        "output_provenance"
    );
    let mut different_epoch = retirement(&session, "live-call");
    different_epoch.current_memory_revision = "e".repeat(64);
    assert_eq!(
        code(RetiredOutputAcl::build(
            &session,
            &[original, different_epoch]
        )),
        "output_provenance"
    );
}

#[test]
fn native_memory_origins_require_matching_bound_native_call_and_result() {
    let mut session = session(ModelProtocol::ChatCompletions);
    session.history = vec![
        HistoryItem::UserText("Request".into()),
        HistoryItem::ToolCall(producer("memory-source", "memory_read")),
        HistoryItem::ToolCall(producer("retired-call", "exec")),
        result("memory-source"),
        result("retired-call"),
        HistoryItem::UserText("Continue".into()),
    ];
    let mut retired = retirement(&session, "retired-call");
    retired.origins = vec![RetirementOrigin::MemoryToolCall {
        call_index: 1,
        call_sha256: item_digest(&session.history[1]).unwrap(),
        result_index: 3,
        result_sha256: item_digest(&session.history[3]).unwrap(),
        memory_revision: "b".repeat(64),
    }];
    assert!(RetiredOutputAcl::build(&session, std::slice::from_ref(&retired)).is_ok());
    let mut wrong_pair = retired.clone();
    if let RetirementOrigin::MemoryToolCall {
        result_index,
        result_sha256,
        ..
    } = &mut wrong_pair.origins[0]
    {
        *result_index = 4;
        *result_sha256 = item_digest(&session.history[4]).unwrap();
    }
    assert_eq!(
        code(RetiredOutputAcl::build(&session, &[wrong_pair])),
        "output_provenance"
    );
    let mut wrong_hash = retired;
    if let RetirementOrigin::MemoryToolCall { call_sha256, .. } = &mut wrong_hash.origins[0] {
        *call_sha256 = "0".repeat(64);
    }
    assert_eq!(
        code(RetiredOutputAcl::build(&session, &[wrong_hash])),
        "output_provenance"
    );
}

#[test]
fn provider_owned_signatures_and_semantic_mirrors_keep_the_same_retired_producer_identity() {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let mut session = session(protocol);
        let call = producer("retired-call", "exec");
        let opaque = match protocol {
            ModelProtocol::ChatCompletions => json!({"reasoning_content":FORGOTTEN}),
            ModelProtocol::Responses => {
                json!({"type":"reasoning","id":"reasoning-fixture","encrypted_content":FORGOTTEN})
            }
            ModelProtocol::AnthropicMessages => {
                json!({"type":"thinking","thinking":FORGOTTEN,"signature":"retained-signature"})
            }
        };
        session.history = vec![
            HistoryItem::UserText("Request".into()),
            HistoryItem::ProviderItem {
                protocol,
                value: opaque,
            },
        ];
        match protocol {
            ModelProtocol::ChatCompletions => {},
            ModelProtocol::Responses => session.history.push(HistoryItem::ProviderItem {
                protocol,
                value:json!({"type":"function_call","call_id":call.call_id,"name":call.name,"arguments":call.arguments,"status":"completed"}),
            }),
            ModelProtocol::AnthropicMessages => session.history.push(HistoryItem::ProviderItem {
                protocol,
                value:json!({"type":"tool_use","id":call.call_id,"name":call.name,"input":{}}),
            }),
        }
        session.history.extend([
            HistoryItem::ToolCall(call),
            result("retired-call"),
            HistoryItem::UserText("Current input".into()),
        ]);
        let original = session.history.clone();
        let acl =
            RetiredOutputAcl::build(&session, &[retirement(&session, "retired-call")]).unwrap();
        assert!(
            acl.output_result(
                &session,
                &retrieval("output_read"),
                &[metadata(&session, "retired-call")]
            )
            .unwrap()
            .is_some()
        );
        assert_eq!(session.history, original);
    }
}

#[test]
fn retired_background_children_and_reused_call_ids_remain_conservatively_retired() {
    let mut session = session(ModelProtocol::ChatCompletions);
    if let HistoryItem::ToolCall(call) = &mut session.history[5] {
        call.call_id = "retired-call".into();
    }
    if let HistoryItem::ToolResult { call_id, .. } = &mut session.history[6] {
        *call_id = "retired-call".into();
    }
    let acl = RetiredOutputAcl::build(&session, &[retirement(&session, "retired-call")]).unwrap();
    for call_id in [
        "retired-call".into(),
        format!("{}{}", child_prefix("retired-call"), uuid::Uuid::now_v7()),
    ] {
        let projected = acl
            .output_result(
                &session,
                &retrieval("output_read"),
                &[metadata(&session, &call_id)],
            )
            .unwrap()
            .unwrap();
        assert_eq!(projected.error.unwrap().code, "memory_source_retired");
    }
}

#[test]
fn background_status_withholds_fact_and_error_bodies_while_retaining_typed_outcomes() {
    let session = session(ModelProtocol::ChatCompletions);
    let acl = RetiredOutputAcl::build(&session, &[retirement(&session, "retired-call")]).unwrap();
    let run_id = RunId::new();
    let origin = BackgroundJobOrigin {
        session_id: session.id,
        run_id,
        call_id: format!("{}{}", child_prefix("retired-call"), uuid::Uuid::now_v7()),
    };
    for status in [
        JobStatus::Running,
        JobStatus::Finished(observed()),
        JobStatus::Failed(BoundaryError(
            ErrorInfo {
                code: FORGOTTEN.into(),
                message: FORGOTTEN.into(),
            },
            None,
            Some(Box::new(observed())),
        )),
    ] {
        let projected = acl
            .background_result(
                &session,
                run_id,
                &status_call(),
                Some(&origin),
                Some(&status),
            )
            .unwrap()
            .unwrap();
        assert!(
            !serde_json::to_string(&projected)
                .unwrap()
                .contains(FORGOTTEN)
        );
        let data = projected.data.unwrap();
        assert_eq!(data["body_withheld"], true);
        assert_eq!(data["reason"], "memory_context_changed");
        if matches!(status, JobStatus::Running) {
            assert_eq!(data["status"], "running");
            assert!(data["mechanical_outcome"].is_null());
        } else {
            assert_eq!(data["mechanical_outcome"]["exit_code"], 7);
            assert_eq!(data["mechanical_outcome"]["cancelled"], true);
            assert_eq!(data["mechanical_outcome"]["timed_out"], false);
            assert_eq!(data["mechanical_outcome"]["error_present"], true);
        }
    }
}

#[test]
fn background_origin_uses_host_run_session_and_actual_producer_instead_of_query_fields() {
    let session = session(ModelProtocol::ChatCompletions);
    let acl = RetiredOutputAcl::build(&session, &[retirement(&session, "retired-call")]).unwrap();
    let run_id = RunId::new();
    let mut origin = BackgroundJobOrigin {
        session_id: session.id,
        run_id,
        call_id: "live-call".into(),
    };
    let status = JobStatus::Finished(observed());
    assert!(
        acl.background_result(
            &session,
            run_id,
            &status_call(),
            Some(&origin),
            Some(&status)
        )
        .unwrap()
        .is_none()
    );
    origin.call_id = "unknown-producer".into();
    assert_eq!(
        acl.background_result(
            &session,
            run_id,
            &status_call(),
            Some(&origin),
            Some(&status)
        )
        .unwrap()
        .unwrap()
        .data
        .unwrap()["reason"],
        "unverified_job_origin"
    );
    let unverified = acl
        .background_result(&session, run_id, &status_call(), None, Some(&status))
        .unwrap()
        .unwrap();
    assert!(
        !serde_json::to_string(&unverified)
            .unwrap()
            .contains(FORGOTTEN)
    );
    assert!(
        acl.background_result(&session, run_id, &status_call(), None, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        code(acl.background_result(&session, run_id, &status_call(), Some(&origin), None)),
        "output_provenance"
    );
    origin.run_id = RunId::new();
    assert_eq!(
        code(acl.background_result(
            &session,
            run_id,
            &status_call(),
            Some(&origin),
            Some(&status)
        )),
        "output_provenance"
    );
    origin.run_id = run_id;
    origin.session_id = SessionId::new();
    assert_eq!(
        code(acl.background_result(
            &session,
            run_id,
            &status_call(),
            Some(&origin),
            Some(&status)
        )),
        "output_provenance"
    );
}

#[test]
fn empty_retirement_compatibility_does_not_disable_other_guards_and_text_only_retirements_still_require_known_provenance()
 {
    let mut session = session(ModelProtocol::ChatCompletions);
    let empty = RetiredOutputAcl::build(&session, &[]).unwrap();
    assert!(!empty.is_active());
    assert!(
        empty
            .output_result(&session, &retrieval("output_read"), &[])
            .unwrap()
            .is_none()
    );
    session.history = vec![
        HistoryItem::UserText("Request".into()),
        HistoryItem::AssistantText(FORGOTTEN.into()),
    ];
    let retired = GroupRetirement {
        range: ItemRange { start: 1, end: 2 },
        source_sha256: history_digest(&session.history[1..2]).unwrap(),
        current_memory_revision: "c".repeat(64),
        origins: vec![RetirementOrigin::InjectedMemoryTurn {
            request_id_sha256: "a".repeat(64),
            memory_revision: "b".repeat(64),
        }],
        observations: vec![],
    };
    let acl = RetiredOutputAcl::build(&session, &[retired]).unwrap();
    assert!(acl.is_active());
    assert_eq!(
        acl.output_result(&session, &retrieval("output_read"), &[])
            .unwrap()
            .unwrap()
            .error
            .unwrap()
            .code,
        "output_provenance_unavailable"
    );
    let mut cancel = status_call();
    cancel.name = "background_cancel".into();
    assert!(
        acl.background_result(&session, RunId::new(), &cancel, None, None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn selectors_and_unfinished_or_malformed_provider_groups_fail_closed() {
    let session = session(ModelProtocol::ChatCompletions);
    let retired = retirement(&session, "retired-call");
    let acl = RetiredOutputAcl::build(&session, std::slice::from_ref(&retired)).unwrap();
    for arguments in [
        "{}".to_string(),
        "not json".to_string(),
        json!({"output_ref":"../escaped"}).to_string(),
        json!({"output_ref":"x".repeat(161)}).to_string(),
    ] {
        let mut call = retrieval("output_read");
        call.arguments = arguments;
        assert!(acl.output_result(&session, &call, &[]).is_err());
    }
    let mut split = session.clone();
    split
        .history
        .insert(3, HistoryItem::UserText("Invalid split".into()));
    assert_eq!(
        code(RetiredOutputAcl::build(&split, &[retired])),
        "invalid_history"
    );
}
