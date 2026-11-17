use super::*;
use crate::{
    contract::{ModelProtocol, PermissionMode, RunId, SessionId},
    memory::MemoryStore,
    permissions::PermissionConfig,
    tools::read::ReadLimits,
};

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
struct EndModel;
struct EndStream(bool);
impl Model for EndModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn crate::boundary::ModelStream>> {
        Box::pin(async { Ok(Box::new(EndStream(false)) as Box<dyn crate::boundary::ModelStream>) })
    }
}
impl crate::boundary::ModelStream for EndStream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if std::mem::replace(&mut self.0, true) {
                return Ok(None);
            }
            Ok(Some(ModelUpdate::Finished {
                items: vec![HistoryItem::AssistantText("done".into())],
                usage: None,
            }))
        })
    }
}
struct ModeStore {
    inner: MemoryStore,
    fail: AtomicBool,
    concurrent: Mutex<Option<ModeController>>,
    batches: Mutex<Vec<Vec<StoreChange>>>,
}
impl Store for ModeStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            let mode_activity = changes.iter().any(|change| {
                matches!(
                    change,
                    StoreChange::Activity {
                        payload: EventPayload::ModeChangeRequested { .. }
                            | EventPayload::ModeChanged { .. },
                        ..
                    }
                )
            });
            if mode_activity && self.fail.swap(false, Ordering::AcqRel) {
                return Err(BoundaryError::new(
                    error_codes::STORAGE_ERROR,
                    "controlled mode transaction failure",
                ));
            }
            self.inner.commit(changes).await?;
            self.batches.lock().unwrap().push(changes.to_vec());
            if mode_activity && let Some(modes) = self.concurrent.lock().unwrap().take() {
                modes.request(WorkMode::Code)?;
            }
            Ok(())
        })
    }
}
fn fixture(root: &std::path::Path, store: Arc<ModeStore>) -> (AgentRuntime, Session) {
    let session = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture".into(),
        history: vec![HistoryItem::UserText("fixture".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    let runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: Arc::new(EndModel),
        store,
        policy: Arc::new(Policy::new(root, PermissionConfig::default()).unwrap()),
        files: Arc::new(ReadTools::new(root, &[], false, ReadLimits::default()).unwrap()),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "fixture".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
    .unwrap();
    (runtime, session)
}
fn store(fail: bool) -> Arc<ModeStore> {
    Arc::new(ModeStore {
        inner: MemoryStore::default(),
        fail: AtomicBool::new(fail),
        concurrent: Mutex::new(None),
        batches: Mutex::new(vec![]),
    })
}
struct CleanupFailureModel(Arc<services::RuntimeServices>);
impl Model for CleanupFailureModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn crate::boundary::ModelStream>> {
        self.0.fail(BoundaryError::new(
            error_codes::OUTCOME_UNKNOWN,
            "controlled cleanup failure",
        ));
        EndModel.start(request, cancel)
    }
}
#[tokio::test]
async fn unknown_service_cleanup_persists_a_failed_run_without_releasing_its_guard() {
    let root = tempfile::tempdir().unwrap();
    let store = store(false);
    let (mut runtime, mut session) = fixture(root.path(), store.clone());
    runtime.model = Arc::new(CleanupFailureModel(runtime.services.clone()));
    runtime.modes.hold_transition().unwrap();
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(result.stop_reason.as_deref(), Some("outcome_unknown"));
    assert_eq!(runtime.modes.active_invocations().unwrap(), 1);
    assert!(
        store
            .batches
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|change| {
                matches!(change, StoreChange::RunFinished(saved) if saved == &result)
            })
    );
}
#[tokio::test]
async fn mode_transaction_acknowledges_only_its_snapshot_during_concurrent_request() {
    let root = tempfile::tempdir().unwrap();
    let store = store(false);
    let (mut runtime, mut session) = fixture(root.path(), store.clone());
    runtime.request_mode(WorkMode::Plan).unwrap();
    *store.concurrent.lock().unwrap() = Some(runtime.modes.clone());
    assert_eq!(
        runtime.run(&mut session).await.unwrap().status,
        RunStatus::Completed
    );
    assert_eq!(session.work_mode, WorkMode::Code);
    let batches = store.batches.lock().unwrap();
    let activities = batches
        .iter()
        .flat_map(|batch| batch.iter())
        .filter_map(|change| {
            if let StoreChange::Activity { payload, .. } = change {
                Some(payload.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        activities,
        vec![
            EventPayload::ModeChangeRequested {
                from: WorkMode::Work,
                to: WorkMode::Plan
            },
            EventPayload::ModeChanged {
                from: WorkMode::Work,
                to: WorkMode::Plan
            },
            EventPayload::ModeChangeRequested {
                from: WorkMode::Plan,
                to: WorkMode::Code
            },
            EventPayload::ModeChanged {
                from: WorkMode::Plan,
                to: WorkMode::Code
            },
        ]
    );
    for batch in batches.iter().filter(|batch| {
        batch
            .iter()
            .any(|change| matches!(change, StoreChange::Activity { .. }))
    }) {
        assert!(matches!(batch.first(), Some(StoreChange::Session(_))));
    }
    assert!(runtime.modes.event_snapshot().unwrap().1.is_empty());
}
#[tokio::test]
async fn failed_mode_transaction_retains_journal_and_session_until_successful_commit() {
    let root = tempfile::tempdir().unwrap();
    let store = store(true);
    let (mut runtime, mut session) = fixture(root.path(), store.clone());
    runtime.request_mode(WorkMode::Plan).unwrap();
    assert_eq!(
        runtime.run(&mut session).await.unwrap_err().0.code,
        "storage_error"
    );
    assert_eq!(session.work_mode, WorkMode::Work);
    assert_eq!(runtime.modes.event_snapshot().unwrap().1.len(), 2);
    runtime.sync_mode(&mut session).await.unwrap();
    assert_eq!(session.work_mode, WorkMode::Plan);
    assert!(runtime.modes.event_snapshot().unwrap().1.is_empty());
    assert_eq!(
        store
            .inner
            .load_session(session.id)
            .await
            .unwrap()
            .unwrap()
            .work_mode,
        WorkMode::Plan
    );
}
