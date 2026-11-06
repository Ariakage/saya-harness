mod support;
use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store},
    contract::{
        BoundaryError, HistoryItem, ModelProtocol, PermissionMode, Run, RunId, RunStatus, Session,
        SessionId, ToolCall, WorkMode,
    },
    memory::MemoryStore,
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
struct FakeModel {
    turns: Mutex<VecDeque<Vec<HistoryItem>>>,
    seen: Mutex<Vec<ModelRequest>>,
}
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
#[tokio::test]
async fn model_tool_loop_returns_results_and_persists_once()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("source.txt"), "hello")?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![
        vec![
            call(
                "read",
                "read_file",
                serde_json::json!({"path":"source.txt","start_line":1,"line_count":1}),
            ),
            call(
                "write",
                "write_file",
                serde_json::json!({"path":"out.txt","content":"result","expected_version":null}),
            ),
        ],
        vec![HistoryItem::AssistantText("done".into())],
    ]));
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        store.clone(),
        Arc::new(NoCancel),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(result.text.as_deref(), Some("done"));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("out.txt"))?,
        "result"
    );
    let saved = store
        .load_session(session.id)
        .await?
        .ok_or("session missing")?;
    assert_eq!(saved.history, session.history);
    for id in ["read", "write"] {
        assert_eq!(saved.history.iter().filter(|item| matches!(item,HistoryItem::ToolResult {call_id,result} if call_id==id && result.ok)).count(),1);
    }
    let seen = model.seen.lock().map_err(|_| "lock")?;
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1]
            .history
            .iter()
            .filter(|item| matches!(item, HistoryItem::ToolResult { .. }))
            .count(),
        2
    );
    assert!(seen[0].instructions.contains("# Work mode: WORK"));
    Ok(())
}

#[tokio::test]
async fn personal_context_is_request_only_and_not_stored_as_history()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![vec![HistoryItem::AssistantText(
        "done".into(),
    )]]));
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        store.clone(),
        Arc::new(NoCancel),
    )?
    .with_personal_context(saya_core::runtime::PersonalContext {
        preferences: Some("TEST_PREFERENCES".into()),
        memory: Some("TEST_MEMORY".into()),
    });
    assert_eq!(
        runtime.run(&mut session).await?.status,
        RunStatus::Completed
    );
    {
        let seen = model.seen.lock().map_err(|_| "lock")?;
        let dynamic = seen[0]
            .dynamic_instructions
            .as_ref()
            .ok_or("missing dynamic context")?;
        assert!(dynamic.contains("\"long_term_memory_supplied\":true"));
        assert!(!dynamic.contains("TEST_MEMORY"));
        assert!(serde_json::to_string(&seen[0].history)?.contains("TEST_MEMORY"));
    }
    let saved = store.load_session(session.id).await?.ok_or("missing")?;
    assert!(!serde_json::to_string(&saved.history)?.contains("TEST_MEMORY"));
    Ok(())
}
#[tokio::test]
async fn malformed_write_is_paired_and_model_can_continue() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![
        vec![call(
            "invalid",
            "write_file",
            serde_json::json!({"path":"out.txt","content":"data"}),
        )],
        vec![HistoryItem::AssistantText("handled".into())],
    ]));
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(temp.path(), run, model, store.clone(), Arc::new(NoCancel))?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    assert!(!temp.path().join("out.txt").exists());
    let saved = store.load_session(session.id).await?.ok_or("missing")?;
    assert_eq!(saved.history, session.history);
    assert!(saved.history.iter().any(|item| matches!(item,HistoryItem::ToolResult {call_id,result} if call_id=="invalid" && !result.ok)));
    Ok(())
}
#[tokio::test]
async fn noninteractive_review_stops_before_write() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Manual);
    let model = Arc::new(FakeModel::new(vec![vec![call(
        "write",
        "write_file",
        serde_json::json!({"path":"out.txt","content":"data","expected_version":null}),
    )]]));
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        store.clone(),
        Arc::new(NoCancel),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::NeedsConfirmation);
    assert!(!temp.path().join("out.txt").exists());
    assert_eq!(model.seen.lock().map_err(|_| "lock")?.len(), 1);
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history,
        session.history
    );
    Ok(())
}
#[tokio::test]
async fn empty_completed_model_response_is_not_success() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let mut runtime = runtime(
        temp.path(),
        run,
        Arc::new(FakeModel::new(vec![vec![]])),
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(
        result.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_model_response")
    );
    Ok(())
}

struct SwitchingModel {
    modes: Mutex<Option<saya_core::runtime::RuntimeHandle>>,
    count: std::sync::atomic::AtomicUsize,
}
impl Model for SwitchingModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let count = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                self.modes
                    .lock()
                    .map_err(|_| err("test"))?
                    .as_ref()
                    .ok_or_else(|| err("test"))?
                    .request_mode(WorkMode::Plan)?;
                Ok(Box::new(Stream(Some(vec![call(
                    "write",
                    "write_file",
                    serde_json::json!({"path":"out.txt","content":"x","expected_version":null}),
                )]))) as Box<dyn ModelStream>)
            } else {
                Ok(Box::new(Stream(Some(vec![HistoryItem::AssistantText(
                    "planned".into(),
                )]))) as Box<dyn ModelStream>)
            }
        })
    }
}

#[derive(Clone)]
struct Signal(tokio::sync::watch::Sender<bool>);
impl Signal {
    fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    fn cancel(&self) {
        self.0.send_replace(true);
    }
}
impl Cancellation for Signal {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut rx = self.0.subscribe();
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}
#[tokio::test]
async fn cancel_before_run_does_not_claim_persistence() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let signal = Signal::new();
    signal.cancel();
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(
        temp.path(),
        run,
        Arc::new(FakeModel::new(vec![])),
        store.clone(),
        Arc::new(signal),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Cancelled);
    assert!(!result.persistence.ok_or("missing")?.saved);
    assert!(store.load_session(session.id).await?.is_none());
    Ok(())
}
#[tokio::test]
async fn cancelling_running_command_waits_for_cleanup() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Bypass);
    let marker = temp.path().join("started");
    let model = Arc::new(FakeModel::new(vec![vec![call(
        "cmd",
        "exec",
        serde_json::json!({"argv":["/bin/sh","-c","printf ready > \"$1\"; sleep 30","sh",marker]}),
    )]]));
    let signal = Signal::new();
    let store = Arc::new(MemoryStore::default());
    // bypass paths need unrestricted roots; the workspace is still a temporary fixture.
    let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        true,
        ReadLimits::default(),
    )?);
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store: store.clone(),
        policy,
        files,
        cancellation: Arc::new(signal.clone()),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })?;
    let controller = async {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !marker.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        signal.cancel();
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (result, control) = tokio::join!(runtime.run(&mut session), controller);
    control?;
    let result = result?;
    assert_eq!(result.status, RunStatus::Cancelled);
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history,
        session.history
    );
    Ok(())
}

#[tokio::test]
async fn user_mode_switch_before_dispatch_rechecks_plan_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(SwitchingModel {
        modes: Mutex::new(None),
        count: std::sync::atomic::AtomicUsize::new(0),
    });
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        store.clone(),
        Arc::new(NoCancel),
    )?;
    *model.modes.lock().map_err(|_| "lock")? = Some(runtime.handle());
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(session.work_mode, WorkMode::Plan);
    assert!(!temp.path().join("out.txt").exists());
    assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult {call_id,result} if call_id=="write" && !result.ok)));
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history,
        session.history
    );
    Ok(())
}

struct Approve;
impl saya_core::runtime::ApprovalHandler for Approve {
    fn confirm<'a>(
        &'a self,
        _: &'a saya_core::contract::ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async { Ok(Some(true)) })
    }
}
#[tokio::test]
async fn interactive_confirmation_allows_only_requested_write()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Manual);
    let model = Arc::new(FakeModel::new(vec![
        vec![call(
            "write",
            "write_file",
            serde_json::json!({"path":"out.txt","content":"approved","expected_version":null}),
        )],
        vec![HistoryItem::AssistantText("done".into())],
    ]));
    let store = Arc::new(MemoryStore::default());
    let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store: store.clone(),
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: Some(Arc::new(Approve)),
        limits: RuntimeLimits::default(),
    })?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("out.txt"))?,
        "approved"
    );
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history,
        session.history
    );
    Ok(())
}
#[tokio::test]
async fn confirmation_stop_closes_remaining_tool_calls_without_execution()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Manual);
    let model = Arc::new(FakeModel::new(vec![vec![
        call(
            "first",
            "write_file",
            serde_json::json!({"path":"one","content":"1","expected_version":null}),
        ),
        call(
            "second",
            "write_file",
            serde_json::json!({"path":"two","content":"2","expected_version":null}),
        ),
    ]]));
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(temp.path(), run, model, store.clone(), Arc::new(NoCancel))?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::NeedsConfirmation);
    assert!(!temp.path().join("one").exists());
    assert!(!temp.path().join("two").exists());
    assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult {call_id,result} if call_id=="second" && result.error.as_ref().is_some_and(|e|e.code=="not_executed"))));
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history,
        session.history
    );
    Ok(())
}
#[tokio::test]
async fn duplicate_model_tool_ids_fail_before_any_dispatch()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![vec![
        call(
            "same",
            "write_file",
            serde_json::json!({"path":"one","content":"x","expected_version":null}),
        ),
        call(
            "same",
            "write_file",
            serde_json::json!({"path":"two","content":"x","expected_version":null}),
        ),
    ]]));
    let mut runtime = runtime(
        temp.path(),
        run,
        model,
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(
        result.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_model_response")
    );
    assert!(!temp.path().join("one").exists());
    assert!(!temp.path().join("two").exists());
    Ok(())
}

struct BackgroundModel {
    modes: Mutex<Option<saya_core::runtime::RuntimeHandle>>,
    count: std::sync::atomic::AtomicUsize,
    marker: std::path::PathBuf,
    job: Mutex<Option<String>>,
}
impl Model for BackgroundModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let count = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                let args = serde_json::json!({"tool":"exec","arguments":{"argv":["/bin/sh","-c","printf started > \"$1\"; sleep 30","sh",self.marker]}});
                return Ok(
                    Box::new(Stream(Some(vec![call("launch", "background_start", args)])))
                        as Box<dyn ModelStream>,
                );
            }
            let id = request
                .history
                .iter()
                .find_map(|item| match item {
                    HistoryItem::ToolResult { call_id, result } if call_id == "launch" => result
                        .data
                        .as_ref()?
                        .get("job_id")?
                        .as_str()
                        .map(str::to_owned),
                    _ => None,
                })
                .ok_or_else(|| err("missing_job"))?;
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                while !self.marker.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| err("not_started"))?;
            let handle = self
                .modes
                .lock()
                .map_err(|_| err("test"))?
                .clone()
                .ok_or_else(|| err("test"))?;
            if !matches!(
                handle.background_status(&id)?,
                Some(saya_core::background::JobStatus::Running)
            ) {
                return Err(err("job_not_running"));
            }
            *self.job.lock().map_err(|_| err("test"))? = Some(id);
            Ok(Box::new(Stream(Some(vec![HistoryItem::AssistantText(
                "continued".into(),
            )]))) as Box<dyn ModelStream>)
        })
    }
}
#[tokio::test]
async fn runtime_continues_model_while_background_runs_then_cleans_up()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Bypass);
    let model = Arc::new(BackgroundModel {
        modes: Mutex::new(None),
        count: std::sync::atomic::AtomicUsize::new(0),
        marker: temp.path().join("started"),
        job: Mutex::new(None),
    });
    let store = Arc::new(MemoryStore::default());
    let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        true,
        ReadLimits::default(),
    )?);
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: model.clone(),
        store: store.clone(),
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })?;
    let handle = runtime.handle();
    *model.modes.lock().map_err(|_| "lock")? = Some(handle.clone());
    let result = tokio::time::timeout(std::time::Duration::from_secs(8), runtime.run(&mut session))
        .await??;
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(result.text.as_deref(), Some("continued"));
    assert_eq!(
        result.stop_reason.as_deref(),
        Some("completed_with_background_cleanup")
    );
    let id = model
        .job
        .lock()
        .map_err(|_| "lock")?
        .clone()
        .ok_or("missing")?;
    assert!(!matches!(
        handle.background_status(&id)?,
        Some(saya_core::background::JobStatus::Running)
    ));
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history,
        session.history
    );
    assert_eq!(
        session
            .history
            .iter()
            .filter(|h| matches!(h, HistoryItem::ToolResult { .. }))
            .count(),
        1
    );
    Ok(())
}

struct FailingStore {
    inner: MemoryStore,
    fail_result: bool,
}
impl Store for FailingStore {
    fn load_session<'a>(&'a self, id: SessionId) -> Operation<'a, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [saya_core::boundary::StoreChange]) -> Operation<'a, ()> {
        let reject = changes.iter().any(|change| {
            if self.fail_result {
                matches!(
                    change,
                    saya_core::boundary::StoreChange::ToolFinished { .. }
                )
            } else {
                matches!(change, saya_core::boundary::StoreChange::ToolIntent { .. })
            }
        });
        if reject {
            Box::pin(async { Err(err("io_error")) })
        } else {
            self.inner.commit(changes)
        }
    }
}
#[tokio::test]
async fn intent_failure_stops_before_side_effect() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![
        vec![call(
            "write",
            "write_file",
            serde_json::json!({"path":"out.txt","content":"data","expected_version":null}),
        )],
        vec![HistoryItem::AssistantText("should not happen".into())],
    ]));
    let store = Arc::new(FailingStore {
        inner: MemoryStore::default(),
        fail_result: false,
    });
    let mut runtime = runtime(temp.path(), run, model.clone(), store, Arc::new(NoCancel))?;
    let error = runtime.run(&mut session).await.expect_err("intent failure");
    assert_eq!(error.0.code, "storage_error");
    assert!(!temp.path().join("out.txt").exists());
    assert_eq!(model.seen.lock().map_err(|_| "lock")?.len(), 1);
    Ok(())
}
#[tokio::test]
async fn result_save_failure_retains_observed_write_result()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![
        vec![call(
            "write",
            "write_file",
            serde_json::json!({"path":"out.txt","content":"data","expected_version":null}),
        )],
        vec![HistoryItem::AssistantText("should not happen".into())],
    ]));
    let store = Arc::new(FailingStore {
        inner: MemoryStore::default(),
        fail_result: true,
    });
    let mut runtime = runtime(temp.path(), run, model.clone(), store, Arc::new(NoCancel))?;
    let error = runtime.run(&mut session).await.expect_err("result failure");
    assert_eq!(error.0.code, "storage_error");
    assert!(error.2.as_ref().is_some_and(|result| result.ok));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("out.txt"))?,
        "data"
    );
    assert_eq!(model.seen.lock().map_err(|_| "lock")?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn review_requests_share_execution_request_limit() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![
        vec![call(
            "exec",
            "exec",
            serde_json::json!({"argv":["/bin/echo","hello"]}),
        )],
        vec![HistoryItem::AssistantText(
            "{\"decision\":\"allow\",\"reason\":\"bounded command\"}".into(),
        )],
        vec![HistoryItem::AssistantText("unexpected".into())],
    ]));
    let store = Arc::new(MemoryStore::default());
    let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let limits = RuntimeLimits {
        model_requests: 2,
        ..RuntimeLimits::default()
    };
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: model.clone(),
        store,
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits,
    })?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::LimitReached, "{result:?}");
    let seen = model.seen.lock().map_err(|_| "lock")?;
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].purpose, saya_core::contract::ModelPurpose::Review);
    assert!(seen[1].tools.is_empty());
    assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult {call_id,result} if call_id=="exec" && result.ok)));
    Ok(())
}

struct RetryModel(std::sync::atomic::AtomicUsize);
impl Model for RetryModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            if self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Err(err("transient_connection"));
            }
            Ok(Box::new(Stream(Some(vec![HistoryItem::AssistantText(
                "retried".into(),
            )]))) as Box<dyn ModelStream>)
        })
    }
}
#[tokio::test(start_paused = true)]
async fn transient_model_error_retries_within_budget() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(RetryModel(std::sync::atomic::AtomicUsize::new(0)));
    let store = Arc::new(MemoryStore::default());
    let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let limits = RuntimeLimits {
        retry_jitter_max: std::time::Duration::ZERO,
        ..RuntimeLimits::default()
    };
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: model.clone(),
        store,
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits,
    })?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(result.text.as_deref(), Some("retried"));
    assert_eq!(model.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    Ok(())
}
struct PartialStream(bool);
impl ModelStream for PartialStream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if self.0 {
                self.0 = false;
                Ok(Some(ModelUpdate::TextDelta("partial".into())))
            } else {
                Err(err("transient_connection"))
            }
        })
    }
}
struct PartialModel(std::sync::atomic::AtomicUsize);
impl Model for PartialModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Box::new(PartialStream(true)) as Box<dyn ModelStream>)
        })
    }
}
#[tokio::test]
async fn partial_model_failure_is_preserved_and_never_retried()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(PartialModel(std::sync::atomic::AtomicUsize::new(0)));
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        store.clone(),
        Arc::new(NoCancel),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(result.text.as_deref(), Some("partial"));
    assert_eq!(model.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history,
        session.history
    );
    Ok(())
}

#[tokio::test]
async fn runtime_enforces_all_work_and_permission_combinations()
-> Result<(), Box<dyn std::error::Error>> {
    for mode in [
        WorkMode::Plan,
        WorkMode::Build,
        WorkMode::Code,
        WorkMode::Work,
    ] {
        for permissions in [
            PermissionMode::Auto,
            PermissionMode::Manual,
            PermissionMode::Bypass,
        ] {
            let temp = support::temp_workspace()?;
            let (mut session, mut run) = fixture(temp.path(), permissions);
            session.work_mode = mode;
            run.work_mode = mode;
            let model = Arc::new(FakeModel::new(vec![
                vec![call(
                    "write",
                    "write_file",
                    serde_json::json!({"path":"out.txt","content":"data","expected_version":null}),
                )],
                vec![HistoryItem::AssistantText("done".into())],
            ]));
            let store = Arc::new(MemoryStore::default());
            let mut runtime = runtime(temp.path(), run, model, store.clone(), Arc::new(NoCancel))?;
            let result = runtime.run(&mut session).await?;
            match (mode, permissions) {
                (WorkMode::Plan, _) => {
                    assert_eq!(result.status, RunStatus::Completed);
                    assert!(!temp.path().join("out.txt").exists());
                    assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult {call_id,result} if call_id=="write" && !result.ok)));
                }
                (_, PermissionMode::Manual) => {
                    assert_eq!(result.status, RunStatus::NeedsConfirmation);
                    assert!(!temp.path().join("out.txt").exists());
                }
                (_, _) => {
                    assert_eq!(result.status, RunStatus::Completed);
                    assert_eq!(
                        std::fs::read_to_string(temp.path().join("out.txt"))?,
                        "data"
                    );
                }
            }
            assert_eq!(
                store
                    .load_session(session.id)
                    .await?
                    .ok_or("missing")?
                    .history,
                session.history
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn stale_session_never_overwrites_saved_history() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let mut saved = session.clone();
    saved.history.push(HistoryItem::UserText("preserve".into()));
    let store = Arc::new(MemoryStore::default());
    store
        .commit(&[saya_core::boundary::StoreChange::Session(saved.clone())])
        .await?;
    let model = Arc::new(FakeModel::new(vec![vec![HistoryItem::AssistantText(
        "unused".into(),
    )]]));
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        store.clone(),
        Arc::new(NoCancel),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(
        result.error.as_ref().map(|e| e.code.as_str()),
        Some("session_conflict")
    );
    assert_eq!(
        store.load_session(session.id).await?.ok_or("missing")?,
        saved
    );
    assert!(model.seen.lock().map_err(|_| "lock")?.is_empty());
    Ok(())
}

struct NeverModel;
impl Model for NeverModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(pending())
    }
}
#[tokio::test(start_paused = true)]
async fn active_budget_interrupts_hanging_model() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let store = Arc::new(MemoryStore::default());
    let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let limits = RuntimeLimits {
        active_time: std::time::Duration::from_secs(1),
        ..RuntimeLimits::default()
    };
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: Arc::new(NeverModel),
        store,
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits,
    })?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::LimitReached);
    Ok(())
}
struct FailBackgroundResult {
    inner: MemoryStore,
}
impl Store for FailBackgroundResult {
    fn load_session<'a>(&'a self, id: SessionId) -> Operation<'a, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [saya_core::boundary::StoreChange]) -> Operation<'a, ()> {
        if changes.iter().any(|change| matches!(change,saya_core::boundary::StoreChange::ToolFinished {call_id,..} if call_id.starts_with("background-"))) {Box::pin(async {Err(err("storage_error"))})} else {self.inner.commit(changes)}
    }
}
struct WaitBackgroundFailure {
    handle: Mutex<Option<saya_core::runtime::RuntimeHandle>>,
    count: std::sync::atomic::AtomicUsize,
}
impl Model for WaitBackgroundFailure {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            if self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return Ok(Box::new(Stream(Some(vec![call(
                    "launch",
                    "background_start",
                    serde_json::json!({"tool":"read_file","arguments":{"path":"input.txt","start_line":1,"line_count":1}}),
                )]))) as Box<dyn ModelStream>);
            }
            let id = request
                .history
                .iter()
                .find_map(|item| match item {
                    HistoryItem::ToolResult { call_id, result } if call_id == "launch" => result
                        .data
                        .as_ref()?
                        .get("job_id")?
                        .as_str()
                        .map(str::to_owned),
                    _ => None,
                })
                .ok_or_else(|| err("missing_job"))?;
            let handle = self
                .handle
                .lock()
                .map_err(|_| err("test"))?
                .clone()
                .ok_or_else(|| err("test"))?;
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if matches!(
                        handle.background_status(&id),
                        Ok(Some(saya_core::background::JobStatus::Failed(_)))
                    ) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .map_err(|_| err("timeout"))?;
            Ok(Box::new(Stream(Some(vec![HistoryItem::AssistantText(
                "unexpected".into(),
            )]))) as Box<dyn ModelStream>)
        })
    }
}
#[tokio::test]
async fn background_save_failure_cannot_be_reported_as_success()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("input.txt"), "data")?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let store = Arc::new(FailBackgroundResult {
        inner: MemoryStore::default(),
    });
    let model = Arc::new(WaitBackgroundFailure {
        handle: Mutex::new(None),
        count: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut runtime = runtime(temp.path(), run, model.clone(), store, Arc::new(NoCancel))?;
    *model.handle.lock().map_err(|_| "lock")? = Some(runtime.handle());
    let failure = runtime
        .run(&mut session)
        .await
        .expect_err("storage failure");
    assert_eq!(failure.0.code, "storage_error");
    assert!(failure.2.as_ref().is_some_and(|result| result.ok));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn active_time_budget_starts_when_run_begins_not_when_constructed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![vec![HistoryItem::AssistantText(
        "done".into(),
    )]]));
    let mut runtime = runtime(
        temp.path(),
        run,
        model,
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?;
    tokio::time::advance(std::time::Duration::from_secs(3600)).await;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    Ok(())
}

#[tokio::test]
async fn manual_noninteractive_read_stops_before_revealing_file()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("input.txt"), "private-fixture")?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Manual);
    let model = Arc::new(FakeModel::new(vec![vec![call(
        "read",
        "read_file",
        serde_json::json!({"path":"input.txt"}),
    )]]));
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(temp.path(), run, model.clone(), store, Arc::new(NoCancel))?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::NeedsConfirmation);
    assert_eq!(model.seen.lock().map_err(|_| "lock")?.len(), 1);
    assert!(!serde_json::to_string(&session.history)?.contains("private-fixture"));
    Ok(())
}

#[tokio::test]
async fn manual_noninteractive_background_status_requires_confirmation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Manual);
    let model = Arc::new(FakeModel::new(vec![vec![call(
        "status",
        "background_status",
        serde_json::json!({"job_id":"unknown-job"}),
    )]]));
    let store = Arc::new(MemoryStore::default());
    let mut runtime = runtime(temp.path(), run, model.clone(), store, Arc::new(NoCancel))?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::NeedsConfirmation);
    assert_eq!(model.seen.lock().map_err(|_| "lock")?.len(), 1);
    assert!(session.history.iter().any(|item| matches!(item, HistoryItem::ToolResult {call_id, result} if call_id == "status" && result.error.as_ref().is_some_and(|e| e.code == "needs_confirmation"))));
    Ok(())
}

struct ProgressThenFailure(std::sync::atomic::AtomicUsize);
struct ProgressStream(bool);
impl ModelStream for ProgressStream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if !self.0 {
                self.0 = true;
                Ok(Some(ModelUpdate::Progress))
            } else {
                Err(err("transient_connection"))
            }
        })
    }
}
impl Model for ProgressThenFailure {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Box::new(ProgressStream(false)) as Box<dyn ModelStream>)
        })
    }
}
#[tokio::test]
async fn non_text_provider_event_prevents_retry_after_connection_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(ProgressThenFailure(std::sync::atomic::AtomicUsize::new(0)));
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(model.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        session
            .history
            .iter()
            .any(|item| matches!(item, HistoryItem::AssistantPartial(_)))
    );
    Ok(())
}

struct LimitedModel(&'static str);
impl Model for LimitedModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move { Err(err(self.0)) })
    }
}
#[tokio::test]
async fn provider_context_and_output_limits_have_distinct_limit_reasons()
-> Result<(), Box<dyn std::error::Error>> {
    for code in ["context_budget", "output_limit"] {
        let temp = support::temp_workspace()?;
        let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
        let mut runtime = runtime(
            temp.path(),
            run,
            Arc::new(LimitedModel(code)),
            Arc::new(MemoryStore::default()),
            Arc::new(NoCancel),
        )?;
        let result = runtime.run(&mut session).await?;
        assert_eq!(result.status, RunStatus::LimitReached);
        assert_eq!(result.stop_reason.as_deref(), Some(code));
    }
    Ok(())
}

#[tokio::test]
async fn read_only_runtime_blocks_fabricated_foreground_and_background_mutations()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    for (name, args) in [
        (
            "write_file",
            serde_json::json!({"path":"forbidden","content":"x","expected_version":null}),
        ),
        (
            "exec",
            serde_json::json!({"argv":["/usr/bin/touch",temp.path().join("forbidden").to_str().ok_or("path")?]}),
        ),
        (
            "background_start",
            serde_json::json!({"tool":"write_file","arguments":{"path":"forbidden","content":"x","expected_version":null}}),
        ),
    ] {
        let (mut session, run) = fixture(temp.path(), PermissionMode::Bypass);
        let model = Arc::new(FakeModel::new(vec![
            vec![call("bad", name, args)],
            vec![HistoryItem::AssistantText("done".into())],
        ]));
        let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
        let files = Arc::new(ReadTools::new(
            temp.path(),
            &[],
            true,
            ReadLimits::default(),
        )?);
        let mut runtime = AgentRuntime::new_read_only(RuntimeSetup {
            run,
            model: model.clone(),
            store: Arc::new(MemoryStore::default()),
            policy,
            files,
            cancellation: Arc::new(NoCancel),
            review: ReviewConfig::new("fake".into()),
            exec: ExecConfig::default(),
            preauthorizations: Preauthorizations::default(),
            known_secrets: vec![],
            summary: "task".into(),
            confirmation: None,
            limits: RuntimeLimits::default(),
        })?;
        let _ = runtime.run(&mut session).await?;
        assert!(
            !temp.path().join("forbidden").exists(),
            "{name} mutated the workspace"
        );
        let seen = model.seen.lock().map_err(|_| "lock")?;
        assert!(
            seen[0]
                .tools
                .iter()
                .all(|t| !matches!(t.name.as_str(), "write_file" | "apply_patch" | "exec"))
        );
        let background = seen[0]
            .tools
            .iter()
            .find(|t| t.name == "background_start")
            .ok_or("background missing")?;
        assert_eq!(
            background.input_schema["properties"]["tool"]["enum"],
            serde_json::json!(["read_file", "list_dir", "find_files", "search_text"])
        );
    }
    Ok(())
}

struct AnswerInput {
    answer: &'static str,
    delay: std::time::Duration,
}
impl saya_core::input::InputHandler for AnswerInput {
    fn ask<'a>(
        &'a self,
        request: &'a saya_core::input::InputRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<String>> {
        Box::pin(async move {
            assert_eq!(request.question, "Which option?");
            assert_eq!(request.options, ["one", "two"]);
            tokio::time::sleep(self.delay).await;
            Ok(Some(self.answer.into()))
        })
    }
}
fn ask_call() -> HistoryItem {
    call(
        "ask",
        "ask_user",
        serde_json::json!({"question":"Which option?","options":["one","two"]}),
    )
}

#[tokio::test]
async fn noninteractive_clarification_preserves_question_and_stops()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![vec![ask_call()]]));
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::NeedsInput);
    assert_eq!(model.seen.lock().map_err(|_| "lock")?.len(), 1);
    assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult{call_id,result} if call_id=="ask" && result.data.as_ref().is_some_and(|v|v["question"]=="Which option?"))));
    Ok(())
}
#[tokio::test(start_paused = true)]
async fn clarification_answer_returns_to_model_and_human_wait_does_not_consume_active_budget()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![
        vec![ask_call()],
        vec![HistoryItem::AssistantText("done".into())],
    ]));
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?
    .with_input_handler(Arc::new(AnswerInput {
        answer: "two",
        delay: std::time::Duration::from_secs(2000),
    }));
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    let seen = model.seen.lock().map_err(|_| "lock")?;
    assert!(seen[0].tools.iter().any(|t| t.name == "ask_user"));
    assert!(seen[1].history.iter().any(|item|matches!(item,HistoryItem::ToolResult{call_id,result} if call_id=="ask" && result.data.as_ref().is_some_and(|v|v["answer"]=="two"))));
    Ok(())
}
#[tokio::test]
async fn manual_mode_requires_approval_even_for_a_clarification()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Manual);
    let model = Arc::new(FakeModel::new(vec![vec![ask_call()]]));
    let mut runtime = runtime(
        temp.path(),
        run,
        model,
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?
    .with_input_handler(Arc::new(AnswerInput {
        answer: "two",
        delay: std::time::Duration::ZERO,
    }));
    assert_eq!(
        runtime.run(&mut session).await?.status,
        RunStatus::NeedsConfirmation
    );
    Ok(())
}

#[tokio::test]
async fn scoped_project_context_is_supplied_as_data_before_nested_tool_dispatch()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::project_context::ProjectContext;
    let temp = support::temp_workspace()?;
    std::fs::create_dir_all(temp.path().join("src"))?;
    std::fs::write(temp.path().join("AGENTS.md"), "ROOT_CONVENTION")?;
    std::fs::write(temp.path().join("src/AGENTS.md"), "NESTED_CONVENTION")?;
    std::fs::write(temp.path().join("src/data.txt"), "useful data")?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let read = serde_json::json!({"path":"src/data.txt","start_line":1,"line_count":3});
    let model = Arc::new(FakeModel::new(vec![
        vec![
            call("first", "read_file", read.clone()),
            call("second", "read_file", read.clone()),
        ],
        vec![call("retry", "read_file", read)],
        vec![HistoryItem::AssistantText("done".into())],
    ]));
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?
    .with_project_context(ProjectContext::new(temp.path(), None)?)?;
    assert_eq!(
        runtime.run(&mut session).await?.status,
        RunStatus::Completed
    );
    let seen = model.seen.lock().map_err(|_| "lock")?;
    assert_eq!(seen.len(), 3);
    let initial = serde_json::to_string(&seen[0].history)?;
    assert!(initial.contains("ROOT_CONVENTION"));
    assert!(!initial.contains("NESTED_CONVENTION"));
    assert!(!seen[0].instructions.contains("ROOT_CONVENTION"));
    let second = serde_json::to_string(&seen[1].history)?;
    assert!(second.contains("NESTED_CONVENTION"));
    assert!(second.contains("project_context_changed"));
    assert!(second.contains("not_executed"));
    assert!(seen[2].history.iter().any(|item|matches!(item,HistoryItem::ToolResult{call_id,result} if call_id=="retry" && result.ok)));
    Ok(())
}

struct ChangeContextModel {
    inner: FakeModel,
    path: std::path::PathBuf,
}
impl Model for ChangeContextModel {
    fn start<'a>(
        &'a self,
        req: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            if self.inner.seen.lock().map_err(|_| err("test"))?.is_empty() {
                std::fs::write(&self.path, "CHANGED_CONVENTION").map_err(|_| err("test"))?;
            }
            self.inner.start(req, cancel).await
        })
    }
}
#[tokio::test]
async fn changed_instruction_during_model_call_forces_context_refresh_before_tool()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::project_context::ProjectContext;
    let temp = support::temp_workspace()?;
    let source = temp.path().join("AGENTS.md");
    std::fs::write(&source, "ORIGINAL_CONVENTION")?;
    std::fs::write(temp.path().join("data.txt"), "content")?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let args = serde_json::json!({"path":"data.txt","start_line":1,"line_count":1});
    let model = Arc::new(ChangeContextModel {
        path: source,
        inner: FakeModel::new(vec![
            vec![call("first", "read_file", args.clone())],
            vec![call("retry", "read_file", args)],
            vec![HistoryItem::AssistantText("done".into())],
        ]),
    });
    let mut runtime = runtime(
        temp.path(),
        run,
        model.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(NoCancel),
    )?
    .with_project_context(ProjectContext::new(temp.path(), None)?)?;
    assert_eq!(
        runtime.run(&mut session).await?.status,
        RunStatus::Completed
    );
    let seen = model.inner.seen.lock().map_err(|_| "lock")?;
    assert!(serde_json::to_string(&seen[0].history)?.contains("ORIGINAL_CONVENTION"));
    assert!(serde_json::to_string(&seen[1].history)?.contains("CHANGED_CONVENTION"));
    assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult{call_id,result} if call_id=="first" && result.error.as_ref().is_some_and(|e|e.code=="project_context_changed"))));
    Ok(())
}

struct ChangeAtApproval(std::path::PathBuf);
impl saya_core::runtime::ApprovalHandler for ChangeAtApproval {
    fn confirm<'a>(
        &'a self,
        _: &'a saya_core::contract::ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            std::fs::write(&self.0, "UPDATED_DURING_APPROVAL").map_err(|_| err("test"))?;
            Ok(Some(true))
        })
    }
}
#[tokio::test]
async fn context_change_during_approval_requires_a_new_model_turn_before_dispatch()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::project_context::ProjectContext;
    for background in [false, true] {
        let temp = support::temp_workspace()?;
        let source = temp.path().join("AGENTS.md");
        std::fs::write(&source, "BEFORE_APPROVAL")?;
        std::fs::write(temp.path().join("data.txt"), "content")?;
        let (mut session, run) = fixture(temp.path(), PermissionMode::Manual);
        let read = serde_json::json!({"path":"data.txt","start_line":1,"line_count":1});
        let (tool, args) = if background {
            (
                "background_start",
                serde_json::json!({"tool":"read_file","arguments":read}),
            )
        } else {
            ("read_file", read)
        };
        let model = Arc::new(FakeModel::new(vec![
            vec![call("first", tool, args.clone())],
            vec![call("retry", tool, args)],
            vec![HistoryItem::AssistantText("done".into())],
        ]));
        let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
        let files = Arc::new(ReadTools::new(
            temp.path(),
            &[],
            false,
            ReadLimits::default(),
        )?);
        let mut runtime = AgentRuntime::new(RuntimeSetup {
            run,
            model: model.clone(),
            store: Arc::new(MemoryStore::default()),
            policy,
            files,
            cancellation: Arc::new(NoCancel),
            review: ReviewConfig::new("fake".into()),
            exec: ExecConfig::default(),
            preauthorizations: Preauthorizations::default(),
            known_secrets: vec![],
            summary: "task".into(),
            confirmation: Some(Arc::new(ChangeAtApproval(source))),
            limits: RuntimeLimits::default(),
        })?
        .with_project_context(ProjectContext::new(temp.path(), None)?)?;
        assert_eq!(
            runtime.run(&mut session).await?.status,
            RunStatus::Completed
        );
        assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult{call_id,result} if call_id=="first" && result.error.as_ref().is_some_and(|e|e.code=="project_context_changed"))),"background={background}");
        let seen = model.seen.lock().map_err(|_| "lock")?;
        assert!(serde_json::to_string(&seen[1].history)?.contains("UPDATED_DURING_APPROVAL"));
    }
    Ok(())
}

struct SlowApproval;
impl saya_core::runtime::ApprovalHandler for SlowApproval {
    fn confirm<'a>(
        &'a self,
        _: &'a saya_core::contract::ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_secs(2000)).await;
            Ok(Some(true))
        })
    }
}
#[tokio::test(start_paused = true)]
async fn human_approval_wait_does_not_consume_active_run_budget()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("data.txt"), "content")?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Manual);
    let model = Arc::new(FakeModel::new(vec![
        vec![call(
            "read",
            "read_file",
            serde_json::json!({"path":"data.txt","start_line":1,"line_count":1}),
        )],
        vec![HistoryItem::AssistantText("done".into())],
    ]));
    let policy = Arc::new(Policy::new(temp.path(), PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store: Arc::new(MemoryStore::default()),
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: Some(Arc::new(SlowApproval)),
        limits: RuntimeLimits::default(),
    })?;
    assert_eq!(
        runtime.run(&mut session).await?.status,
        RunStatus::Completed
    );
    Ok(())
}

#[tokio::test]
async fn sqlite_intent_failure_prevents_a_real_workspace_write()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::sqlite::SqliteStore;
    let temp = support::temp_workspace()?;
    let store = Arc::new(SqliteStore::open(&temp.path().join("state"), vec![])?);
    let connection = rusqlite::Connection::open(store.path())?;
    connection.execute_batch("CREATE TRIGGER reject_intent BEFORE INSERT ON tool_calls BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;")?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![vec![call(
        "write",
        "write_file",
        serde_json::json!({"path":"never.txt","content":"should not appear","expected_version":null}),
    )]]));
    let mut runtime = runtime(temp.path(), run, model, store.clone(), Arc::new(NoCancel))?;
    let failure = runtime
        .run(&mut session)
        .await
        .expect_err("intent failure must stop the run");
    assert_eq!(failure.0.code, "storage_error");
    assert!(!temp.path().join("never.txt").exists());
    let persisted = store
        .load_session(session.id)
        .await?
        .ok_or("session missing")?;
    assert!(
        persisted
            .history
            .iter()
            .any(|item| matches!(item,HistoryItem::ToolCall(call) if call.call_id=="write"))
    );
    Ok(())
}
#[tokio::test]
async fn sqlite_records_actual_tool_result_and_session_history()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::sqlite::SqliteStore;
    let temp = support::temp_workspace()?;
    let data = temp.path().join("state");
    let store = Arc::new(SqliteStore::open(&data, vec![])?);
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![
        vec![call(
            "write",
            "write_file",
            serde_json::json!({"path":"created.txt","content":"fixture","expected_version":null}),
        )],
        vec![HistoryItem::AssistantText("done".into())],
    ]));
    let mut runtime = runtime(
        temp.path(),
        run.clone(),
        model,
        store.clone(),
        Arc::new(NoCancel),
    )?;
    assert_eq!(
        runtime.run(&mut session).await?.status,
        RunStatus::Completed
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("created.txt"))?,
        "fixture"
    );
    drop(runtime);
    drop(store);
    let reopened = SqliteStore::open(&data, vec![])?;
    assert_eq!(reopened.load_session(session.id).await?, Some(session));
    let conn = rusqlite::Connection::open(reopened.path())?;
    let state: String = conn.query_row(
        "SELECT state FROM tool_calls WHERE run_id=?1 AND call_id='write'",
        [run.id.0.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(state, "succeeded");
    Ok(())
}

#[tokio::test]
async fn sqlite_result_failure_preserves_observed_write_and_stops_loop()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::sqlite::SqliteStore;
    let temp = support::temp_workspace()?;
    let data = temp.path().join("state");
    let store = Arc::new(SqliteStore::open(&data, vec![])?);
    let conn = rusqlite::Connection::open(store.path())?;
    conn.execute_batch("CREATE TRIGGER reject_result BEFORE UPDATE OF result ON tool_calls BEGIN SELECT RAISE(FAIL, 'fixture failure'); END;")?;
    let (mut session, run) = fixture(temp.path(), PermissionMode::Auto);
    let model = Arc::new(FakeModel::new(vec![
        vec![call(
            "write",
            "write_file",
            serde_json::json!({"path":"written.txt","content":"once","expected_version":null}),
        )],
        vec![HistoryItem::AssistantText("must not be called".into())],
    ]));
    let mut runtime = runtime(
        temp.path(),
        run.clone(),
        model.clone(),
        store.clone(),
        Arc::new(NoCancel),
    )?;
    let failure = runtime
        .run(&mut session)
        .await
        .expect_err("result failure must stop the run");
    assert_eq!(failure.0.code, "storage_error");
    assert!(failure.2.as_ref().is_some_and(|result| result.ok));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("written.txt"))?,
        "once"
    );
    assert_eq!(model.seen.lock().map_err(|_| "lock")?.len(), 1);
    let state: String = conn.query_row(
        "SELECT state FROM tool_calls WHERE run_id=?1 AND call_id='write'",
        [run.id.0.to_string()],
        |r| r.get(0),
    )?;
    assert_eq!(state, "running");
    Ok(())
}
