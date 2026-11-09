use saya_core::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, StoreChange,
    },
    compaction::{
        EvidenceLevel, Fact, FactStatus, MandatoryItem, MandatoryKind, StructuredSummary,
    },
    context_builder::{ContextBudget, ItemRange},
    contract::{
        BoundaryError, HistoryItem, ModelProtocol, ModelPurpose, PermissionMode, Run, RunId,
        RunStatus, Session, SessionId, WorkMode,
    },
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    review::ReviewConfig,
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    sqlite::SqliteStore,
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use serde_json::Value;
use std::{
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
};

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct Stream(Option<Vec<HistoryItem>>);
impl ModelStream for Stream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async {
            Ok(self
                .0
                .take()
                .map(|items| ModelUpdate::Finished { items, usage: None }))
        })
    }
}
#[derive(Default)]
struct SummaryModel(Mutex<Vec<ModelRequest>>, Option<String>);
impl Model for SummaryModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.0.lock().unwrap().push(request.clone());
            let text = if request.purpose == ModelPurpose::Compaction {
                assert!(request.tools.is_empty());
                let HistoryItem::UserText(text) = &request.history[0] else {
                    panic!("summary data carrier")
                };
                let data: Value = serde_json::from_str(text).unwrap();
                let range: ItemRange =
                    serde_json::from_value(data["assigned_source_range"].clone()).unwrap();
                let mut summary = StructuredSummary {
                    schema_version: 1,
                    covered_range: range,
                    objective: vec![],
                    constraints: vec![],
                    decisions: vec![],
                    completed: vec![],
                    remaining: vec![],
                    uncertainties: vec![],
                    required_ids: vec![],
                    references: vec![],
                };
                for unit in data["source_units"].as_array().unwrap() {
                    for source in unit.as_array().unwrap() {
                        if let Some(prior) = source.get("prior_summary_data") {
                            let previous: StructuredSummary = serde_json::from_value(
                                prior.get("summary").unwrap_or(prior).clone(),
                            )
                            .unwrap();
                            summary.objective.extend(previous.objective);
                            summary.constraints.extend(previous.constraints);
                            summary.decisions.extend(previous.decisions);
                            summary.completed.extend(previous.completed);
                            summary.remaining.extend(previous.remaining);
                            summary.uncertainties.extend(previous.uncertainties);
                        } else if source["role"] == "user" {
                            summary.objective.push(Fact {
                                text: "User task remains active".into(),
                                source_ids: vec![source["source_id"].as_str().unwrap().into()],
                                evidence: EvidenceLevel::UserRequest,
                                status: FactStatus::Active,
                            });
                        }
                    }
                }
                let facts = std::mem::take(&mut summary.objective);
                for fact in facts {
                    if let Some(last) = summary.objective.last_mut().filter(|last| {
                        last.text == fact.text
                            && last.source_ids.len() + fact.source_ids.len() <= 32
                    }) {
                        last.source_ids.extend(fact.source_ids);
                    } else {
                        summary.objective.push(fact);
                    }
                }
                let required: Vec<MandatoryItem> =
                    serde_json::from_value(data["required_items"].clone()).unwrap();
                for item in required {
                    let fact = Fact {
                        text: item.text,
                        source_ids: item.source_ids,
                        evidence: EvidenceLevel::UserRequest,
                        status: item.status,
                    };
                    match item.kind {
                        MandatoryKind::Objective => summary.objective.push(fact),
                        MandatoryKind::Constraint => summary.constraints.push(fact),
                        MandatoryKind::Remaining => summary.remaining.push(fact),
                        MandatoryKind::Recovery => summary.uncertainties.push(fact),
                    }
                    summary.required_ids.push(item.id);
                }
                summary.references = data["references"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|reference| reference["id"].as_str().unwrap().into())
                    .collect();
                serde_json::to_string(&summary).unwrap()
            } else {
                assert_eq!(request.purpose, ModelPurpose::Execution);
                self.1.clone().unwrap_or_else(|| "completed".into())
            };
            Ok(
                Box::new(Stream(Some(vec![HistoryItem::AssistantText(text)])))
                    as Box<dyn ModelStream>,
            )
        })
    }
}
fn fixture(root: &Path, protocol: ModelProtocol) -> (Session, Run) {
    let session = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol,
        model: "fixture".into(),
        history: vec![
            HistoryItem::UserText("Keep every source document and finish the task.".into()),
            HistoryItem::AssistantText("temporary repetitive observation ".repeat(900)),
            HistoryItem::UserText("Keep proceeding".into()),
            HistoryItem::AssistantText("temporary repetitive observation ".repeat(900)),
            HistoryItem::UserText("Continue from the recorded evidence.".into()),
        ],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: session.work_mode,
        permissions: PermissionMode::Auto,
    };
    (session, run)
}
fn runtime(
    root: &Path,
    run: Run,
    model: Arc<dyn Model>,
    store: Arc<dyn Store>,
    model_requests: usize,
) -> Result<AgentRuntime, BoundaryError> {
    AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store,
        policy: Arc::new(Policy::new(root, PermissionConfig::default())?),
        files: Arc::new(ReadTools::new(root, &[], false, ReadLimits::default()).unwrap()),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "fixture task".into(),
        confirmation: None,
        limits: RuntimeLimits {
            model_requests,
            ..RuntimeLimits::default()
        },
    })?
    .with_context_budget(ContextBudget::new(64_000, 2048, 6401))
}

#[tokio::test]
async fn automatic_compaction_rebuilds_then_dispatches_and_restores_all_protocols()
-> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let temp = tempfile::tempdir()?;
        let (mut session, run) = fixture(temp.path(), protocol);
        let raw = session.history.clone();
        let store = Arc::new(SqliteStore::open(&temp.path().join("sessions"), vec![])?);
        let model = Arc::new(SummaryModel::default());
        let mut engine = runtime(temp.path(), run, model.clone(), store.clone(), 64)?;
        let result = engine.run(&mut session).await?;
        assert_eq!(result.status, RunStatus::Completed, "{:?}", result.error);
        assert_eq!(&session.history[..raw.len()], raw.as_slice());
        let saved = store
            .context_checkpoint(session.id)
            .await?
            .expect("published checkpoint");
        saved.validate()?;
        assert_eq!(store.load_session(session.id).await?.unwrap(), session);
        {
            let seen = model.0.lock().unwrap();
            assert!(
                seen.iter()
                    .any(|request| request.purpose == ModelPurpose::Compaction)
            );
            let execution = seen
                .iter()
                .find(|request| request.purpose == ModelPurpose::Execution)
                .unwrap();
            assert!(
                execution
                    .dynamic_instructions
                    .as_ref()
                    .unwrap()
                    .contains("\"checkpoint_selected_by_host\":true")
            );
            assert!(execution.history.iter().any(|item| matches!(item, HistoryItem::UserText(text) if text == "Continue from the recorded evidence.")));
            assert!(!execution.history.iter().any(|item| matches!(item, HistoryItem::AssistantText(text) if text.contains("temporary repetitive observation"))));
        }
        let resumed_model = Arc::new(SummaryModel::default());
        let resumed_run = Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: session.work_mode,
            permissions: PermissionMode::Auto,
        };
        let mut resumed = runtime(
            temp.path(),
            resumed_run,
            resumed_model.clone(),
            store.clone(),
            64,
        )?;
        assert_eq!(
            resumed.run(&mut session).await?.status,
            RunStatus::Completed
        );
        assert!(
            resumed_model
                .0
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.purpose == ModelPurpose::Execution)
        );
        assert_eq!(store.context_checkpoint(session.id).await?.unwrap(), saved);
    }
    Ok(())
}

#[tokio::test]
async fn manual_compaction_preserves_history_and_uses_no_execution_request()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let (mut session, run) = fixture(temp.path(), ModelProtocol::Responses);
    let raw = session.history.clone();
    let store = Arc::new(MemoryStore::default());
    let model = Arc::new(SummaryModel::default());
    let mut engine = runtime(temp.path(), run, model.clone(), store.clone(), 64)?;
    let result = engine.compact(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed, "{:?}", result.error);
    assert_eq!(session.history, raw);
    assert!(store.context_checkpoint(session.id).await?.is_some());
    assert!(
        model
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.purpose == ModelPurpose::Compaction)
    );
    Ok(())
}

#[tokio::test]
async fn compaction_uses_the_same_request_budget_as_execution()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let (mut session, run) = fixture(temp.path(), ModelProtocol::Responses);
    let model = Arc::new(SummaryModel::default());
    let mut engine = runtime(
        temp.path(),
        run,
        model.clone(),
        Arc::new(MemoryStore::default()),
        1,
    )?;
    let result = engine.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::LimitReached);
    let seen = model.0.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].purpose, ModelPurpose::Compaction);
    Ok(())
}

#[tokio::test]
async fn checkpoint_publication_is_idempotent_and_fences_stale_sources_and_pointers()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let (mut session, run) = fixture(temp.path(), ModelProtocol::Responses);
    let run_id = run.id;
    let store = Arc::new(SqliteStore::open(&temp.path().join("sessions"), vec![])?);
    let mut engine = runtime(
        temp.path(),
        run,
        Arc::new(SummaryModel::default()),
        store.clone(),
        64,
    )?;
    assert_eq!(
        engine.compact(&mut session).await?.status,
        RunStatus::Completed
    );
    let checkpoint = store.context_checkpoint(session.id).await?.unwrap();
    let change = |checkpoint| StoreChange::ContextCheckpoint {
        run_id,
        checkpoint: Box::new(checkpoint),
    };
    store.commit(&[change(checkpoint.clone())]).await?;
    let mut stale = checkpoint.clone();
    stale.view.checkpoint_id = "stale_pointer".into();
    stale.record.checkpoint_id = stale.view.checkpoint_id.clone();
    assert!(store.commit(&[change(stale)]).await.is_err());
    assert_eq!(
        store.context_checkpoint(session.id).await?.unwrap(),
        checkpoint
    );
    session.history.push(HistoryItem::UserText(
        "External current history advanced".into(),
    ));
    store
        .commit(&[StoreChange::History {
            session_id: session.id,
            item: session.history.last().unwrap().clone(),
        }])
        .await?;
    assert!(store.commit(&[change(checkpoint.clone())]).await.is_err());
    assert_eq!(
        store.context_checkpoint(session.id).await?.unwrap(),
        checkpoint
    );
    Ok(())
}

fn global_store(root: &Path) -> Arc<saya_core::global_memory::GlobalMemoryStore> {
    use saya_core::{
        credentials::SecretRegistry,
        global_memory::{GlobalMemoryStore, MemoryLimits, WorkspaceIdentity},
    };
    Arc::new(
        GlobalMemoryStore::open(
            &root.join(".saya"),
            WorkspaceIdentity::from_path(root).unwrap(),
            MemoryLimits::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    )
}
fn seed_memory(memory: &saya_core::global_memory::GlobalMemoryStore) {
    use saya_core::global_memory::*;
    let source = MemorySource {
        id: "src_color".into(),
        kind: MemorySourceKind::User,
        scope: MemoryScope::Global,
        reference: "user_request_1".into(),
        revision: saya_core::memory_context::digest(b"source"),
        observed_at: "not_recorded".into(),
        description: "Explicit user preference".into(),
        metadata_withheld: false,
    };
    let entry = MemoryEntry {
        id: "mem_color".into(),
        title: "Color preference".into(),
        scope: MemoryScope::Global,
        updated_at: "not_recorded".into(),
        resources: vec![],
        facts: vec![MemoryFact {
            id: "fact_color".into(),
            kind: MemoryKind::ExplicitPreference,
            text: "Prefer the color ultraviolet.".into(),
            sources: vec![MemorySourceRef {
                source_id: source.id.clone(),
                revision: source.revision.clone(),
            }],
            aliases: vec![],
            derived_from: vec![],
        }],
    };
    let proposal = memory
        .propose(
            &memory.snapshot().unwrap().revision,
            "op_seed",
            MemoryChange::Upsert {
                entry,
                sources: vec![source],
            },
        )
        .unwrap();
    assert_eq!(
        memory.commit(&proposal).unwrap().status,
        MemoryCommitStatus::Committed
    );
}

#[tokio::test]
async fn missing_managed_memory_remains_missing_through_the_normal_runtime()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let memory = global_store(temp.path());
    let (mut session, run) = fixture(temp.path(), ModelProtocol::Responses);
    session.history = vec![HistoryItem::UserText("Finish this short task".into())];
    let store = Arc::new(MemoryStore::default());
    let mut engine = runtime(
        temp.path(),
        run,
        Arc::new(SummaryModel::default()),
        store.clone(),
        64,
    )?
    .with_global_memory(memory.clone(), saya_core::config::MemorySettings::default())?;
    assert_eq!(engine.run(&mut session).await?.status, RunStatus::Completed);
    assert!(!memory.root().exists());
    assert!(store.memory_exposures(session.id).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn forgotten_memory_and_derived_assistant_text_are_retired_after_sqlite_resume()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::global_memory::{MemoryChange, MemoryScope};
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let temp = tempfile::tempdir()?;
        let memory = global_store(temp.path());
        seed_memory(&memory);
        let (mut session, run) = fixture(temp.path(), protocol);
        session.history = vec![HistoryItem::UserText("Use my Color preference.".into())];
        let model = Arc::new(SummaryModel(
            Mutex::new(Vec::new()),
            Some("I will use ultraviolet.".into()),
        ));
        let store = Arc::new(SqliteStore::open(&temp.path().join("sessions"), vec![])?);
        let mut engine = runtime(temp.path(), run, model.clone(), store.clone(), 64)?
            .with_global_memory(memory.clone(), saya_core::config::MemorySettings::default())?;
        assert_eq!(engine.run(&mut session).await?.status, RunStatus::Completed);
        assert!(model.0.lock().unwrap().iter().any(|request| {
            serde_json::to_string(&request.history)
                .unwrap()
                .contains("ultraviolet")
        }));
        let records = store.memory_exposures(session.id).await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].range, ItemRange { start: 1, end: 2 });
        let proposal = memory.propose(
            &memory.snapshot()?.revision,
            "op_forget",
            MemoryChange::Forget {
                scope: MemoryScope::Global,
                fact_ids: vec!["fact_color".into()],
            },
        )?;
        memory.commit(&proposal)?;
        let resumed_model = Arc::new(SummaryModel::default());
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: session.work_mode,
            permissions: PermissionMode::Auto,
        };
        let mut engine = runtime(temp.path(), run, resumed_model.clone(), store.clone(), 64)?
            .with_global_memory(memory.clone(), saya_core::config::MemorySettings::default())?;
        assert_eq!(engine.run(&mut session).await?.status, RunStatus::Completed);
        {
            let seen = resumed_model.0.lock().unwrap();
            let request = seen.last().unwrap();
            let projected = serde_json::to_string(&request.history)?;
            assert!(!projected.contains("ultraviolet"), "{projected}");
            assert!(projected.contains("saya_retired_history_group_v1"));
        }
        assert!(
            serde_json::to_string(&store.load_session(session.id).await?.unwrap().history)?
                .contains("ultraviolet"),
            "raw retained history remains auditable"
        );
    }
    Ok(())
}

#[tokio::test]
async fn sixty_five_prior_user_turns_can_be_compacted_without_permanent_verbatim_constraints()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let (mut session, run) = fixture(temp.path(), ModelProtocol::Responses);
    session.history = (0..65)
        .flat_map(|index| {
            [
                HistoryItem::UserText(format!("Earlier request {index}")),
                HistoryItem::AssistantText("Earlier answer".repeat(10)),
            ]
        })
        .collect();
    session
        .history
        .push(HistoryItem::UserText("Continue the latest task".into()));
    let raw = session.history.clone();
    let store = Arc::new(MemoryStore::default());
    let mut engine = runtime(
        temp.path(),
        run,
        Arc::new(SummaryModel::default()),
        store.clone(),
        64,
    )?
    .with_context_budget(ContextBudget::new(128_000, 2048, 12_801))?;
    let result = engine.compact(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed, "{:?}", result.error);
    assert_eq!(session.history, raw);
    assert!(store.context_checkpoint(session.id).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn memory_switches_control_actual_context_and_catalog_in_both_modes()
-> Result<(), Box<dyn std::error::Error>> {
    for mode in [WorkMode::Work, WorkMode::Plan] {
        for read in [false, true] {
            for inject in [false, true] {
                for write in [false, true] {
                    let temp = tempfile::tempdir()?;
                    let memory = global_store(temp.path());
                    seed_memory(&memory);
                    let (mut session, mut run) = fixture(temp.path(), ModelProtocol::Responses);
                    session.history = vec![HistoryItem::UserText("Use my Color preference".into())];
                    session.work_mode = mode;
                    run.work_mode = mode;
                    let model = Arc::new(SummaryModel::default());
                    let mut engine = runtime(
                        temp.path(),
                        run,
                        model.clone(),
                        Arc::new(MemoryStore::default()),
                        64,
                    )?
                    .with_global_memory(
                        memory,
                        saya_core::config::MemorySettings {
                            read,
                            inject,
                            write,
                            ..Default::default()
                        },
                    )?;
                    let result = engine.run(&mut session).await?;
                    assert_eq!(result.status, RunStatus::Completed, "{:?}", result.error);
                    let requests = model.0.lock().unwrap();
                    let request = requests.last().unwrap();
                    assert_eq!(
                        serde_json::to_string(&request.history)?.contains("ultraviolet"),
                        read && inject
                    );
                    let exposed = |name: &str| request.tools.iter().any(|tool| tool.name == name);
                    assert_eq!(exposed("memory_read"), read);
                    assert_eq!(exposed("memory_search"), read);
                    assert_eq!(exposed("memory_evidence"), read || write);
                    assert_eq!(exposed("memory_update"), write && mode != WorkMode::Plan);
                    assert_eq!(exposed("memory_forget"), write && mode != WorkMode::Plan);
                }
            }
        }
    }
    Ok(())
}

struct CounterModel {
    counts: std::sync::atomic::AtomicUsize,
    inner: SummaryModel,
}
impl saya_core::context_builder::RequestCounter for CounterModel {
    fn count(
        &self,
        _: &ModelRequest,
        identity: &saya_core::context_builder::CountingIdentity,
        fingerprint: &str,
    ) -> Result<saya_core::context_builder::CountEvidence, BoundaryError> {
        self.counts
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(saya_core::context_builder::CountEvidence {
            request_fingerprint: fingerprint.into(),
            identity: identity.clone(),
            input_tokens: 100,
            method: saya_core::context_builder::CountMethod::VerifiedTokenizer {
                name: "controlled counter".into(),
                version: "1".into(),
            },
        })
    }
}
impl Model for CounterModel {
    fn request_counter(&self) -> Option<&dyn saya_core::context_builder::RequestCounter> {
        Some(self)
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        self.inner.start(request, cancel)
    }
}
#[tokio::test]
async fn normal_runtime_preserves_the_adapter_counter_through_model_wrappers()
-> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let temp = tempfile::tempdir()?;
        let (mut session, run) = fixture(temp.path(), protocol);
        session.history = vec![HistoryItem::UserText("Use the local counter".into())];
        let model = Arc::new(CounterModel {
            counts: std::sync::atomic::AtomicUsize::new(0),
            inner: SummaryModel::default(),
        });
        let mut engine = runtime(
            temp.path(),
            run,
            model.clone(),
            Arc::new(MemoryStore::default()),
            64,
        )?;
        let result = engine.run(&mut session).await?;
        assert_eq!(result.status, RunStatus::Completed, "{:?}", result.error);
        assert!(model.counts.load(std::sync::atomic::Ordering::Acquire) > 0);
        assert_eq!(model.inner.0.lock().unwrap().len(), 1);
    }
    Ok(())
}

struct MemoryLoopModel {
    store: Arc<saya_core::global_memory::GlobalMemoryStore>,
    phase: std::sync::atomic::AtomicUsize,
    reviews: std::sync::atomic::AtomicUsize,
}
impl Model for MemoryLoopModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let items = if request.purpose == ModelPurpose::Review {
                assert!(request.tools.is_empty());
                self.reviews
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                vec![HistoryItem::AssistantText(r#"{"decision":"allow","reason":"The proposed memory change matches the explicit user request and selected source."}"#.into())]
            } else {
                assert_eq!(request.purpose, ModelPurpose::Execution);
                let phase = self.phase.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                let (name, arguments) = match phase {
                    0 => ("memory_evidence", serde_json::json!({})),
                    1 => (
                        "memory_update",
                        serde_json::json!({"operation_id":"op_runtime_update","expected_revision":self.store.snapshot()?.revision,"scope":"global","title":"Communication preferences","facts":[{"id":"fact_user","kind":"explicit_preference","text":"Prefer concise and accurate explanations.","evidence":[{"kind":"user_message","ordinal":1}]}]}),
                    ),
                    2 => ("memory_read", serde_json::json!({"fact_id":"fact_user"})),
                    3 => ("memory_search", serde_json::json!({"query":"concise"})),
                    4 => (
                        "memory_forget",
                        serde_json::json!({"operation_id":"op_runtime_forget","expected_revision":self.store.snapshot()?.revision,"scope":"global","fact_ids":["fact_user"]}),
                    ),
                    _ => {
                        return Ok(Box::new(Stream(Some(vec![HistoryItem::AssistantText(
                            "Memory task completed.".into(),
                        )]))) as Box<dyn ModelStream>);
                    }
                };
                vec![HistoryItem::ToolCall(saya_core::contract::ToolCall {
                    call_id: format!("memory_loop_{phase}"),
                    name: name.into(),
                    arguments: arguments.to_string(),
                })]
            };
            Ok(Box::new(Stream(Some(items))) as Box<dyn ModelStream>)
        })
    }
}
#[derive(Default)]
struct ApproveAll(std::sync::atomic::AtomicUsize);
impl saya_core::runtime::ApprovalHandler for ApproveAll {
    fn confirm<'a>(
        &'a self,
        _: &'a saya_core::contract::ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async {
            self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            Ok(Some(true))
        })
    }
}
#[tokio::test]
async fn managed_memory_tools_use_real_runtime_approvals_history_and_forget()
-> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        for mode in [WorkMode::Work, WorkMode::Plan] {
            for permissions in [
                PermissionMode::Auto,
                PermissionMode::Manual,
                PermissionMode::Bypass,
            ] {
                let temp = tempfile::tempdir()?;
                let memory = global_store(temp.path());
                let (mut session, mut run) = fixture(temp.path(), protocol);
                session.work_mode = mode;
                run.work_mode = mode;
                run.permissions = permissions;
                session.history = vec![HistoryItem::UserText("Remember that I prefer concise and accurate explanations, then test reading and searching it, then forget it.".into())];
                let approvals = Arc::new(ApproveAll::default());
                let model = Arc::new(MemoryLoopModel {
                    store: memory.clone(),
                    phase: std::sync::atomic::AtomicUsize::new(0),
                    reviews: std::sync::atomic::AtomicUsize::new(0),
                });
                let store = Arc::new(SqliteStore::open(&temp.path().join("sessions"), vec![])?);
                let mut engine = AgentRuntime::new(RuntimeSetup {
                    run,
                    model: model.clone(),
                    store: store.clone(),
                    policy: Arc::new(Policy::new(temp.path(), PermissionConfig::default())?),
                    files: Arc::new(ReadTools::new(
                        temp.path(),
                        &[],
                        false,
                        ReadLimits::default(),
                    )?),
                    cancellation: Arc::new(Never),
                    review: ReviewConfig::new("fixture".into()),
                    exec: ExecConfig::default(),
                    preauthorizations: Preauthorizations::default(),
                    known_secrets: vec![],
                    summary: "Memory task".into(),
                    confirmation: Some(approvals.clone()),
                    limits: RuntimeLimits::default(),
                })?
                .with_context_budget(ContextBudget::new(64_000, 2048, 6401))?
                .with_global_memory(memory.clone(), saya_core::config::MemorySettings::default())?;
                let result = engine.run(&mut session).await?;
                assert_eq!(
                    result.status,
                    RunStatus::Completed,
                    "{protocol:?} {mode:?} {permissions:?} {:?}",
                    result.error
                );
                let results = session
                    .history
                    .iter()
                    .filter_map(|item| match item {
                        HistoryItem::ToolResult { result, .. } => Some(result),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(results.len(), 5);
                if mode == WorkMode::Plan {
                    for index in [1, 4] {
                        assert_eq!(results[index].error.as_ref().unwrap().code, "mode_denied");
                    }
                    assert!(!memory.root().exists());
                } else {
                    for result in &results {
                        assert!(result.ok, "{:?}", result.error);
                    }
                    assert!(memory.snapshot()?.entries.is_empty());
                    assert!(!memory.snapshot()?.forget_revision.is_empty());
                }
                let expected_approvals = if permissions == PermissionMode::Manual {
                    if mode == WorkMode::Plan { 3 } else { 5 }
                } else {
                    0
                };
                assert_eq!(
                    approvals.0.load(std::sync::atomic::Ordering::Acquire),
                    expected_approvals
                );
                assert_eq!(
                    model.reviews.load(std::sync::atomic::Ordering::Acquire),
                    usize::from(permissions == PermissionMode::Auto && mode != WorkMode::Plan) * 2
                );
                assert_eq!(store.load_session(session.id).await?.unwrap(), session);
            }
        }
    }
    Ok(())
}
