//! Actual runtime/SQLite/dispatcher regression for forgotten output provenance.
use saya_core::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, StoreChange,
    },
    config::MemorySettings,
    context_builder::ContextBudget,
    contract::{
        ApprovalRequest, BoundaryError, ErrorInfo, HistoryItem, ModelProtocol, ModelPurpose,
        PermissionMode, Run, RunId, RunStatus, Session, SessionId, ToolCall, WorkMode,
    },
    credentials::SecretRegistry,
    global_memory::{
        GlobalMemoryStore, MemoryChange, MemoryCommitStatus, MemoryEntry, MemoryFact, MemoryKind,
        MemoryLimits, MemoryScope, MemorySource, MemorySourceKind, MemorySourceRef,
        WorkspaceIdentity,
    },
    output_store::{OutputLimits, OutputStore, OutputStream, ReadOptions},
    permissions::{PermissionConfig, Policy},
    review::ReviewConfig,
    runtime::{AgentRuntime, ApprovalHandler, RuntimeLimits, RuntimeSetup},
    sqlite::SqliteStore,
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

const FORGOTTEN: &str = "Retired ultraviolet project observation";

fn failure(code: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: code.into(),
        },
        None,
        None,
    )
}

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
        Box::pin(async move {
            Ok(self
                .0
                .take()
                .map(|items| ModelUpdate::Finished { items, usage: None }))
        })
    }
}

struct ScriptedModel {
    turns: Mutex<VecDeque<Vec<HistoryItem>>>,
    requests: Mutex<Vec<ModelRequest>>,
}
impl ScriptedModel {
    fn new(turns: Vec<Vec<HistoryItem>>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            requests: Mutex::new(vec![]),
        }
    }
}
impl Model for ScriptedModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            if request.purpose != ModelPurpose::Execution {
                return Err(failure("unexpected_review_request"));
            }
            self.requests.lock().unwrap().push(request.clone());
            let items = self
                .turns
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| failure("unexpected_model_request"))?;
            Ok(Box::new(Stream(Some(items))) as Box<dyn ModelStream>)
        })
    }
}

fn call(id: &str, name: &str, arguments: Value) -> HistoryItem {
    HistoryItem::ToolCall(ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments: arguments.to_string(),
    })
}

fn forget(memory: &GlobalMemoryStore) -> Result<(), BoundaryError> {
    let proposal = memory.propose(
        &memory.snapshot()?.revision,
        "op_forget_output_source",
        MemoryChange::Forget {
            scope: MemoryScope::Global,
            fact_ids: vec!["fact_color".into()],
        },
    )?;
    let outcome = memory.commit(&proposal)?;
    if outcome.status != MemoryCommitStatus::Committed {
        return Err(failure("fixture_forget_incomplete"));
    }
    Ok(())
}

struct Confirmation {
    answer: bool,
    calls: AtomicUsize,
    forget_on_first_approval: Option<Arc<GlobalMemoryStore>>,
}
impl ApprovalHandler for Confirmation {
    fn confirm<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            assert!(matches!(
                request.tool.as_str(),
                "output_read" | "output_search"
            ));
            if self.calls.fetch_add(1, Ordering::AcqRel) == 0
                && let Some(memory) = &self.forget_on_first_approval
            {
                forget(memory)?;
            }
            Ok(Some(self.answer))
        })
    }
}

fn engine(
    workspace: &Path,
    run: Run,
    model: Arc<dyn Model>,
    store: Arc<dyn Store>,
    outputs: Arc<OutputStore>,
    memory: Arc<GlobalMemoryStore>,
    confirmation: Option<Arc<dyn ApprovalHandler>>,
) -> Result<AgentRuntime, BoundaryError> {
    AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store,
        policy: Arc::new(Policy::new(workspace, PermissionConfig::default())?),
        files: Arc::new(
            ReadTools::new(workspace, &[], false, ReadLimits::default())
                .map_err(|_| failure("fixture_read"))?,
        ),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "Verify forgotten output retrieval".into(),
        confirmation,
        limits: RuntimeLimits::default(),
    })?
    .with_context_budget(ContextBudget::new(128_000, 4096, 12_801))?
    .with_output_store(outputs)?
    .with_global_memory(memory, MemorySettings::default())
}

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    sessions: PathBuf,
    memory: Arc<GlobalMemoryStore>,
    outputs: Arc<OutputStore>,
    session: Session,
    output_ref: String,
}
impl Fixture {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace)?;
        let workspace = std::fs::canonicalize(workspace)?;
        let sessions = root.path().join("sessions");
        let secrets = Arc::new(SecretRegistry::default());
        let memory = Arc::new(GlobalMemoryStore::open(
            &root.path().join("personal"),
            WorkspaceIdentity::from_path(&workspace)?,
            MemoryLimits::default(),
            secrets.clone(),
        )?);
        let source = MemorySource {
            id: "src_color".into(),
            kind: MemorySourceKind::User,
            scope: MemoryScope::Global,
            reference: "original_user_request".into(),
            revision: saya_core::memory_context::digest(b"fixture"),
            observed_at: "not_recorded".into(),
            description: "Explicit color preference".into(),
            metadata_withheld: false,
        };
        let proposal = memory.propose(
            &memory.snapshot()?.revision,
            "op_seed_output_memory",
            MemoryChange::Upsert {
                entry: MemoryEntry {
                    id: "mem_color".into(),
                    title: "Color preference".into(),
                    scope: MemoryScope::Global,
                    updated_at: "not_recorded".into(),
                    resources: vec![],
                    facts: vec![MemoryFact {
                        id: "fact_color".into(),
                        kind: MemoryKind::ExplicitPreference,
                        text: FORGOTTEN.into(),
                        sources: vec![MemorySourceRef {
                            source_id: source.id.clone(),
                            revision: source.revision.clone(),
                        }],
                        aliases: vec![],
                        derived_from: vec![],
                    }],
                },
                sources: vec![source],
            },
        )?;
        assert_eq!(
            memory.commit(&proposal)?.status,
            MemoryCommitStatus::Committed
        );
        let outputs = Arc::new(OutputStore::open(
            root.path().join("outputs"),
            OutputLimits::default(),
            secrets,
        )?);
        let store = Arc::new(SqliteStore::open(&sessions, vec![])?);
        let mut session = Session {
            id: SessionId::new(),
            workspace: workspace.clone(),
            work_mode: WorkMode::Work,
            profile: "fixture".into(),
            protocol: ModelProtocol::Responses,
            model: "fixture".into(),
            history: vec![HistoryItem::UserText(
                "Use my Color preference and capture the observation.".into(),
            )],
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: session.work_mode,
            permissions: PermissionMode::Bypass,
        };
        let model = Arc::new(ScriptedModel::new(vec![
            vec![call(
                "observed-output",
                "exec",
                json!({"argv":["/bin/echo",FORGOTTEN]}),
            )],
            vec![HistoryItem::AssistantText("Observation recorded.".into())],
        ]));
        let mut runtime = engine(
            &workspace,
            run,
            model.clone(),
            store.clone(),
            outputs.clone(),
            memory.clone(),
            None,
        )?;
        let result = runtime.run(&mut session).await?;
        assert_eq!(result.status, RunStatus::Completed, "{:?}", result.error);
        assert!(model.requests.lock().unwrap().iter().any(|request| {
            serde_json::to_string(&request.history)
                .unwrap()
                .contains(FORGOTTEN)
        }));
        assert!(!store.memory_exposures(session.id).await?.is_empty());
        let output_ref = store
            .output_references(session.id)
            .await?
            .into_iter()
            .find(|metadata| {
                metadata.scope.call_id == "observed-output"
                    && metadata.scope.stream == OutputStream::Stdout
            })
            .ok_or_else(|| failure("fixture_output_missing"))?
            .output_ref;
        drop(runtime);
        drop(store);
        Ok(Self {
            _root: root,
            workspace,
            sessions,
            memory,
            outputs,
            session,
            output_ref,
        })
    }

    async fn resume(
        &mut self,
        confirmation: Arc<Confirmation>,
    ) -> Result<(Arc<SqliteStore>, Arc<ScriptedModel>), Box<dyn std::error::Error>> {
        let store = Arc::new(SqliteStore::open(&self.sessions, vec![])?);
        self.session = store.load_session(self.session.id).await?.unwrap();
        let user =
            HistoryItem::UserText("Inspect the retained output's current availability.".into());
        store
            .commit(&[StoreChange::History {
                session_id: self.session.id,
                item: user.clone(),
            }])
            .await?;
        self.session.history.push(user);
        let model = Arc::new(ScriptedModel::new(vec![
            vec![call(
                "retrieve-read",
                "output_read",
                json!({"output_ref":self.output_ref}),
            )],
            vec![call(
                "retrieve-search",
                "output_search",
                json!({"output_ref":self.output_ref,"pattern":"Retired"}),
            )],
            vec![HistoryItem::AssistantText(
                "Current output availability checked.".into(),
            )],
        ]));
        let run = Run {
            id: RunId::new(),
            session_id: self.session.id,
            work_mode: self.session.work_mode,
            permissions: PermissionMode::Manual,
        };
        let mut runtime = engine(
            &self.workspace,
            run,
            model.clone(),
            store.clone(),
            self.outputs.clone(),
            self.memory.clone(),
            Some(confirmation),
        )?;
        let result = runtime.run(&mut self.session).await?;
        assert_eq!(result.status, RunStatus::Completed, "{:?}", result.error);
        Ok((store, model))
    }
}

#[tokio::test]
async fn forgotten_output_retrieval_after_sqlite_resume_requires_manual_approval_and_saves_only_withheld_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new().await?;
    forget(&fixture.memory)?;
    let confirmation = Arc::new(Confirmation {
        answer: true,
        calls: AtomicUsize::new(0),
        forget_on_first_approval: None,
    });
    let (store, model) = fixture.resume(confirmation.clone()).await?;
    assert_eq!(confirmation.calls.load(Ordering::Acquire), 2);
    let saved = store.load_session(fixture.session.id).await?.unwrap();
    for id in ["retrieve-read", "retrieve-search"] {
        let result = saved
            .history
            .iter()
            .find_map(|item| match item {
                HistoryItem::ToolResult { call_id, result } if call_id == id => Some(result),
                _ => None,
            })
            .unwrap();
        assert_eq!(result.error.as_ref().unwrap().code, "memory_source_retired");
        assert_eq!(result.data.as_ref().unwrap()["body_withheld"], true);
        assert!(!serde_json::to_string(result)?.contains(FORGOTTEN));
    }
    for request in model.requests.lock().unwrap().iter() {
        assert!(!serde_json::to_string(&request.history)?.contains(FORGOTTEN));
    }
    assert!(serde_json::to_string(&saved.history)?.contains(FORGOTTEN));
    let audit = fixture.outputs.read(
        fixture.session.id,
        &fixture.output_ref,
        ReadOptions::default(),
    )?;
    assert!(
        audit
            .chunks
            .iter()
            .any(|chunk| chunk.text.contains(FORGOTTEN))
    );
    Ok(())
}

#[tokio::test]
async fn forgetting_while_waiting_for_manual_approval_rechecks_live_provenance_before_artifact_read()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new().await?;
    let confirmation = Arc::new(Confirmation {
        answer: true,
        calls: AtomicUsize::new(0),
        forget_on_first_approval: Some(fixture.memory.clone()),
    });
    let (store, _) = fixture.resume(confirmation.clone()).await?;
    assert_eq!(confirmation.calls.load(Ordering::Acquire), 2);
    let saved = store.load_session(fixture.session.id).await?.unwrap();
    let read = saved
        .history
        .iter()
        .find_map(|item| match item {
            HistoryItem::ToolResult { call_id, result } if call_id == "retrieve-read" => {
                Some(result)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(read.data.as_ref().unwrap()["body_withheld"], true);
    assert!(!serde_json::to_string(read)?.contains(FORGOTTEN));
    Ok(())
}

#[tokio::test]
async fn retired_output_provenance_does_not_bypass_manual_denial()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new().await?;
    forget(&fixture.memory)?;
    let confirmation = Arc::new(Confirmation {
        answer: false,
        calls: AtomicUsize::new(0),
        forget_on_first_approval: None,
    });
    let (store, _) = fixture.resume(confirmation.clone()).await?;
    assert_eq!(confirmation.calls.load(Ordering::Acquire), 2);
    let saved = store.load_session(fixture.session.id).await?.unwrap();
    for id in ["retrieve-read", "retrieve-search"] {
        let result = saved
            .history
            .iter()
            .find_map(|item| match item {
                HistoryItem::ToolResult { call_id, result } if call_id == id => Some(result),
                _ => None,
            })
            .unwrap();
        assert_eq!(result.error.as_ref().unwrap().code, "permission_denied");
        assert!(result.data.is_none());
    }
    Ok(())
}

struct ForgetBeforeCheckpointLookup {
    inner: Arc<SqliteStore>,
    memory: Arc<GlobalMemoryStore>,
    triggered: AtomicBool,
}
impl Store for ForgetBeforeCheckpointLookup {
    fn memory_exposures(
        &self,
        id: SessionId,
    ) -> Operation<'_, Vec<saya_core::memory_context::MemoryExposure>> {
        self.inner.memory_exposures(id)
    }
    fn output_references(
        &self,
        id: SessionId,
    ) -> Operation<'_, Vec<saya_core::output_store::OutputMetadata>> {
        self.inner.output_references(id)
    }
    fn context_checkpoint(
        &self,
        id: SessionId,
    ) -> Operation<'_, Option<saya_core::context_store::ContextCheckpoint>> {
        Box::pin(async move {
            if !self.triggered.swap(true, Ordering::AcqRel) {
                forget(&self.memory)?;
            }
            self.inner.context_checkpoint(id).await
        })
    }
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        self.inner.commit(changes)
    }
}

#[derive(Default)]
struct CompactionLeakTrap {
    requests: Mutex<Vec<ModelRequest>>,
}
impl Model for CompactionLeakTrap {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            Err(failure("fixture_stops_after_observing_dispatch"))
        })
    }
}

#[tokio::test]
async fn forget_after_context_snapshot_must_be_checked_before_any_compaction_model_dispatch()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new().await?;
    let store = Arc::new(ForgetBeforeCheckpointLookup {
        inner: Arc::new(SqliteStore::open(&fixture.sessions, vec![])?),
        memory: fixture.memory.clone(),
        triggered: AtomicBool::new(false),
    });
    let model = Arc::new(CompactionLeakTrap::default());
    let run = Run {
        id: RunId::new(),
        session_id: fixture.session.id,
        work_mode: fixture.session.work_mode,
        permissions: PermissionMode::Auto,
    };
    let mut runtime = engine(
        &fixture.workspace,
        run,
        model.clone(),
        store.clone(),
        fixture.outputs.clone(),
        fixture.memory.clone(),
        None,
    )?;
    let _ = runtime.compact(&mut fixture.session).await?;
    assert!(store.triggered.load(Ordering::Acquire));
    for request in model.requests.lock().unwrap().iter() {
        assert!(
            !serde_json::to_string(&request.history)?.contains(FORGOTTEN),
            "A forgotten observation was sent to {:?} before fresh version validation",
            request.purpose,
        );
    }
    Ok(())
}
