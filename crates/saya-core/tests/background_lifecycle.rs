use saya_core::{
    approvals::{ApprovalSetup, Authorization, PermissionRevision, Preauthorizations},
    background::{BackgroundCancelReason, BackgroundEventKind, BackgroundJobs, JobStatus},
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, Operation, Store, StoreChange, ToolContext,
    },
    contract::{
        BoundaryError, ErrorInfo, ModelProtocol, PermissionMode, Run, RunId, Session, SessionId,
        ToolCall, WorkMode,
    },
    dispatch::{DispatchReview, ToolDispatcher},
    memory::MemoryStore,
    mode::ModeController,
    permissions::{PermissionConfig, Policy},
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct UnusedModel;
impl Model for UnusedModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async { Err(failure("unexpected_review")) })
    }
}
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
struct PauseStore {
    inner: MemoryStore,
    pause: AtomicBool,
    entered: Notify,
    release: Notify,
    completed: AtomicBool,
    fail: bool,
    on_finish: bool,
}
impl Store for PauseStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            if changes.iter().any(|change| match change {
                StoreChange::ToolIntent { .. } => !self.on_finish,
                StoreChange::ToolFinished { .. } => self.on_finish,
                _ => false,
            }) && self.pause.swap(false, Ordering::AcqRel)
            {
                self.entered.notify_one();
                self.release.notified().await;
                self.completed.store(true, Ordering::Release);
                if self.fail {
                    return Err(failure("storage_error"));
                }
            }
            self.inner.commit(changes).await
        })
    }
}
async fn dispatcher(
    root: &std::path::Path,
    store: Arc<dyn Store>,
) -> (Arc<ToolDispatcher>, ToolContext, ModeController) {
    let session = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture".into(),
        history: vec![],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Bypass,
    };
    store
        .commit(&[
            StoreChange::Session(session),
            StoreChange::RunStarted(run.clone()),
        ])
        .await
        .unwrap();
    let context = ToolContext {
        run: run.clone(),
        workspace: root.into(),
    };
    let modes = ModeController::new(WorkMode::Work);
    let dispatcher = ToolDispatcher::new(
        Arc::new(Policy::new(root, PermissionConfig::default()).unwrap()),
        modes.clone(),
        PermissionRevision::new(modes.clone(), PermissionMode::Bypass),
        store,
        ApprovalSetup {
            run_id: run.id,
            session_id: run.session_id,
            initial_permissions: PermissionMode::Bypass,
            preauthorizations: Preauthorizations::default(),
            known_secrets: vec![],
            cancellation: Arc::new(Never),
        },
        Arc::new(ReadTools::new(root, &[], true, ReadLimits::default()).unwrap()),
        ExecConfig::default(),
    )
    .unwrap();
    (Arc::new(dispatcher), context, modes)
}
async fn permit(
    dispatcher: &ToolDispatcher,
    call: &ToolCall,
    context: &ToolContext,
) -> Arc<saya_core::approvals::OneShotPermit> {
    let budget = SharedBudget::default();
    let config = ReviewConfig::new("fixture".into());
    let result = dispatcher
        .authorize(
            call,
            context,
            "fixture",
            false,
            DispatchReview {
                model: &UnusedModel,
                budget: &budget,
                config: &config,
            },
        )
        .await
        .unwrap();
    let Authorization::Permit(permit) = result else {
        panic!("expected fixture permit")
    };
    permit
}

#[tokio::test]
async fn accepted_but_unpolled_mutation_is_cancelled_before_plan_applies() {
    for name in ["write_file", "exec"] {
        let root = tempfile::tempdir().unwrap();
        let (dispatcher, context, modes) =
            dispatcher(root.path(), Arc::new(MemoryStore::default())).await;
        let jobs = BackgroundJobs::new(dispatcher.clone(), 1, 2).unwrap();
        let arguments = if name == "write_file" {
            serde_json::json!({"path":"forbidden", "content":"must not write", "expected_version":null})
        } else {
            serde_json::json!({"argv":["/usr/bin/touch","forbidden"]})
        };
        let call = ToolCall {
            call_id: format!("unpolled-{name}"),
            name: name.into(),
            arguments: arguments.to_string(),
        };
        let permit = permit(&dispatcher, &call, &context).await;
        let id = jobs.start(call, context, permit).unwrap();
        jobs.request_mode(WorkMode::Plan).unwrap();
        assert_eq!(modes.snapshot().unwrap().current, WorkMode::Work);
        assert_eq!(modes.snapshot().unwrap().pending, Some(WorkMode::Plan));
        let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let code = match status {
            JobStatus::Finished(result) => result.error.unwrap().code,
            JobStatus::Failed(error) => error.0.code,
            JobStatus::Running => panic!("job did not finish"),
        };
        assert_eq!(code, "cancelled");
        assert_eq!(modes.snapshot().unwrap().current, WorkMode::Plan);
        assert!(!root.path().join("forbidden").exists());
        jobs.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn non_exec_timeout_waits_for_inflight_store_and_cleanup_before_releasing_lease() {
    for fail in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(PauseStore {
            inner: MemoryStore::default(),
            pause: AtomicBool::new(true),
            entered: Notify::new(),
            release: Notify::new(),
            completed: AtomicBool::new(false),
            fail,
            on_finish: false,
        });
        let (dispatcher, context, modes) = dispatcher(root.path(), store.clone()).await;
        let jobs = BackgroundJobs::new(dispatcher.clone(), 1, 2)
            .unwrap()
            .with_tool_timeout(Duration::from_millis(20))
            .unwrap();
        let call = ToolCall {
            call_id: "timeout-read".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path":"input.txt"}).to_string(),
        };
        std::fs::write(root.path().join("input.txt"), "data").unwrap();
        let permit = permit(&dispatcher, &call, &context).await;
        let id = jobs.start(call, context, permit).unwrap();
        store.entered.notified().await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !jobs.events().unwrap().iter().any(|event| {
                matches!(
                    event.kind,
                    BackgroundEventKind::CancellationRequested {
                        reason: BackgroundCancelReason::Timeout
                    }
                )
            }) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            jobs.status(&id).unwrap(),
            Some(JobStatus::Running)
        ));
        assert!(!store.completed.load(Ordering::Acquire));
        jobs.request_mode(WorkMode::Plan).unwrap();
        assert_eq!(modes.snapshot().unwrap().current, WorkMode::Work);
        store.release.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let JobStatus::Failed(error) = result else {
            panic!("expected timeout/failure")
        };
        assert_eq!(
            error.0.code,
            if fail {
                "storage_error"
            } else {
                "tool_timeout"
            }
        );
        assert!(store.completed.load(Ordering::Acquire));
        assert_eq!(modes.snapshot().unwrap().current, WorkMode::Plan);
        assert_eq!(
            jobs.fatal_error().unwrap().map(|error| error.code),
            if fail {
                Some("storage_error".into())
            } else {
                None
            }
        );
        jobs.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn timeout_after_file_commit_retains_observed_success_and_waits_for_result_save() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(PauseStore {
        inner: MemoryStore::default(),
        pause: AtomicBool::new(true),
        entered: Notify::new(),
        release: Notify::new(),
        completed: AtomicBool::new(false),
        fail: false,
        on_finish: true,
    });
    let (dispatcher, context, modes) = dispatcher(root.path(), store.clone()).await;
    let jobs = BackgroundJobs::new(dispatcher.clone(), 1, 2)
        .unwrap()
        .with_tool_timeout(Duration::from_millis(20))
        .unwrap();
    let call = ToolCall {
        call_id: "committed-before-timeout".into(), name: "write_file".into(),
        arguments: serde_json::json!({"path":"written.txt","content":"actual data","expected_version":null}).to_string(),
    };
    let permit = permit(&dispatcher, &call, &context).await;
    let id = jobs.start(call, context, permit).unwrap();
    store.entered.notified().await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("written.txt")).unwrap(),
        "actual data"
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while !jobs.events().unwrap().iter().any(|event| {
            matches!(
                event.kind,
                BackgroundEventKind::CancellationRequested {
                    reason: BackgroundCancelReason::Timeout
                }
            )
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        jobs.status(&id).unwrap(),
        Some(JobStatus::Running)
    ));
    assert!(!store.completed.load(Ordering::Acquire));
    jobs.request_mode(WorkMode::Plan).unwrap();
    assert_eq!(modes.snapshot().unwrap().current, WorkMode::Work);
    store.release.notify_one();
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Failed(error) = status else {
        panic!("expected deadline status")
    };
    assert_eq!(error.0.code, "tool_timeout");
    assert!(error.2.unwrap().ok);
    assert!(store.completed.load(Ordering::Acquire));
    assert_eq!(modes.snapshot().unwrap().current, WorkMode::Plan);
    assert_eq!(
        std::fs::read_to_string(root.path().join("written.txt")).unwrap(),
        "actual data"
    );
    jobs.shutdown().await.unwrap();
}

#[tokio::test]
async fn prior_user_cancellation_keeps_its_reason_when_cleanup_crosses_the_deadline() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(PauseStore {
        inner: MemoryStore::default(),
        pause: AtomicBool::new(true),
        entered: Notify::new(),
        release: Notify::new(),
        completed: AtomicBool::new(false),
        fail: false,
        on_finish: false,
    });
    let (dispatcher, context, _) = dispatcher(root.path(), store.clone()).await;
    let jobs = BackgroundJobs::new(dispatcher.clone(), 1, 2)
        .unwrap()
        .with_tool_timeout(Duration::from_millis(20))
        .unwrap();
    let call = ToolCall {
        call_id: "cancel-before-deadline".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path":"input.txt"}).to_string(),
    };
    let permit = permit(&dispatcher, &call, &context).await;
    let id = jobs.start(call, context, permit).unwrap();
    store.entered.notified().await;
    assert!(jobs.cancel(&id).unwrap());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(matches!(
        jobs.status(&id).unwrap(),
        Some(JobStatus::Running)
    ));
    store.release.notify_one();
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Failed(error) = status else {
        panic!("expected cancellation")
    };
    assert_eq!(error.0.code, "cancelled");
    assert!(!jobs.events().unwrap().iter().any(|event| matches!(
        event.kind,
        BackgroundEventKind::CancellationRequested {
            reason: BackgroundCancelReason::Timeout
        }
    )));
    assert!(jobs.fatal_error().unwrap().is_none());
    jobs.shutdown().await.unwrap();
}
