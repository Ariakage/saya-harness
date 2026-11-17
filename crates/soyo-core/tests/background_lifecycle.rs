use soyo_core::{
    approvals::{ApprovalSetup, Authorization, PermissionRevision, Preauthorizations},
    background::{
        BackgroundCancelReason, BackgroundEventKind, BackgroundJobs, BackgroundToolKind,
        BackgroundTools, JobStatus,
    },
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, Operation, Store, StoreChange, Tool,
        ToolContext,
    },
    contract::{
        BoundaryError, ErrorInfo, ModelProtocol, PermissionMode, Run, RunId, Session, SessionId,
        ToolCall, ToolResult, WorkMode,
    },
    credentials::SecretRegistry,
    dispatch::{DispatchReview, ToolDispatcher},
    memory::MemoryStore,
    mode::ModeController,
    output_store::{OutputLimits, OutputStore},
    permissions::{PermissionConfig, Policy},
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::collections::BTreeSet;
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
                StoreChange::ToolFinished { result, .. } => self.on_finish && result.ok,
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
    dispatcher_with_secrets(root, store, Vec::new()).await
}
async fn dispatcher_with_secrets(
    root: &std::path::Path,
    store: Arc<dyn Store>,
    known_secrets: Vec<String>,
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
            known_secrets,
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
) -> Arc<soyo_core::approvals::OneShotPermit> {
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
        .with_tool_timeout(Duration::from_secs(60))
        .unwrap();
    let call = ToolCall {
        call_id: "committed-before-timeout".into(), name: "write_file".into(),
        arguments: serde_json::json!({"path":"written.txt","content":"actual data","expected_version":null}).to_string(),
    };
    let permit = permit(&dispatcher, &call, &context).await;
    let id = jobs.start(call, context, permit).unwrap();
    tokio::time::timeout(Duration::from_secs(5), store.entered.notified())
        .await
        .expect("successful write did not reach the result-save barrier");
    assert_eq!(
        std::fs::read_to_string(root.path().join("written.txt")).unwrap(),
        "actual data"
    );
    // Pause only after real filesystem work completes, then cross the deadline explicitly.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
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

#[tokio::test]
async fn browser_background_jobs_return_real_ids_and_cancel_as_unknown() {
    let root = tempfile::tempdir().unwrap();
    let (dispatcher, context, _) = dispatcher(root.path(), Arc::new(MemoryStore::default())).await;
    let jobs = Arc::new(
        BackgroundJobs::new(dispatcher.clone(), 2, 4)
            .unwrap()
            .with_tool_timeout(Duration::from_secs(5))
            .unwrap(),
    );

    let completed = Arc::new(AtomicBool::new(false));
    let flag = completed.clone();
    let id = jobs
        .start_browser(
            "launch-call",
            &context,
            "browser_download",
            true,
            move |cancel| {
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    if cancel.is_cancelled() {
                        return Err(failure("cancelled"));
                    }
                    flag.store(true, Ordering::Release);
                    Ok((
                        soyo_core::contract::ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({"source":"untrusted_browser","artifact_ref":"media_fixture"})),
                            error: None,
                            truncated: false,
                            media_refs: vec!["media_fixture".into()],
                        },
                        Vec::new(),
                    ))
                })
            },
        )
        .unwrap();
    assert!(id.starts_with("background-"));
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Finished(result) = status else {
        panic!("expected finished browser job")
    };
    assert!(result.ok);
    assert!(completed.load(Ordering::Acquire));
    assert_eq!(result.media_refs, vec!["media_fixture".to_owned()]);
    assert!(jobs.events().unwrap().iter().any(|event| matches!(
        event.kind,
        BackgroundEventKind::Accepted { ref tool, .. } if tool == "browser_download"
    )));

    // A dispatched browser action cancelled by the supervisor reports a
    // cancelled terminal status and is never replayed.
    let id = jobs
        .start_browser(
            "cancel-call",
            &context,
            "browser_navigate",
            true,
            move |cancel| {
                Box::pin(async move {
                    cancel.cancelled().await;
                    Err(failure("cancelled"))
                })
            },
        )
        .unwrap();
    assert!(jobs.cancel(&id).unwrap());
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Failed(error) = status else {
        panic!("expected cancelled browser job")
    };
    assert_eq!(error.0.code, "cancelled");
    jobs.shutdown().await.unwrap();
}

#[tokio::test]
async fn browser_business_failures_are_job_outcomes_and_never_poison_the_run() {
    let root = tempfile::tempdir().unwrap();
    let (dispatcher, context, _) = dispatcher(root.path(), Arc::new(MemoryStore::default())).await;
    let jobs = Arc::new(
        BackgroundJobs::new(dispatcher.clone(), 2, 4)
            .unwrap()
            .with_tool_timeout(Duration::from_secs(5))
            .unwrap(),
    );

    let id = jobs
        .start_browser(
            "business-failure",
            &context,
            "browser_navigate",
            true,
            move |_cancel| Box::pin(async { Err(failure("network_denied")) }),
        )
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Failed(error) = status else {
        panic!("expected a failed browser job")
    };
    assert_eq!(error.0.code, "network_denied");
    assert!(
        jobs.fatal_error().unwrap().is_none(),
        "a browser business failure must not become a fatal supervisor error"
    );

    // The supervisor remains open for independent work.
    let next = jobs
        .start_browser(
            "after-failure",
            &context,
            "browser_wait",
            false,
            move |_cancel| {
                Box::pin(async {
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({"waited": true})),
                            error: None,
                            truncated: false,
                            media_refs: Vec::new(),
                        },
                        Vec::new(),
                    ))
                })
            },
        )
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&next))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(status, JobStatus::Finished(result) if result.ok));
    jobs.shutdown().await.unwrap();
}

#[tokio::test]
async fn background_download_pin_survives_gc_and_status_keeps_media_refs() {
    let root = tempfile::tempdir().unwrap();
    let (dispatcher, context, _) = dispatcher(root.path(), Arc::new(MemoryStore::default())).await;
    let jobs = Arc::new(
        BackgroundJobs::new(dispatcher.clone(), 2, 4)
            .unwrap()
            .with_tool_timeout(Duration::from_secs(5))
            .unwrap(),
    );
    let outputs = Arc::new(
        OutputStore::open(
            root.path().join("outputs"),
            OutputLimits::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    let sealed = outputs
        .put_file(
            context.run.session_id,
            "text/plain",
            "download.txt",
            b"background download",
        )
        .unwrap();
    let reference = sealed.reference.clone();

    let id = jobs
        .start_browser(
            "download-parent",
            &context,
            "browser_download",
            true,
            move |_cancel| {
                let reference = reference.clone();
                Box::pin(async move {
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({
                                "source": "untrusted_browser",
                                "download": {"artifact_ref": reference.clone()},
                            })),
                            error: None,
                            truncated: false,
                            media_refs: vec![reference],
                        },
                        vec![sealed],
                    ))
                })
            },
        )
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Finished(result) = status else {
        panic!("expected a finished background download")
    };
    assert_eq!(result.media_refs.len(), 1);
    let reference = result.media_refs[0].clone();

    // Nothing has been committed to history yet. The job-held lease must keep
    // the artifact alive across GC.
    let collection = outputs.collect_garbage(&BTreeSet::new(), 8).unwrap();
    assert_eq!(collection.removed, 0);
    let (bytes, _, _) = outputs
        .load_file(context.run.session_id, &reference)
        .expect("background artifact was reclaimed before status/upload");
    assert_eq!(bytes, b"background download");

    let bridge = Arc::new(BackgroundTools::new(
        jobs.clone(),
        Arc::new(UnusedModel),
        SharedBudget::default(),
        ReviewConfig::new("fixture".into()),
        "task".into(),
        false,
    ));
    let status_call = ToolCall {
        call_id: "status-call".into(),
        name: "background_status".into(),
        arguments: serde_json::json!({"job_id": id}).to_string(),
    };
    let response = bridge
        .tool(BackgroundToolKind::Status)
        .execute(&status_call, &context, &Never)
        .await
        .unwrap();
    assert!(response.ok);
    assert_eq!(response.media_refs, vec![reference.clone()]);
    // The model projection of the nested result never carries host refs.
    assert!(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("result"))
            .and_then(|result| result.get("media_refs"))
            .is_none()
    );
    jobs.shutdown().await.unwrap();
}

#[tokio::test]
async fn background_timeout_keeps_a_produced_artifact_pinned_until_the_job_state_drops() {
    let root = tempfile::tempdir().unwrap();
    let (dispatcher, context, _) = dispatcher(root.path(), Arc::new(MemoryStore::default())).await;
    let jobs = Arc::new(
        BackgroundJobs::new(dispatcher.clone(), 1, 2)
            .unwrap()
            .with_tool_timeout(Duration::from_millis(100))
            .unwrap(),
    );
    let outputs = Arc::new(
        OutputStore::open(
            root.path().join("outputs"),
            OutputLimits::default(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    let sealed = outputs
        .put_file(
            context.run.session_id,
            "text/plain",
            "late-download.txt",
            b"late background download",
        )
        .unwrap();
    let reference = sealed.reference.clone();
    let job_reference = reference.clone();
    let id = jobs
        .start_browser(
            "timeout-parent",
            &context,
            "browser_download",
            true,
            move |_cancel| {
                Box::pin(async move {
                    // Cross the supervisor deadline and still complete the
                    // operation. The timeout branch must retain its lease.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({
                                "source": "untrusted_browser",
                                "download": {"artifact_ref": job_reference.clone()},
                            })),
                            error: None,
                            truncated: false,
                            media_refs: vec![job_reference.clone()],
                        },
                        vec![sealed],
                    ))
                })
            },
        )
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(3), jobs.wait(&id))
        .await
        .expect("timed out waiting for the background job")
        .expect("status")
        .expect("job");
    let JobStatus::Failed(error) = status else {
        panic!("expected a timeout outcome: {status:?}")
    };
    assert_eq!(error.0.code, "tool_timeout");
    let observed = error.2.expect("timeout must retain the observed result");
    assert_eq!(observed.media_refs, vec![reference.clone()]);
    // The failed job still pins the artifact so `background_status` and a
    // later upload cannot observe a reclaimed File.
    assert_eq!(
        outputs
            .collect_garbage(&BTreeSet::new(), 8)
            .unwrap()
            .removed,
        0,
        "timeout dropped the artifact lease while the job state still holds it"
    );
    assert!(
        outputs
            .load_file(context.run.session_id, &reference)
            .is_ok()
    );

    jobs.shutdown().await.unwrap();
    drop(jobs);
    // Once the job state is gone the lease is released and GC may reclaim it.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let removed = outputs
            .collect_garbage(&BTreeSet::new(), 8)
            .unwrap()
            .removed;
        if removed == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "artifact lease outlived the job state"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn background_browser_results_are_redacted_before_status_or_persistence() {
    let root = tempfile::tempdir().unwrap();
    let (dispatcher, context, _) = dispatcher_with_secrets(
        root.path(),
        Arc::new(MemoryStore::default()),
        vec!["echo-secret".into()],
    )
    .await;
    let jobs = Arc::new(
        BackgroundJobs::new(dispatcher.clone(), 2, 4)
            .unwrap()
            .with_tool_timeout(Duration::from_secs(5))
            .unwrap(),
    );

    let id = jobs
        .start_browser(
            "redact-finish",
            &context,
            "browser_download",
            true,
            move |_cancel| {
                Box::pin(async {
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({
                                "filename": "echo-secret.txt",
                                "final_url": "https://example.test/?q=echo-secret",
                            })),
                            error: None,
                            truncated: false,
                            media_refs: Vec::new(),
                        },
                        Vec::new(),
                    ))
                })
            },
        )
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Finished(result) = status else {
        panic!("expected a finished browser job")
    };
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(!encoded.contains("echo-secret"), "background result leaked");
    assert!(encoded.contains("[REDACTED]"));

    let id = jobs
        .start_browser(
            "redact-error",
            &context,
            "browser_navigate",
            true,
            move |_cancel| {
                Box::pin(async {
                    Err(BoundaryError(
                        ErrorInfo {
                            code: "provider_error".into(),
                            message: "echoed echo-secret in final URL".into(),
                        },
                        None,
                        None,
                    ))
                })
            },
        )
        .unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Failed(error) = status else {
        panic!("expected a failed browser job")
    };
    assert_eq!(error.0.code, "provider_error");
    assert!(!error.0.message.contains("echo-secret"));
    assert!(error.0.message.contains("[REDACTED]"));
    assert!(jobs.fatal_error().unwrap().is_none());
    jobs.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_tool_business_failures_do_not_close_the_background_supervisor() {
    let root = tempfile::tempdir().unwrap();
    let (dispatcher, context, _) = dispatcher(root.path(), Arc::new(MemoryStore::default())).await;
    let jobs = BackgroundJobs::new(dispatcher.clone(), 2, 4).unwrap();

    let call = ToolCall {
        call_id: "missing-read".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path":"missing.txt"}).to_string(),
    };
    let first_permit = permit(&dispatcher, &call, &context).await;
    let id = jobs.start(call, context.clone(), first_permit).unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), jobs.wait(&id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let failed = match &status {
        JobStatus::Failed(_) => true,
        JobStatus::Finished(result) => !result.ok,
        JobStatus::Running => false,
    };
    assert!(
        failed,
        "missing file did not produce a failed job outcome: {status:?}"
    );
    assert!(
        jobs.fatal_error().unwrap().is_none(),
        "a missing file must not poison every later background job"
    );

    let call = ToolCall {
        call_id: "second-missing-read".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path":"other-missing.txt"}).to_string(),
    };
    let second_permit = permit(&dispatcher, &call, &context).await;
    jobs.start(call, context, second_permit)
        .expect("supervisor remains available after a tool-level failure");
    jobs.shutdown().await.unwrap();
}
