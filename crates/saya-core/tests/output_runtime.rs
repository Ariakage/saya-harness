use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store},
    contract::{
        BoundaryError, HistoryItem, ModelProtocol, PermissionMode, Run, RunId, RunStatus, Session,
        SessionId, ToolCall, WorkMode,
    },
    permissions::{PermissionConfig, Policy},
    review::ReviewConfig,
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    collections::VecDeque,
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
};

struct NoCancel;
impl Cancellation for NoCancel {
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
        Box::pin(async move {
            Ok(self
                .0
                .take()
                .map(|items| ModelUpdate::Finished { items, usage: None }))
        })
    }
}
#[allow(dead_code)]
struct FakeModel {
    turns: Mutex<VecDeque<Vec<HistoryItem>>>,
    seen: Mutex<Vec<ModelRequest>>,
}
#[allow(dead_code)]
impl FakeModel {
    fn new(turns: Vec<Vec<HistoryItem>>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            seen: Mutex::new(vec![]),
        }
    }
}
impl Model for FakeModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.seen
                .lock()
                .map_err(|_| err("test"))?
                .push(request.clone());
            let items = self
                .turns
                .lock()
                .map_err(|_| err("test"))?
                .pop_front()
                .ok_or_else(|| err("unexpected_request"))?;
            Ok(Box::new(Stream(Some(items))) as Box<dyn ModelStream>)
        })
    }
}
fn err(code: &str) -> BoundaryError {
    BoundaryError(
        saya_core::contract::ErrorInfo {
            code: code.into(),
            message: code.into(),
        },
        None,
        None,
    )
}
fn fixture(root: &Path, permissions: PermissionMode) -> (Session, Run) {
    let session = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "fake".into(),
        protocol: ModelProtocol::Responses,
        model: "fake".into(),
        history: vec![HistoryItem::UserText("task".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions,
    };
    (session, run)
}
fn call(id: &str, name: &str, args: serde_json::Value) -> HistoryItem {
    HistoryItem::ToolCall(ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments: args.to_string(),
    })
}
fn runtime(
    root: &Path,
    run: Run,
    model: Arc<dyn Model>,
    store: Arc<dyn Store>,
    cancel: Arc<dyn Cancellation>,
) -> Result<AgentRuntime, BoundaryError> {
    let policy = Arc::new(Policy::new(root, PermissionConfig::default())?);
    let files =
        Arc::new(ReadTools::new(root, &[], false, ReadLimits::default()).map_err(|_| err("test"))?);
    AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store,
        policy,
        files,
        cancellation: cancel,
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
}

use saya_core::{
    boundary::StoreChange,
    credentials::SecretRegistry,
    memory::MemoryStore,
    output_store::{CaptureCompletion, OutputLimits, OutputScope, OutputStore, OutputStream},
    sqlite::SqliteStore,
};
use serde_json::json;
use std::collections::BTreeSet;

struct OutputJourney {
    step: Mutex<u8>,
    outputs: Arc<OutputStore>,
    store: Arc<dyn Store>,
}
fn result_data(request: &ModelRequest, id: &str) -> serde_json::Value {
    request
        .history
        .iter()
        .rev()
        .find_map(|item| match item {
            HistoryItem::ToolResult { call_id, result } if call_id == id => {
                if id != "produce" {
                    assert!(result.ok, "{result:?}");
                }
                result.data.clone()
            }
            _ => None,
        })
        .expect("preceding tool result")
}
impl Model for OutputJourney {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let step = {
                let mut step = self.step.lock().unwrap();
                let current = *step;
                *step += 1;
                current
            };
            let items = match step {
                0 => {
                    assert!(request.tools.iter().any(|tool| tool.name == "output_read"));
                    vec![call(
                        "produce",
                        "exec",
                        json!({"argv":["/usr/bin/awk", "BEGIN {for(i=0;i<6000;i++) {printf \"row-%05d payload payload payload\\n\",i; printf \"warn-%05d payload payload payload\\n\",i > \"/dev/stderr\";} print \"needle-end\"; exit 7}"]}),
                    )]
                }
                1 => {
                    let data = result_data(request, "produce");
                    let metadata = &data["output_refs"][0];
                    assert_eq!(metadata["scope"]["stream"], "stdout");
                    assert_eq!(metadata["capture_limit_bytes"], 65536);
                    assert_eq!(metadata["artifact_complete"], false);
                    assert!(
                        metadata["gaps"]
                            .as_array()
                            .is_some_and(|gaps| !gaps.is_empty())
                    );
                    assert_eq!(data["exit_code"], 7);
                    assert_eq!(data["timed_out"], false);
                    assert_eq!(data["cancelled"], false);
                    assert!(data.get("stdout").is_none());
                    assert!(data["projection"].is_string());
                    let registered = self
                        .store
                        .output_references(
                            metadata["scope"]["session_id"]
                                .as_str()
                                .unwrap()
                                .parse::<uuid::Uuid>()
                                .map(SessionId)
                                .unwrap(),
                        )
                        .await?;
                    assert_eq!(registered.len(), 2);
                    assert_eq!(
                        self.outputs.collect_garbage(&BTreeSet::new(), 16)?.leased,
                        2
                    );
                    vec![call(
                        "locate",
                        "output_search",
                        json!({"output_ref":metadata["output_ref"],"pattern":"needle-end","max_bytes":256,"context_lines":1}),
                    )]
                }
                2 => {
                    let data = result_data(request, "locate");
                    assert!(
                        data["matches"][0]["text"]
                            .as_str()
                            .unwrap()
                            .contains("needle-end")
                    );
                    vec![call(
                        "page",
                        "output_read",
                        json!({"output_ref":data["metadata"]["output_ref"],"cursor":data["matches"][0]["cursor"],"max_bytes":256}),
                    )]
                }
                3 => {
                    let data = result_data(request, "page");
                    assert!(
                        data["chunks"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|chunk| chunk["text"].as_str().unwrap().contains("needle-end"))
                    );
                    vec![HistoryItem::AssistantText(
                        "Found the retained tail, with an explicit capture gap.".into(),
                    )]
                }
                _ => return Err(err("unexpected_request")),
            };
            Ok(Box::new(Stream(Some(items))) as Box<dyn ModelStream>)
        })
    }
}

#[tokio::test]
async fn command_output_commits_before_model_retrieval_and_stays_leased_for_run()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let (mut session, run) = fixture(&workspace, PermissionMode::Bypass);
    let store = Arc::new(SqliteStore::open(&root.path().join("data"), vec![])?);
    store.create_session(&session, "fixture").await?;
    let outputs = Arc::new(OutputStore::open(
        root.path().join("outputs"),
        OutputLimits {
            preview_bytes: 8192,
            ..OutputLimits::default()
        },
        Arc::new(SecretRegistry::default()),
    )?);
    let model = Arc::new(OutputJourney {
        step: Mutex::new(0),
        outputs: outputs.clone(),
        store: store.clone(),
    });
    let mut runtime = runtime(
        &workspace,
        run,
        model.clone(),
        store.clone(),
        Arc::new(NoCancel),
    )?
    .with_output_store(outputs.clone())?
    .with_context_budget(saya_core::context_builder::ContextBudget::new(
        200_000, 8192, 20_001,
    ))?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    assert_eq!(*model.step.lock().unwrap(), 4);
    assert_eq!(
        store.load_session(session.id).await?.unwrap().history,
        session.history
    );
    let registered = store.output_references(session.id).await?;
    let wrong = outputs.read(
        SessionId::new(),
        &registered[0].output_ref,
        Default::default(),
    );
    assert_eq!(
        wrong.expect_err("cross-session output").0.code,
        "output_denied"
    );
    drop(runtime);
    assert_eq!(outputs.collect_garbage(&BTreeSet::new(), 16)?.removed, 2);
    assert_eq!(store.output_references(session.id).await?.len(), 2);
    assert_eq!(
        outputs
            .read(session.id, &registered[0].output_ref, Default::default())
            .expect_err("collected output")
            .0
            .code,
        "output_unavailable"
    );
    Ok(())
}

#[tokio::test]
async fn publication_recovers_only_atomic_host_records_and_query_obeys_manual_and_plan()
-> Result<(), Box<dyn std::error::Error>> {
    for permissions in [PermissionMode::Bypass, PermissionMode::Manual] {
        let root = tempfile::tempdir()?;
        let (mut session, mut producer) = fixture(root.path(), permissions);
        session.work_mode = WorkMode::Plan;
        producer.work_mode = WorkMode::Plan;
        let store = Arc::new(SqliteStore::open(&root.path().join("data"), vec![])?);
        store.create_session(&session, "fixture").await?;
        store
            .commit(&[
                StoreChange::RunStarted(producer.clone()),
                StoreChange::ToolIntent {
                    run_id: producer.id,
                    call: ToolCall {
                        call_id: "source".into(),
                        name: "read_file".into(),
                        arguments: json!({"path":"x"}).to_string(),
                    },
                },
            ])
            .await?;
        let outputs = Arc::new(OutputStore::open(
            root.path().join("outputs"),
            OutputLimits::default(),
            Arc::new(SecretRegistry::default()),
        )?);
        let mut capture = outputs.begin(
            OutputScope {
                session_id: session.id,
                run_id: producer.id,
                call_id: "source".into(),
                stream: OutputStream::Text,
            },
            100,
        )?;
        capture.push(b"durable source text")?;
        let sealed = capture.seal(CaptureCompletion::Eof)?;
        let tool_result = saya_core::contract::ToolResult {
            ok: true,
            data: Some(json!({"output_refs":[sealed.metadata]})),
            error: None,
            truncated: false,
        };
        assert!(
            store
                .commit(&[StoreChange::OutputReferences {
                    run_id: producer.id,
                    call_id: "source".into(),
                    outputs: vec![sealed.metadata.clone()]
                }])
                .await
                .is_err()
        );
        assert!(store.output_references(session.id).await?.is_empty());
        store
            .commit(&[
                StoreChange::ToolFinished {
                    run_id: producer.id,
                    call_id: "source".into(),
                    result: tool_result,
                },
                StoreChange::OutputReferences {
                    run_id: producer.id,
                    call_id: "source".into(),
                    outputs: vec![sealed.metadata.clone()],
                },
            ])
            .await?;
        let run = Run {
            id: RunId::new(),
            ..producer
        };
        let model = Arc::new(FakeModel::new(vec![
            vec![call(
                "query",
                "output_read",
                json!({"output_ref":sealed.metadata.output_ref}),
            )],
            vec![HistoryItem::AssistantText("done".into())],
        ]));
        let mut runtime = runtime(root.path(), run, model, store, Arc::new(NoCancel))?
            .with_output_store(outputs.clone())?;
        let result = runtime.run(&mut session).await?;
        assert_eq!(
            result.status,
            if permissions == PermissionMode::Manual {
                RunStatus::NeedsConfirmation
            } else {
                RunStatus::Completed
            },
            "{result:?}"
        );
        assert!(
            outputs
                .describe(session.id, &sealed.metadata.output_ref)?
                .artifact_complete
        );
    }
    Ok(())
}

#[test]
fn output_deny_and_manual_precede_readonly_metadata_default()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::permissions::{Decision, Operation};
    let root = tempfile::tempdir()?;
    let policy = Policy::new(
        root.path(),
        PermissionConfig::from_toml("[[rules]]\neffect='deny'\ntool='output_search'\n")?,
    )?;
    let operation = Operation::output_query("output_search")?;
    for mode in [
        WorkMode::Work,
        WorkMode::Code,
        WorkMode::Plan,
        WorkMode::Build,
    ] {
        for permissions in [
            PermissionMode::Auto,
            PermissionMode::Bypass,
            PermissionMode::Manual,
        ] {
            assert_eq!(
                policy.evaluate(mode, permissions, &operation).decision,
                Decision::Deny
            );
        }
    }
    let policy = Policy::new(root.path(), PermissionConfig::default())?;
    assert_eq!(
        policy
            .evaluate(WorkMode::Plan, PermissionMode::Manual, &operation)
            .decision,
        Decision::Ask
    );
    assert_eq!(
        policy
            .evaluate(WorkMode::Plan, PermissionMode::Auto, &operation)
            .decision,
        Decision::Allow
    );
    Ok(())
}

struct PublicationFault {
    inner: Arc<SqliteStore>,
    outputs: Arc<OutputStore>,
    quarantine: std::path::PathBuf,
    injected: std::sync::atomic::AtomicBool,
}
impl Store for PublicationFault {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn output_references(
        &self,
        id: SessionId,
    ) -> Operation<'_, Vec<saya_core::output_store::OutputMetadata>> {
        self.inner.output_references(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            self.inner.commit(changes).await?;
            if changes
                .iter()
                .any(|change| matches!(change, StoreChange::OutputReferences { .. }))
                && !self
                    .injected
                    .swap(true, std::sync::atomic::Ordering::AcqRel)
            {
                for entry in std::fs::read_dir(self.outputs.protected_path()).unwrap() {
                    let entry = entry.unwrap();
                    if entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "out")
                    {
                        std::fs::rename(entry.path(), self.quarantine.join(entry.file_name()))
                            .unwrap();
                    }
                }
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn failed_publication_preserves_observed_result_and_recovery_never_replays_command()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("workspace");
    let quarantine = root.path().join("quarantine");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(&quarantine)?;
    let (mut session, run) = fixture(&workspace, PermissionMode::Bypass);
    let durable = Arc::new(SqliteStore::open(&root.path().join("data"), vec![])?);
    durable.create_session(&session, "fixture").await?;
    let outputs = Arc::new(OutputStore::open(
        root.path().join("outputs"),
        OutputLimits::default(),
        Arc::new(SecretRegistry::default()),
    )?);
    let store = Arc::new(PublicationFault {
        inner: durable.clone(),
        outputs: outputs.clone(),
        quarantine: quarantine.clone(),
        injected: Default::default(),
    });
    let model = Arc::new(FakeModel::new(vec![vec![call(
        "once",
        "exec",
        json!({"argv":["/bin/sh","-c","printf x >> marker; printf done"]}),
    )]]));
    let mut running = runtime(&workspace, run.clone(), model, store, Arc::new(NoCancel))?
        .with_output_store(outputs.clone())?;
    let error = running
        .run(&mut session)
        .await
        .expect_err("publication fault");
    assert_eq!(error.0.code, "storage_error");
    assert!(error.2.is_some_and(|result| result.ok));
    let mut saved = durable.load_session(session.id).await?.unwrap();
    assert!(
        matches!(saved.history.last(), Some(HistoryItem::ToolResult {call_id,result}) if call_id == "once" && result.ok)
    );
    assert_eq!(std::fs::read(workspace.join("marker"))?, b"x");
    for entry in std::fs::read_dir(&quarantine)? {
        let entry = entry?;
        std::fs::rename(
            entry.path(),
            outputs.protected_path().join(entry.file_name()),
        )?;
    }
    drop(running);
    let model = Arc::new(FakeModel::new(vec![vec![HistoryItem::AssistantText(
        "Recovered saved output.".into(),
    )]]));
    let mut resumed = runtime(
        &workspace,
        Run {
            id: RunId::new(),
            ..run
        },
        model,
        durable.clone(),
        Arc::new(NoCancel),
    )?
    .with_output_store(outputs.clone())?;
    assert_eq!(resumed.run(&mut saved).await?.status, RunStatus::Completed);
    assert_eq!(std::fs::read(workspace.join("marker"))?, b"x");
    for metadata in durable.output_references(saved.id).await? {
        outputs.describe(saved.id, &metadata.output_ref)?;
    }
    Ok(())
}

#[tokio::test]
async fn oversized_required_user_context_stops_before_any_model_request()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let (mut session, run) = fixture(root.path(), PermissionMode::Bypass);
    session.history = vec![HistoryItem::UserText("不可丢弃的用户要求".repeat(5000))];
    let original = session.history.clone();
    let store = Arc::new(MemoryStore::default());
    let model = Arc::new(FakeModel::new(vec![]));
    let mut running = runtime(root.path(), run, model.clone(), store, Arc::new(NoCancel))?
        .with_context_budget(saya_core::context_builder::ContextBudget::new(
            32_000, 8192, 1024,
        ))?;
    let result = running.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::LimitReached, "{result:?}");
    assert_eq!(
        result.error.as_ref().map(|error| error.code.as_str()),
        Some("context_limit")
    );
    assert!(model.seen.lock().unwrap().is_empty());
    assert_eq!(session.history, original);
    Ok(())
}
