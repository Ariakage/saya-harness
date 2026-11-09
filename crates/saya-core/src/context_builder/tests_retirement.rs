use super::*;
use crate::{
    boundary::ModelRequest,
    contract::{ErrorInfo, ModelPurpose, ToolCall, ToolState},
};

pub(crate) fn fixture(
    protocol: ModelProtocol,
) -> (ModelRequest, CountingIdentity, Vec<GroupRetirement>) {
    let opaque = match protocol {
        ModelProtocol::ChatCompletions => json!({"reasoning_content":"FORGOTTEN_VALUE"}),
        ModelProtocol::Responses => {
            json!({"type":"reasoning","encrypted_content":"FORGOTTEN_VALUE"})
        }
        ModelProtocol::AnthropicMessages => {
            json!({"type":"thinking","thinking":"FORGOTTEN_VALUE","signature":"FORGOTTEN_VALUE"})
        }
    };
    let history = vec![
        HistoryItem::UserText("Continue the complete current task.".into()),
        HistoryItem::ProviderItem {
            protocol,
            value: opaque,
        },
        HistoryItem::ToolCall(ToolCall {
            call_id: "memory".into(),
            name: "memory_update".into(),
            arguments: json!({"text":"FORGOTTEN_VALUE"}).to_string(),
        }),
        HistoryItem::ToolCall(ToolCall {
            call_id: "command".into(),
            name: "exec".into(),
            arguments: json!({"argv":["printf","FORGOTTEN_VALUE"]}).to_string(),
        }),
        HistoryItem::ToolResult {
            call_id: "memory".into(),
            result: ToolResult {
                ok: true,
                data: Some(json!({"text":"FORGOTTEN_VALUE"})),
                error: None,
                truncated: false,
            },
        },
        HistoryItem::ToolResult {
            call_id: "command".into(),
            result: ToolResult {
                ok: false,
                data: Some(
                    json!({"exit_code":7,"timed_out":false,"cancelled":false,"stdout":"FORGOTTEN_VALUE"}),
                ),
                error: Some(ErrorInfo {
                    code: "command_failed".into(),
                    message: "FORGOTTEN_VALUE".into(),
                }),
                truncated: true,
            },
        },
        HistoryItem::AssistantText(
            "FORGOTTEN_VALUE was learned from an earlier injected memory.".into(),
        ),
        HistoryItem::UserText("Continue after forgetting.".into()),
        HistoryItem::ToolCall(ToolCall {
            call_id: "fresh".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }),
        HistoryItem::ToolResult {
            call_id: "fresh".into(),
            result: ToolResult {
                ok: true,
                data: Some(json!({"text":"fresh observation"})),
                error: None,
                truncated: false,
            },
        },
    ];
    let first = GroupRetirement {
        range: ItemRange { start: 1, end: 6 },
        source_sha256: history_digest(&history[1..6]).unwrap(),
        current_memory_revision: "b".repeat(64),
        origins: vec![RetirementOrigin::MemoryToolCall {
            call_index: 2,
            call_sha256: item_digest(&history[2]).unwrap(),
            result_index: 4,
            result_sha256: item_digest(&history[4]).unwrap(),
            memory_revision: "a".repeat(64),
        }],
        observations: vec![ObservationBinding {
            call_index: 3,
            call_sha256: item_digest(&history[3]).unwrap(),
            result_index: 5,
            result_sha256: item_digest(&history[5]).unwrap(),
            registered_tool_name: "exec".into(),
            state: ToolState::Failed,
        }],
    };
    let second = GroupRetirement {
        range: ItemRange { start: 6, end: 7 },
        source_sha256: history_digest(&history[6..7]).unwrap(),
        current_memory_revision: "b".repeat(64),
        origins: vec![RetirementOrigin::InjectedMemoryTurn {
            request_id_sha256: "c".repeat(64),
            memory_revision: "a".repeat(64),
        }],
        observations: Vec::new(),
    };
    (
        ModelRequest {
            request_id: "retirement-fixture".into(),
            purpose: ModelPurpose::Execution,
            profile: "fixture".into(),
            instructions: "Current trusted contract".into(),
            dynamic_instructions: None,
            max_output_tokens: Some(100),
            history,
            tools: Vec::new(),
        },
        CountingIdentity {
            protocol,
            model: "fixture-model".into(),
            adapter_version: "fixture-v1".into(),
        },
        vec![first, second],
    )
}

fn input<'a>(
    request: &'a ModelRequest,
    identity: &'a CountingIdentity,
    retirements: &'a [GroupRetirement],
) -> BuildInput<'a> {
    BuildInput {
        request,
        identity,
        budget: ContextBudget::new(100_000, 100, 100),
        source_history: &request.history,
        history_offset: 0,
        revision: 10,
        committed_view: None,
        protected_ranges: &[],
        output_projections: &[],
        group_retirements: retirements,
    }
}
fn code<T>(result: Result<T, BoundaryError>) -> String {
    match result {
        Ok(_) => panic!("unexpected success"),
        Err(failure) => failure.0.code,
    }
}

#[test]
fn whole_memory_groups_and_exposed_assistant_turns_retire_in_all_three_protocols() {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let (request, identity, retirements) = fixture(protocol);
        let original = request.history.clone();
        let built = build(input(&request, &identity, &retirements), None).unwrap();
        assert!(!format!("{:?}", built.request.history).contains("FORGOTTEN_VALUE"));
        assert_eq!(built.request.history.len(), 6);
        assert_eq!(built.request.history[0], original[0]);
        assert_eq!(built.request.history[3], original[7]);
        assert_eq!(built.request.history[5], original[9]);
        assert_eq!(
            built.source_projections[1].source_range,
            ItemRange { start: 1, end: 6 }
        );
        assert_eq!(
            built.source_projections[1].projected_range,
            ItemRange { start: 1, end: 2 }
        );
        assert_eq!(
            built.source_projections[3].source_range,
            ItemRange { start: 7, end: 8 }
        );
        assert_eq!(
            built.source_projections[3].projected_range,
            ItemRange { start: 3, end: 4 }
        );
        assert_eq!(built.mechanical_observations[0].exit_code, Some(7));
        assert_eq!(built.mechanical_observations[0].state, ToolState::Failed);
        assert!(built.mechanical_observations[0].error_present);
        assert_eq!(
            built.watermark.history_sha256,
            history_digest(&original).unwrap()
        );
        assert_eq!(request.history, original);
        assert!(
            group_history(protocol, &built.request.history)
                .unwrap()
                .open_call_ids
                .is_empty()
        );
    }
}

#[test]
fn retirement_rejects_partial_stale_or_mismatched_proofs_and_explicit_pins() {
    let (request, identity, base) = fixture(ModelProtocol::Responses);
    for field in 0..4 {
        let mut invalid = base.clone();
        match field {
            0 => invalid[0].range.start = 2,
            1 => invalid[0].source_sha256 = "f".repeat(64),
            2 => {
                invalid[0].origins = vec![RetirementOrigin::MemoryToolCall {
                    call_index: 2,
                    call_sha256: item_digest(&request.history[2]).unwrap(),
                    result_index: 5,
                    result_sha256: item_digest(&request.history[5]).unwrap(),
                    memory_revision: "a".repeat(64),
                }]
            }
            _ => invalid[0].observations[0].result_sha256 = "f".repeat(64),
        }
        assert_eq!(
            code(build(input(&request, &identity, &invalid), None)),
            "context_retirement"
        );
    }
    let pins = [ItemRange { start: 2, end: 3 }];
    let mut spec = input(&request, &identity, &base);
    spec.protected_ranges = &pins;
    assert_eq!(code(build(spec, None)), "context_retirement");
}

#[test]
fn committed_views_bind_the_exact_retirement_manifest_and_current_revision() {
    let (request, identity, retirements) = fixture(ModelProtocol::ChatCompletions);
    let first = build(input(&request, &identity, &retirements), None).unwrap();
    let view = ContextView {
        checkpoint_id: "checkpoint".into(),
        source_revision: 10,
        covered_prefix: 7,
        source_prefix_sha256: history_digest(&request.history[..7]).unwrap(),
        profile: request.profile.clone(),
        identity: identity.clone(),
        summary: json!({"facts":["Current safe context"]}),
        retirement_manifest: first.retirement_manifest,
    };
    let mut spec = input(&request, &identity, &retirements);
    spec.committed_view = Some(&view);
    let resumed = build(spec, None).unwrap();
    assert!(!format!("{:?}", resumed.request.history).contains("FORGOTTEN_VALUE"));
    let mut changed = retirements.clone();
    changed[0].current_memory_revision = "d".repeat(64);
    let mut spec = input(&request, &identity, &changed);
    spec.committed_view = Some(&view);
    assert_eq!(code(build(spec, None)), "context_view");
    let mut spec = input(&request, &identity, &[]);
    spec.committed_view = Some(&view);
    assert_eq!(code(build(spec, None)), "context_view");
}
