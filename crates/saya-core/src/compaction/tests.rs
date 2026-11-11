use super::*;
use crate::{
    boundary::{ModelStream, ModelUpdate, Operation},
    context_builder::ContextBudget,
    contract::{ModelProtocol, ModelPurpose, ToolCall},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    future::pending,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct SoonCancel(AtomicBool);
impl Cancellation for SoonCancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            self.0.store(true, Ordering::SeqCst);
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behavior {
    Valid,
    OmitRequired,
    ForgedSource,
    InventReference,
    InvalidEvidence,
    ToolCall,
    UnknownField,
    DuplicateKey,
    Oversized,
    DeltaMismatch,
    RetryOnce,
    ProgressThenError,
    Incomplete,
    Hung,
    Secret,
}

struct TestModel {
    behavior: Behavior,
    requests: StdMutex<Vec<ModelRequest>>,
}
impl TestModel {
    fn new(behavior: Behavior) -> Self {
        Self {
            behavior,
            requests: StdMutex::new(Vec::new()),
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
struct TestStream(VecDeque<Result<ModelUpdate, BoundaryError>>);
impl ModelStream for TestStream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async { self.0.pop_front().transpose() })
    }
}
impl Model for TestModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let attempt = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.clone());
                requests.len()
            };
            tokio::task::yield_now().await;
            if self.behavior == Behavior::Hung {
                return pending().await;
            }
            if self.behavior == Behavior::RetryOnce && attempt == 1 {
                return Err(error("server_error", "controlled retry"));
            }
            if self.behavior == Behavior::ProgressThenError {
                return Ok(Box::new(TestStream(VecDeque::from([
                    Ok(ModelUpdate::Progress),
                    Err(error("server_error", "controlled partial failure")),
                ]))) as Box<dyn ModelStream>);
            }
            let HistoryItem::UserText(body) = &request.history[0] else {
                panic!("summary data carrier")
            };
            let data: Value = serde_json::from_str(body).unwrap();
            let mut summary = response(&data);
            match self.behavior {
                Behavior::OmitRequired => {
                    summary.required_ids.clear();
                    summary.remaining.clear();
                }
                Behavior::ForgedSource => {
                    summary.objective[0].source_ids = vec!["history:999999".into()]
                }
                Behavior::InventReference => summary.references.push("out_forged".into()),
                Behavior::InvalidEvidence => {
                    summary.objective[0].evidence = EvidenceLevel::Observed
                }
                Behavior::Secret => summary.objective[0].text = "fixture-protected-value".into(),
                _ => {}
            }
            let mut value = serde_json::to_value(&summary).unwrap();
            if self.behavior == Behavior::UnknownField {
                value["extra"] = json!(true);
            }
            let mut text = value.to_string();
            if self.behavior == Behavior::DuplicateKey {
                text = text.replacen("{", "{\"schema_version\":1,", 1);
            }
            if self.behavior == Behavior::Oversized {
                text = "x".repeat(MAX_SUMMARY_BYTES + 1);
            }
            let items = if self.behavior == Behavior::ToolCall {
                vec![HistoryItem::ToolCall(ToolCall {
                    call_id: "forbidden".into(),
                    name: "exec".into(),
                    arguments: "{}".into(),
                })]
            } else {
                vec![HistoryItem::AssistantText(text.clone())]
            };
            let mut updates = VecDeque::new();
            if self.behavior == Behavior::DeltaMismatch {
                updates.push_back(Ok(ModelUpdate::TextDelta("different".into())));
            }
            if self.behavior == Behavior::Incomplete {
                updates.push_back(Ok(ModelUpdate::TextDelta(text)));
            } else {
                updates.push_back(Ok(ModelUpdate::Finished {
                    items,
                    usage: Some(Usage {
                        input_tokens: Some(100),
                        output_tokens: Some(50),
                        cached_input_tokens: None,
                        estimated: false,
                    }),
                }));
            }
            Ok(Box::new(TestStream(updates)) as Box<dyn ModelStream>)
        })
    }
}

fn response(data: &Value) -> StructuredSummary {
    let range: ItemRange = serde_json::from_value(data["assigned_source_range"].clone()).unwrap();
    let mut result = StructuredSummary {
        schema_version: 1,
        covered_range: range,
        objective: Vec::new(),
        constraints: Vec::new(),
        decisions: Vec::new(),
        completed: Vec::new(),
        remaining: Vec::new(),
        uncertainties: Vec::new(),
        required_ids: Vec::new(),
        references: Vec::new(),
    };
    let mut users = BTreeMap::new();
    for content in data["source_units"].as_array().unwrap() {
        for document in content.as_array().unwrap() {
            if let Some(prior) = document.get("prior_summary_data") {
                let summary = prior.get("summary").unwrap_or(prior);
                let previous: StructuredSummary = serde_json::from_value(summary.clone()).unwrap();
                for fact in previous
                    .facts()
                    .filter(|fact| fact.evidence == EvidenceLevel::UserRequest)
                {
                    for source in &fact.source_ids {
                        users.insert(source.clone(), ());
                    }
                }
                result.objective.extend(previous.objective);
                result.constraints.extend(previous.constraints);
                result.decisions.extend(previous.decisions);
                result.completed.extend(previous.completed);
                result.remaining.extend(previous.remaining);
                result.uncertainties.extend(previous.uncertainties);
            } else if document["role"] == "user" {
                let source = document["source_id"].as_str().unwrap().to_string();
                users.insert(source.clone(), ());
                result.objective.push(Fact {
                    text: "Earlier user request remains traceable".into(),
                    source_ids: vec![source],
                    evidence: EvidenceLevel::UserRequest,
                    status: FactStatus::Active,
                });
            }
        }
    }
    let required: Vec<MandatoryItem> =
        serde_json::from_value(data["required_items"].clone()).unwrap();
    for item in required {
        for list in [
            &mut result.objective,
            &mut result.constraints,
            &mut result.decisions,
            &mut result.completed,
            &mut result.remaining,
            &mut result.uncertainties,
        ] {
            list.retain(|fact| fact.text != item.text || fact.source_ids != item.source_ids);
        }
        let evidence = if item.source_ids.iter().all(|id| users.contains_key(id)) {
            EvidenceLevel::UserRequest
        } else {
            EvidenceLevel::Claim
        };
        let fact = Fact {
            text: item.text,
            source_ids: item.source_ids,
            evidence,
            status: item.status,
        };
        let target = match item.kind {
            MandatoryKind::Objective => &mut result.objective,
            MandatoryKind::Constraint => &mut result.constraints,
            MandatoryKind::Remaining => &mut result.remaining,
            MandatoryKind::Recovery => &mut result.uncertainties,
        };
        target.push(fact);
        result.required_ids.push(item.id);
    }
    result.references = data["references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reference| reference["id"].as_str().unwrap().into())
        .collect();
    result
}

struct Fixture {
    request: ModelRequest,
    identity: CountingIdentity,
    session_id: SessionId,
    required: Vec<MandatoryItem>,
    references: Vec<ContextReference>,
    versions: Vec<ContextVersion>,
    secrets: Vec<String>,
}
impl Fixture {
    fn new(protocol: ModelProtocol) -> Self {
        let task = "Validate all platforms before release. ".repeat(1000);
        Self {
            request: ModelRequest {
                request_id: "execution-fixture".into(),
                purpose: ModelPurpose::Execution,
                profile: "fixture".into(),
                instructions: "Current trusted execution contract".into(),
                dynamic_instructions: Some("Current PLAN and Manual Approval state".into()),
                max_output_tokens: Some(100),
                history: vec![
                    HistoryItem::UserText(task),
                    HistoryItem::AssistantText("Recorded implementation evidence. ".repeat(1000)),
                    HistoryItem::UserText("Continue the complete current objective".into()),
                ],
                media: Default::default(),
                tools: Vec::new(),
            },
            identity: CountingIdentity {
                protocol,
                model: "fixture-model".into(),
                adapter_version: "fixture-v1".into(),
            },
            session_id: SessionId::new(),
            required: vec![MandatoryItem {
                id: "acceptance".into(),
                kind: MandatoryKind::Remaining,
                text: "Validate all platforms before release.".into(),
                source_ids: vec!["history:0".into()],
                status: FactStatus::Active,
            }],
            references: Vec::new(),
            versions: Vec::new(),
            secrets: Vec::new(),
        }
    }
    fn input(&self) -> CompactionInput<'_> {
        CompactionInput {
            context: BuildInput {
                request: &self.request,
                identity: &self.identity,
                budget: ContextBudget::new(200_000, 100, 100),
                source_history: &self.request.history,
                history_offset: 0,
                revision: 7,
                committed_view: None,
                protected_ranges: &[],
                output_projections: &[],
                group_retirements: &[],
            },
            session_id: self.session_id,
            checkpoint_id: "checkpoint-fixture",
            prefix_end: self.request.history.len() - 1,
            work_mode: WorkMode::Plan,
            permissions: PermissionMode::Manual,
            required_items: &self.required,
            references: &self.references,
            context_versions: &self.versions,
            known_secrets: &self.secrets,
        }
    }
}
fn coordinator() -> CompactionCoordinator {
    CompactionCoordinator::new(CompactionConfig::default()).unwrap()
}
fn budget() -> SharedBudget {
    SharedBudget::new(64, Duration::from_secs(1800))
}
fn code<T>(result: Result<T, BoundaryError>) -> String {
    match result {
        Ok(_) => panic!("operation unexpectedly succeeded"),
        Err(error) => error.0.code,
    }
}

#[tokio::test]
async fn structured_summary_preserves_control_state_exact_constraints_and_original_history() {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let fixture = Fixture::new(protocol);
        let model = TestModel::new(Behavior::Valid);
        let shared = budget();
        let original = fixture.request.history.clone();
        let prepared = coordinator()
            .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
            .await
            .unwrap();
        assert_eq!(model.count(), 1);
        assert_eq!(shared.used().unwrap(), 1);
        assert_eq!(fixture.request.history, original);
        assert_eq!(
            prepared.projected_request.instructions,
            fixture.request.instructions
        );
        assert_eq!(
            prepared.projected_request.dynamic_instructions,
            fixture.request.dynamic_instructions
        );
        assert_eq!(
            prepared.projected_request.history.last(),
            fixture.request.history.last()
        );
        assert_eq!(
            prepared.record.calls[0]
                .usage
                .as_ref()
                .unwrap()
                .output_tokens,
            Some(50)
        );
        assert_eq!(prepared.view.source_revision, 7);
        assert_eq!(
            prepared.view.summary["required_items"][0]["text"],
            fixture.required[0].text
        );
        assert!(prepared.view.summary.get("permissions").is_none());
        prepared
            .verify_current(
                fixture.session_id,
                &fixture.request.history,
                7,
                "fixture",
                &fixture.identity,
                &fixture.versions,
            )
            .unwrap();
        for request in model.requests.lock().unwrap().iter() {
            assert_eq!(request.purpose, ModelPurpose::Compaction);
            assert!(request.tools.is_empty());
            assert_eq!(request.instructions, COMPACT_INSTRUCTIONS);
            assert!(request.dynamic_instructions.is_none());
        }
    }
}

#[tokio::test]
async fn same_source_concurrent_and_repeated_preparations_share_model_calls() {
    let fixture = Fixture::new(ModelProtocol::ChatCompletions);
    let coordinator = coordinator();
    let model = TestModel::new(Behavior::Valid);
    let shared = budget();
    let mut second_input = fixture.input();
    second_input.checkpoint_id = "another-proposed-checkpoint";
    let (first, second) = tokio::join!(
        coordinator.prepare(fixture.input(), &model, &shared, None, &NeverCancel),
        coordinator.prepare(second_input, &model, &shared, None, &NeverCancel)
    );
    assert_eq!(first.unwrap().view, second.unwrap().view);
    coordinator
        .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
        .await
        .unwrap();
    assert_eq!(model.count(), 1);
    assert_eq!(shared.used().unwrap(), 1);
}

#[tokio::test]
async fn cached_preparation_rechecks_input_limits_and_accepts_output_target_changes() {
    let fixture = Fixture::new(ModelProtocol::ChatCompletions);
    let coordinator = coordinator();
    let model = TestModel::new(Behavior::Valid);
    let shared = budget();
    coordinator
        .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
        .await
        .unwrap();
    let mut smaller = fixture.input();
    smaller.context.budget.window_tokens = 4000;
    assert_eq!(
        code(
            coordinator
                .prepare(smaller, &model, &shared, None, &NeverCancel)
                .await
        ),
        "context_limit"
    );
    let mut margin = fixture.input();
    margin.context.budget.safety_margin = 198_000;
    assert_eq!(
        code(
            coordinator
                .prepare(margin, &model, &shared, None, &NeverCancel)
                .await
        ),
        "context_limit"
    );
    let mut reserve = fixture.input();
    reserve.context.budget.output_reserve = 199_000;
    let prepared = coordinator
        .prepare(reserve, &model, &shared, None, &NeverCancel)
        .await
        .unwrap();
    assert_eq!(
        prepared.record.source.item_count,
        fixture.request.history.len()
    );
}

#[tokio::test]
async fn malformed_misreferenced_or_tool_outputs_are_never_applied_or_retried() {
    for behavior in [
        Behavior::OmitRequired,
        Behavior::ForgedSource,
        Behavior::InventReference,
        Behavior::InvalidEvidence,
        Behavior::ToolCall,
        Behavior::UnknownField,
        Behavior::DuplicateKey,
        Behavior::Oversized,
        Behavior::DeltaMismatch,
        Behavior::Incomplete,
    ] {
        let fixture = Fixture::new(ModelProtocol::ChatCompletions);
        let model = TestModel::new(behavior);
        let shared = budget();
        assert_eq!(
            code(
                coordinator()
                    .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
                    .await
            ),
            "compaction_invalid"
        );
        assert_eq!(shared.used().unwrap(), 1);
        assert_eq!(model.count(), 1);
        assert_eq!(fixture.request.history.len(), 3);
    }
}

#[tokio::test(start_paused = true)]
async fn retries_consume_the_existing_shared_budget_and_stop_after_progress() {
    let fixture = Fixture::new(ModelProtocol::ChatCompletions);
    let model = TestModel::new(Behavior::RetryOnce);
    let shared = budget();
    let prepared = coordinator()
        .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
        .await
        .unwrap();
    assert_eq!(shared.used().unwrap(), 2);
    assert_eq!(prepared.record.calls.len(), 2);
    assert_eq!(prepared.record.calls[0].outcome, "server_error");
    assert_eq!(prepared.record.calls[1].attempt, 1);
    assert_ne!(
        prepared.record.calls[0].request_id,
        prepared.record.calls[1].request_id
    );
    let model = TestModel::new(Behavior::ProgressThenError);
    let shared = budget();
    assert_eq!(
        code(
            coordinator()
                .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
                .await
        ),
        "server_error"
    );
    assert_eq!(model.count(), 1);
    assert_eq!(shared.used().unwrap(), 1);
    let model = TestModel::new(Behavior::RetryOnce);
    let shared = SharedBudget::new(1, Duration::from_secs(60));
    assert_eq!(
        code(
            coordinator()
                .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
                .await
        ),
        "limit_reached"
    );
    assert_eq!(model.count(), 1);
}

#[tokio::test(start_paused = true)]
async fn cancellation_and_active_or_operation_time_limits_bound_hung_models() {
    let fixture = Fixture::new(ModelProtocol::ChatCompletions);
    let model = TestModel::new(Behavior::Hung);
    let shared = budget();
    let cancel = SoonCancel(AtomicBool::new(false));
    assert_eq!(
        code(
            coordinator()
                .prepare(fixture.input(), &model, &shared, None, &cancel)
                .await
        ),
        "cancelled"
    );
    assert_eq!(model.count(), 1);
    let model = TestModel::new(Behavior::Hung);
    let shared = SharedBudget::new(64, Duration::from_millis(2));
    assert_eq!(
        code(
            coordinator()
                .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
                .await
        ),
        "limit_reached"
    );
    let model = TestModel::new(Behavior::Hung);
    let config = CompactionConfig {
        operation_timeout: Duration::from_millis(2),
        ..CompactionConfig::default()
    };
    let coordinator = CompactionCoordinator::new(config).unwrap();
    assert_eq!(
        code(
            coordinator
                .prepare(fixture.input(), &model, &budget(), None, &NeverCancel)
                .await
        ),
        "model_timeout"
    );
}

#[tokio::test]
async fn blocks_unsafe_cuts_pins_open_groups_and_oversized_single_groups_before_any_model_call() {
    let mut fixture = Fixture::new(ModelProtocol::ChatCompletions);
    fixture.request.history = vec![
        HistoryItem::UserText("old task".into()),
        HistoryItem::ToolCall(ToolCall {
            call_id: "a".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }),
        HistoryItem::ToolResult {
            call_id: "a".into(),
            result: crate::contract::ToolResult {
                ok: true,
                data: None,
                error: None,
                truncated: false,
            },
        },
        HistoryItem::UserText("latest".into()),
    ];
    fixture.required.clear();
    let model = TestModel::new(Behavior::Valid);
    let shared = budget();
    let mut input = fixture.input();
    input.prefix_end = 2;
    assert_eq!(
        code(
            coordinator()
                .prepare(input, &model, &shared, None, &NeverCancel)
                .await
        ),
        "compaction_unsafe"
    );
    let pins = [ItemRange { start: 1, end: 2 }];
    let mut input = fixture.input();
    input.context.protected_ranges = &pins;
    assert_eq!(
        code(
            coordinator()
                .prepare(input, &model, &shared, None, &NeverCancel)
                .await
        ),
        "compaction_unsafe"
    );
    fixture.request.history.remove(2);
    assert_eq!(
        code(
            coordinator()
                .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
                .await
        ),
        "invalid_history"
    );
    assert_eq!(model.count(), 0);
    fixture.request.history = vec![
        HistoryItem::UserText("source ".repeat(15_000)),
        HistoryItem::UserText("latest".into()),
    ];
    let mut input = fixture.input();
    input.context.budget.window_tokens = 30_000;
    assert_eq!(
        code(
            coordinator()
                .prepare(input, &model, &shared, None, &NeverCancel)
                .await
        ),
        "context_limit"
    );
    assert_eq!(shared.used().unwrap(), 0);
}

#[tokio::test]
async fn chunking_and_merge_preserve_every_user_source_and_respect_call_limit() {
    let mut fixture = Fixture::new(ModelProtocol::ChatCompletions);
    fixture.request.history.clear();
    fixture.required.clear();
    for _ in 0..4 {
        fixture.request.history.push(HistoryItem::UserText(
            "original user requirement ".repeat(500),
        ));
        fixture.request.history.push(HistoryItem::AssistantText(
            "recorded earlier work ".repeat(500),
        ));
    }
    fixture
        .request
        .history
        .push(HistoryItem::UserText("latest exact task".into()));
    let model = TestModel::new(Behavior::Valid);
    let shared = budget();
    let mut input = fixture.input();
    input.context.budget.window_tokens = 30_000;
    let prepared = coordinator()
        .prepare(input, &model, &shared, None, &NeverCancel)
        .await
        .unwrap();
    assert!(model.count() > 1);
    assert!(
        prepared
            .record
            .calls
            .iter()
            .any(|call| call.stage == CompactionStage::Merge)
    );
    assert_eq!(
        prepared.view.summary["summary"]["objective"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(shared.used().unwrap(), model.count());
    let limited = CompactionCoordinator::new(CompactionConfig {
        max_model_calls: 1,
        ..CompactionConfig::default()
    })
    .unwrap();
    let mut input = fixture.input();
    input.context.budget.window_tokens = 30_000;
    assert_eq!(
        code(
            limited
                .prepare(
                    input,
                    &TestModel::new(Behavior::Valid),
                    &budget(),
                    None,
                    &NeverCancel
                )
                .await
        ),
        "limit_reached"
    );
}

#[tokio::test]
async fn provider_opaque_and_signed_state_is_hashed_without_exposing_payloads() {
    for protocol in [ModelProtocol::Responses, ModelProtocol::AnthropicMessages] {
        let mut fixture = Fixture::new(protocol);
        let opaque = if protocol == ModelProtocol::Responses {
            json!({"type":"reasoning","encrypted_content":"PRIVATE_OPAQUE_PAYLOAD"})
        } else {
            json!({"type":"thinking","thinking":"PRIVATE_THINKING_PAYLOAD","signature":"PRIVATE_SIGNATURE_PAYLOAD"})
        };
        fixture.request.history.insert(
            1,
            HistoryItem::ProviderItem {
                protocol,
                value: opaque,
            },
        );
        let model = TestModel::new(Behavior::Valid);
        let prepared = coordinator()
            .prepare(fixture.input(), &model, &budget(), None, &NeverCancel)
            .await
            .unwrap();
        assert_eq!(prepared.view.covered_prefix, 3);
        for request in model.requests.lock().unwrap().iter() {
            let serialized = format!("{:?}", request.history);
            assert!(!serialized.contains("PRIVATE_OPAQUE_PAYLOAD"));
            assert!(!serialized.contains("PRIVATE_THINKING_PAYLOAD"));
            assert!(!serialized.contains("PRIVATE_SIGNATURE_PAYLOAD"));
            assert!(serialized.contains("opaque_state_preserved_by_host"));
        }
    }
}

#[tokio::test]
async fn exact_reference_completeness_is_host_owned_and_forged_ids_fail() {
    let mut fixture = Fixture::new(ModelProtocol::ChatCompletions);
    fixture.references = vec![ContextReference {
        id: "out_fixture".into(),
        source_ids: vec!["history:1".into()],
        metadata: json!({"artifact_complete":false,"coverage":"retained-ranges-only","call_id":"a","run_id":"fixture-run"}),
    }];
    let prepared = coordinator()
        .prepare(
            fixture.input(),
            &TestModel::new(Behavior::Valid),
            &budget(),
            None,
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(
        prepared.view.summary["references"][0]["metadata"]["artifact_complete"],
        false
    );
    assert_eq!(
        prepared.view.summary["summary"]["references"][0],
        "out_fixture"
    );
    assert_eq!(
        code(
            coordinator()
                .prepare(
                    fixture.input(),
                    &TestModel::new(Behavior::InventReference),
                    &budget(),
                    None,
                    &NeverCancel
                )
                .await
        ),
        "compaction_invalid"
    );
}

#[tokio::test]
async fn credentials_are_redacted_in_sources_and_rejected_in_summary_or_mandatory_metadata() {
    let mut fixture = Fixture::new(ModelProtocol::ChatCompletions);
    fixture.secrets = vec!["fixture-protected-value".into()];
    if let HistoryItem::AssistantText(text) = &mut fixture.request.history[1] {
        text.push_str(" fixture-protected-value");
    }
    let model = TestModel::new(Behavior::Valid);
    coordinator()
        .prepare(fixture.input(), &model, &budget(), None, &NeverCancel)
        .await
        .unwrap();
    assert!(
        model
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| !format!("{:?}", request.history).contains("fixture-protected-value"))
    );
    assert_eq!(
        code(
            coordinator()
                .prepare(
                    fixture.input(),
                    &TestModel::new(Behavior::Secret),
                    &budget(),
                    None,
                    &NeverCancel
                )
                .await
        ),
        "compaction_sensitive_data"
    );
    fixture.required[0].text = "fixture-protected-value".into();
    let model = TestModel::new(Behavior::Valid);
    assert_eq!(
        code(
            coordinator()
                .prepare(fixture.input(), &model, &budget(), None, &NeverCancel)
                .await
        ),
        "compaction_sensitive_data"
    );
    assert_eq!(model.count(), 0);
}

#[tokio::test]
async fn cas_verification_rejects_changed_session_revision_source_profile_versions_and_integrity() {
    let fixture = Fixture::new(ModelProtocol::ChatCompletions);
    let prepared = coordinator()
        .prepare(
            fixture.input(),
            &TestModel::new(Behavior::Valid),
            &budget(),
            None,
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(
        code(prepared.verify_current(
            fixture.session_id,
            &fixture.request.history,
            8,
            "fixture",
            &fixture.identity,
            &[]
        )),
        "compaction_conflict"
    );
    assert_eq!(
        code(prepared.verify_current(
            SessionId::new(),
            &fixture.request.history,
            7,
            "fixture",
            &fixture.identity,
            &[]
        )),
        "compaction_conflict"
    );
    assert_eq!(
        code(prepared.verify_current(
            fixture.session_id,
            &fixture.request.history,
            7,
            "changed",
            &fixture.identity,
            &[]
        )),
        "compaction_conflict"
    );
    let mut history = fixture.request.history.clone();
    history.push(HistoryItem::UserText("concurrent correction".into()));
    assert_eq!(
        code(prepared.verify_current(
            fixture.session_id,
            &history,
            7,
            "fixture",
            &fixture.identity,
            &[]
        )),
        "compaction_conflict"
    );
    let versions = [ContextVersion {
        namespace: "memory".into(),
        id: "global".into(),
        revision: 2,
        sha256: "a".repeat(64),
    }];
    assert_eq!(
        code(prepared.verify_current(
            fixture.session_id,
            &fixture.request.history,
            7,
            "fixture",
            &fixture.identity,
            &versions
        )),
        "compaction_conflict"
    );
    let mut tampered = prepared;
    tampered.view.summary["summary"]["objective"][0]["text"] = json!("changed after preparation");
    assert_eq!(code(tampered.verify_integrity()), "compaction_invalid");
}

#[tokio::test]
async fn repeated_compaction_preserves_original_sources_and_explicit_supersession() {
    let mut fixture = Fixture::new(ModelProtocol::ChatCompletions);
    let first = coordinator()
        .prepare(
            fixture.input(),
            &TestModel::new(Behavior::Valid),
            &budget(),
            None,
            &NeverCancel,
        )
        .await
        .unwrap();
    fixture.request.history.push(HistoryItem::AssistantText(
        "more recorded work ".repeat(1000),
    ));
    fixture.request.history.push(HistoryItem::UserText(
        "Newest correction remains raw".into(),
    ));
    fixture.required[0].status = FactStatus::Superseded;
    let mut input = fixture.input();
    input.context.committed_view = Some(&first.view);
    input.context.revision = 8;
    input.checkpoint_id = "checkpoint-second";
    let second = coordinator()
        .prepare(
            input,
            &TestModel::new(Behavior::Valid),
            &budget(),
            None,
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(
        second.record.previous_checkpoint.as_deref(),
        Some("checkpoint-fixture")
    );
    assert_eq!(
        second.view.summary["required_items"][0]["status"],
        "superseded"
    );
    assert_eq!(
        second.view.summary["summary"]["remaining"][0]["source_ids"][0],
        "history:0"
    );
    assert_eq!(
        second.projected_request.history.last(),
        fixture.request.history.last()
    );
    let versions = [ContextVersion {
        namespace: "memory".into(),
        id: "global".into(),
        revision: 2,
        sha256: "a".repeat(64),
    }];
    let mut stale = fixture.input();
    stale.context.committed_view = Some(&first.view);
    stale.context_versions = &versions;
    assert_eq!(
        code(
            coordinator()
                .prepare(
                    stale,
                    &TestModel::new(Behavior::Valid),
                    &budget(),
                    None,
                    &NeverCancel
                )
                .await
        ),
        "compaction_conflict"
    );
}

#[test]
fn runtime_prompt_snapshots_are_english_and_identical_to_published_document_copies() {
    assert!(COMPACT_INSTRUCTIONS.is_ascii());
    assert!(CONTINUATION_INSTRUCTIONS.is_ascii());
    assert_eq!(
        COMPACT_INSTRUCTIONS,
        include_str!("../../../../docs/prompt/v1/compact.md")
    );
    assert_eq!(
        CONTINUATION_INSTRUCTIONS,
        include_str!("../../../../docs/prompt/v1/compaction-continuation.md")
    );
    assert_eq!(summary_schema()["additionalProperties"], false);
}

#[test]
fn compact_prompt_explains_actual_source_roles_and_bounded_fact_output() {
    for role in [
        "user",
        "assistant",
        "assistant_partial",
        "assistant_tool_call",
        "tool_observation",
        "provider_state",
        "retired_group",
        "prior_summary_data",
    ] {
        assert!(COMPACT_INSTRUCTIONS.contains(&format!("`{role}`")));
    }
    for rule in [
        "An assistant's completion report remains a claim.",
        "An assistant tool call records an intended operation",
        "Never cite retired source",
        "A mandatory fact can also satisfy user-source coverage.",
        "Empty categories use empty arrays.",
    ] {
        assert!(COMPACT_INSTRUCTIONS.contains(rule));
    }
}

#[test]
fn assistant_completion_claims_cannot_be_upgraded_to_observed_or_user_requests() {
    let sources = BTreeMap::from([
        ("history:0".into(), schema::SourceRole::User),
        ("history:1".into(), schema::SourceRole::Assistant),
        ("history:2".into(), schema::SourceRole::Tool),
        ("history:3".into(), schema::SourceRole::Provider),
        ("history:4".into(), schema::SourceRole::Retired),
        ("history:5".into(), schema::SourceRole::Mechanical),
    ]);
    let validation = schema::Validation {
        range: ItemRange { start: 0, end: 6 },
        sources: &sources,
        required: Vec::new(),
        references: Vec::new(),
    };
    let mut summary = StructuredSummary {
        schema_version: 1,
        covered_range: validation.range,
        objective: vec![Fact {
            text: "Run the tests".into(),
            source_ids: vec!["history:0".into()],
            evidence: EvidenceLevel::UserRequest,
            status: FactStatus::Active,
        }],
        constraints: Vec::new(),
        decisions: Vec::new(),
        completed: vec![Fact {
            text: "The assistant reported that tests passed".into(),
            source_ids: vec!["history:1".into()],
            evidence: EvidenceLevel::Claim,
            status: FactStatus::Active,
        }],
        remaining: Vec::new(),
        uncertainties: Vec::new(),
        required_ids: Vec::new(),
        references: Vec::new(),
    };
    schema::validate(&summary, &validation).unwrap();
    summary.completed[0].evidence = EvidenceLevel::Inference;
    schema::validate(&summary, &validation).unwrap();
    for evidence in [EvidenceLevel::Observed, EvidenceLevel::UserRequest] {
        summary.completed[0].evidence = evidence;
        assert_eq!(
            schema::validate(&summary, &validation)
                .unwrap_err()
                .0
                .message,
            "Summary evidence level differs from its source role"
        );
    }
    for source in ["history:2", "history:3", "history:5"] {
        summary.completed[0].source_ids = vec![source.into()];
        summary.completed[0].evidence = EvidenceLevel::Observed;
        schema::validate(&summary, &validation).unwrap();
    }
    summary.completed[0].source_ids = vec!["history:4".into()];
    summary.completed[0].evidence = EvidenceLevel::Claim;
    assert_eq!(
        schema::validate(&summary, &validation)
            .unwrap_err()
            .0
            .message,
        "Summary fact text or source references are invalid"
    );
}

#[test]
fn mandatory_items_spanning_original_groups_create_one_safe_summary_unit() {
    let mut fixture = Fixture::new(ModelProtocol::ChatCompletions);
    fixture.request.history.insert(
        2,
        HistoryItem::UserText("Original linked correction".into()),
    );
    fixture
        .request
        .history
        .insert(3, HistoryItem::AssistantText("Later work".repeat(1000)));
    fixture.required[0].source_ids = vec!["history:0".into(), "history:2".into()];
    let input = fixture.input();
    let plan = planning::plan(&input, &CompactionConfig::default(), None).unwrap();
    assert_eq!(plan.units[0].range, ItemRange { start: 0, end: 3 });
    assert_eq!(plan.units[0].content.len(), 3);
    assert_eq!(plan.validation_for(plan.units[0].range).required.len(), 1);
}

#[tokio::test]
async fn one_user_long_running_tool_work_can_compact_and_continue_across_all_protocols() {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let mut fixture = Fixture::new(protocol);
        fixture.request.history = vec![HistoryItem::UserText("Complete all six checks.".into())];
        fixture.required = vec![MandatoryItem {
            id: "objective".into(),
            kind: MandatoryKind::Objective,
            text: "Complete all six checks.".into(),
            source_ids: vec!["history:0".into()],
            status: FactStatus::Active,
        }];
        for index in 0..6 {
            let call_id = format!("call-{index}");
            fixture
                .request
                .history
                .push(HistoryItem::ToolCall(ToolCall {
                    call_id: call_id.clone(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                }));
            fixture.request.history.push(HistoryItem::ToolResult {
                call_id,
                result: crate::contract::ToolResult {
                    ok: true,
                    data: Some(json!({"content":"observed evidence ".repeat(240)})),
                    error: None,
                    truncated: false,
                },
            });
        }
        let original = fixture.request.history.clone();
        let mut input = fixture.input();
        input.prefix_end = fixture.request.history.len();
        input.context.budget.window_tokens = 30_000;
        let first = coordinator()
            .prepare(
                input,
                &TestModel::new(Behavior::Valid),
                &budget(),
                None,
                &NeverCancel,
            )
            .await
            .unwrap();
        assert_eq!(first.projected_request.history[0], original[0]);
        assert_eq!(
            first.record.retained_groups[0].range,
            ItemRange { start: 0, end: 1 }
        );
        assert_eq!(first.view.covered_prefix, original.len());
        first
            .verify_current(
                fixture.session_id,
                &original,
                7,
                "fixture",
                &fixture.identity,
                &[],
            )
            .unwrap();
        assert_eq!(fixture.request.history, original);
        fixture
            .request
            .history
            .push(HistoryItem::ToolCall(ToolCall {
                call_id: "follow-up".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }));
        fixture.request.history.push(HistoryItem::ToolResult {
            call_id: "follow-up".into(),
            result: crate::contract::ToolResult {
                ok: true,
                data: Some(json!({"content":"new evidence ".repeat(400)})),
                error: None,
                truncated: false,
            },
        });
        let mut next = fixture.input();
        next.context.committed_view = Some(&first.view);
        next.context.revision = 8;
        next.context.budget.window_tokens = 30_000;
        next.prefix_end = fixture.request.history.len();
        next.checkpoint_id = "next-checkpoint";
        let model = TestModel::new(Behavior::Valid);
        let second = coordinator()
            .prepare(next, &model, &budget(), None, &NeverCancel)
            .await
            .unwrap();
        assert_eq!(second.projected_request.history[0], original[0]);
        assert_eq!(
            second.record.previous_checkpoint,
            Some(first.view.checkpoint_id)
        );
        let captured = model.requests.lock().unwrap();
        let HistoryItem::UserText(body) = &captured[0].history[0] else {
            panic!("summary input")
        };
        let data: Value = serde_json::from_str(body).unwrap();
        assert!(
            data["source_units"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|unit| unit.as_array().unwrap())
                .any(|document| document["source_id"] == "history:0"
                    && document["display"]["text"] == "Complete all six checks.")
        );
    }
}

#[test]
fn compaction_reads_projected_retirement_units_without_original_memory_bytes() {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let (request, identity, retirements) =
            crate::context_builder::tests_retirement::fixture(protocol);
        let mut fixture = Fixture::new(protocol);
        fixture.request = request;
        fixture.identity = identity;
        fixture.required.clear();
        let mut input = fixture.input();
        input.context.group_retirements = &retirements;
        input.prefix_end = 7;
        let config = CompactionConfig::default();
        let plan = planning::plan(&input, &config, None).unwrap();
        assert_eq!(plan.sources["history:4"], schema::SourceRole::Retired);
        assert_eq!(plan.sources["history:5"], schema::SourceRole::Mechanical);
        assert_eq!(plan.mechanical_observations[0].exit_code, Some(7));
        let request = planning::request_for(
            &plan.units,
            &plan,
            &input,
            &config,
            CompactionStage::Summarize,
        )
        .unwrap();
        assert!(!format!("{:?}", request.history).contains("FORGOTTEN_VALUE"));
        let forbidden = [MandatoryItem {
            id: "old-memory".into(),
            kind: MandatoryKind::Constraint,
            text: "FORGOTTEN_VALUE".into(),
            source_ids: vec!["history:4".into()],
            status: FactStatus::Active,
        }];
        input.required_items = &forbidden;
        assert_eq!(
            code(planning::plan(&input, &config, None)),
            "compaction_invalid"
        );
    }
}

struct TestWireCounter;
impl RequestCounter for TestWireCounter {
    fn count(
        &self,
        request: &ModelRequest,
        identity: &CountingIdentity,
        fingerprint: &str,
    ) -> Result<crate::context_builder::CountEvidence, BoundaryError> {
        let bytes = serde_json::to_vec(&request.history).unwrap().len()
            + request.instructions.len()
            + request.dynamic_instructions.as_ref().map_or(0, String::len)
            + 256;
        Ok(crate::context_builder::CountEvidence {
            request_fingerprint: fingerprint.into(),
            identity: identity.clone(),
            input_tokens: (bytes as u64).div_ceil(4),
            method: crate::context_builder::CountMethod::SerializedHeuristic {
                name: "controlled-wire-heuristic".into(),
                version: "1".into(),
            },
        })
    }
}

#[tokio::test]
async fn an_unreachable_fifty_five_percent_target_is_not_reported_as_ready_compaction() {
    let mut fixture = Fixture::new(ModelProtocol::ChatCompletions);
    fixture.request.history = vec![
        HistoryItem::UserText("goal ".repeat(4600)),
        HistoryItem::AssistantText("work ".repeat(3200)),
    ];
    fixture.required.clear();
    let mut input = fixture.input();
    input.context.budget = ContextBudget::new(10_000, 100, 100);
    input.prefix_end = fixture.request.history.len();
    let config = CompactionConfig {
        summary_output_tokens: 512,
        ..CompactionConfig::default()
    };
    let model = TestModel::new(Behavior::Valid);
    assert_eq!(
        code(
            CompactionCoordinator::new(config)
                .unwrap()
                .prepare(
                    input,
                    &model,
                    &budget(),
                    Some(&TestWireCounter),
                    &NeverCancel
                )
                .await
        ),
        "context_limit"
    );
    assert!(model.count() > 0);
}

#[tokio::test]
async fn host_metadata_overflow_stops_before_model_calls() {
    let mut fixture = Fixture::new(ModelProtocol::Responses);
    let reference = ContextReference {
        id: "out_fixture".into(),
        source_ids: vec!["history:0".into()],
        metadata: json!({}),
    };
    fixture.references = vec![reference; 257];
    let model = TestModel::new(Behavior::Valid);
    let shared = budget();
    assert_eq!(
        code(
            CompactionCoordinator::new(CompactionConfig::default())
                .unwrap()
                .prepare(fixture.input(), &model, &shared, None, &NeverCancel)
                .await
        ),
        "context_limit"
    );
    assert_eq!(model.count(), 0);
}
