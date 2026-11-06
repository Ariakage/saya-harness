mod support;
use saya_core::{
    approvals::{
        ApprovalManager, ApprovalSetup, Authorization, AuthorizationInput, PermissionRevision,
        Preauthorizations,
    },
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, Operation as ModelOperation, Store,
        StoreChange, Tool, ToolContext,
    },
    contract::{
        ApprovalRequest, BoundaryError, ErrorInfo, ModelProtocol, PermissionMode, Run, RunId,
        Session, SessionId, ToolCall, WorkMode,
    },
    memory::MemoryStore,
    mode::ModeController,
    permissions::{Operation, PermissionConfig, Policy},
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::{ExecConfig, ExecGate, ExecPlan, ExecTool},
        read::{ReadLimits, ReadTools},
        write::{DEFAULT_WRITE_MAX_BYTES, WriteGate, WriteTool, WriteToolKind},
    },
};
use std::{
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Default)]
struct Signal(AtomicBool);
impl Cancellation for Signal {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct NoModel(AtomicUsize);
impl Model for NoModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> ModelOperation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::AcqRel);
            Err(BoundaryError(
                ErrorInfo {
                    code: "model_error".into(),
                    message: "not available".into(),
                },
                None,
                None,
            ))
        })
    }
}
#[derive(Default)]
struct RecordingStore {
    inner: MemoryStore,
    requests: Mutex<Vec<ApprovalRequest>>,
}
impl Store for RecordingStore {
    fn load_session(&self, id: SessionId) -> ModelOperation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> ModelOperation<'a, ()> {
        Box::pin(async move {
            self.inner.commit(changes).await?;
            let mut requests = self.requests.lock().map_err(|_| {
                BoundaryError(
                    ErrorInfo {
                        code: "storage_error".into(),
                        message: "lock".into(),
                    },
                    None,
                    None,
                )
            })?;
            for change in changes {
                if let StoreChange::ApprovalRequested(request) = change {
                    requests.push(request.clone());
                }
            }
            Ok(())
        })
    }
}
fn fixture(root: &Path, permissions: PermissionMode) -> (Session, Run, ToolContext) {
    let session = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "fake".into(),
        protocol: ModelProtocol::Responses,
        model: "fake".into(),
        history: vec![],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions,
    };
    let context = ToolContext {
        run: run.clone(),
        workspace: root.into(),
    };
    (session, run, context)
}
async fn store_for(session: &Session, run: &Run) -> Result<Arc<RecordingStore>, BoundaryError> {
    let store = Arc::new(RecordingStore::default());
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    Ok(store)
}
fn policy(root: &Path, rules: &str) -> Result<Arc<Policy>, BoundaryError> {
    Ok(Arc::new(Policy::new(
        root,
        PermissionConfig::from_toml(rules)?,
    )?))
}
fn manager(
    policy: Arc<Policy>,
    modes: ModeController,
    revision: PermissionRevision,
    store: Arc<dyn Store>,
    run: &Run,
    preauth: Preauthorizations,
    signal: Arc<Signal>,
) -> Result<ApprovalManager, BoundaryError> {
    ApprovalManager::new(
        policy,
        modes,
        revision,
        store,
        ApprovalSetup {
            run_id: run.id,
            session_id: run.session_id,
            initial_permissions: run.permissions,
            preauthorizations: preauth,
            known_secrets: vec![],
            cancellation: signal,
        },
    )
}
fn write_call(id: &str, content: &str) -> ToolCall {
    ToolCall {
        call_id: id.into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path":"out.txt","content":content,"expected_version":null})
            .to_string(),
    }
}
fn input<'a>(
    op: &'a Operation,
    call: &'a ToolCall,
    context: &'a ToolContext,
    interactive: bool,
) -> AuthorizationInput<'a> {
    AuthorizationInput {
        operation: op,
        call,
        context,
        summary: "user task",
        interactive,
    }
}
fn deny_model() -> NoModel {
    NoModel(AtomicUsize::new(0))
}
fn config() -> ReviewConfig {
    ReviewConfig::new("execution".into())
}

#[tokio::test]
async fn one_approval_executes_one_exact_write_and_cannot_be_reused()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let call = write_call("call-1", "hello");
    let operation = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
    let request = match manager
        .authorize(
            input(&operation, &call, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("expected pending".into()),
    };
    assert_eq!(store.requests.lock().map_err(|_| "lock")?.len(), 1);
    let permit = match manager.resolve(&request.approval_id, true)? {
        Authorization::Permit(p) => p,
        _ => return Err("expected permit".into()),
    };
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let tool = WriteTool::new(
        files,
        store.clone(),
        permit,
        WriteToolKind::WriteFile,
        DEFAULT_WRITE_MAX_BYTES,
    );
    assert!(tool.execute(&call, &context, &Signal::default()).await?.ok);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("out.txt"))?,
        "hello"
    );
    std::fs::remove_file(temp.path().join("out.txt"))?;
    let again = tool.execute(&call, &context, &Signal::default()).await?;
    assert_eq!(
        again.error.as_ref().map(|e| e.code.as_str()),
        Some("permission_denied")
    );
    assert!(!temp.path().join("out.txt").exists());
    Ok(())
}

#[tokio::test]
async fn noninteractive_confirmation_is_saved_and_does_not_grant()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let call = write_call("c", "hello");
    let op = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
    let request = match manager
        .authorize(
            input(&op, &call, &context, false),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::NeedsConfirmation(r) => r,
        _ => return Err("expected needs confirmation".into()),
    };
    assert_eq!(request.call_id, "c");
    assert_eq!(store.requests.lock().map_err(|_| "lock")?.len(), 1);
    assert!(!temp.path().join("out.txt").exists());
    Ok(())
}

#[tokio::test]
async fn changed_operation_mode_permission_and_cancellation_invalidate_grant()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    for case in 0..4 {
        let modes = ModeController::new(WorkMode::Work);
        let revision = PermissionRevision::new(modes.clone(), run.permissions);
        let signal = Arc::new(Signal::default());
        let manager = manager(
            policy(temp.path(), "")?,
            modes.clone(),
            revision.clone(),
            store.clone(),
            &run,
            Preauthorizations::default(),
            signal.clone(),
        )?;
        let call = write_call(&format!("c{case}"), "hello");
        let op = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
        let request = match manager
            .authorize(
                input(&op, &call, &context, true),
                &deny_model(),
                &SharedBudget::default(),
                &config(),
            )
            .await?
        {
            Authorization::Pending(r) => r,
            _ => return Err("expected pending".into()),
        };
        let permit = match manager.resolve(&request.approval_id, true)? {
            Authorization::Permit(p) => p,
            _ => return Err("expected permit".into()),
        };
        let mut changed = call.clone();
        match case {
            0 => changed = write_call(&call.call_id, "changed"),
            1 => modes.request(WorkMode::Plan)?,
            2 => assert!(revision.change_if_idle(PermissionMode::Bypass)?),
            _ => signal.0.store(true, Ordering::Release),
        }
        assert!(!WriteGate::authorize(
            permit.as_ref(),
            &changed,
            &context,
            &temp.path().join("out.txt")
        )?);
    }
    Ok(())
}

#[tokio::test]
async fn denial_is_remembered_across_mode_change_and_cancelled_pending_cannot_approve()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, mut context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let signal = Arc::new(Signal::default());
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        &run,
        Preauthorizations::default(),
        signal.clone(),
    )?;
    let call = write_call("a", "hello");
    let op = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
    let request = match manager
        .authorize(
            input(&op, &call, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("expected pending".into()),
    };
    assert!(matches!(
        manager.resolve(&request.approval_id, false)?,
        Authorization::Denied("user_denied")
    ));
    modes.request(WorkMode::Code)?;
    context.run.work_mode = WorkMode::Code;
    let second = write_call("b", "hello");
    assert!(matches!(
        manager
            .authorize(
                input(&op, &second, &context, true),
                &deny_model(),
                &SharedBudget::default(),
                &config()
            )
            .await?,
        Authorization::Denied("previously_denied")
    ));
    let different = write_call("c", "different");
    let pending = match manager
        .authorize(
            input(&op, &different, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("expected pending".into()),
    };
    signal.0.store(true, Ordering::Release);
    assert_eq!(
        manager
            .resolve(&pending.approval_id, true)
            .err()
            .ok_or("should cancel")?
            .0
            .code,
        "cancelled"
    );
    Ok(())
}

struct DummyGate;
impl ExecGate for DummyGate {
    fn authorize(
        &self,
        _: &ExecPlan,
        _: &ToolCall,
        _: &ToolContext,
    ) -> Result<bool, BoundaryError> {
        Ok(false)
    }
}
fn command_call(id: &str, argument: &str) -> ToolCall {
    ToolCall {
        call_id: id.into(),
        name: "exec".into(),
        arguments: serde_json::json!({"argv":["/bin/echo",argument]}).to_string(),
    }
}
fn command_input(root: &Path, argument: &str) -> String {
    serde_json::json!({"schema_version":1,"commands":[{"argv":["/bin/echo",argument],"cwd":root}]})
        .to_string()
}

#[tokio::test]
async fn preauthorization_matches_exact_command_once_and_drives_real_adapter()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let inspect = ExecTool::new(
        files.clone(),
        store.clone(),
        Arc::new(DummyGate),
        ExecConfig::default(),
    );
    let call = command_call("a", "hello");
    let plan = inspect.plan(&call, &context)?;
    let operation = Operation::exec(&plan)?;
    let preauth = Preauthorizations::from_json(&command_input(temp.path(), "hello"), temp.path())?;
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        &run,
        preauth,
        Arc::new(Signal::default()),
    )?;
    let permit = match manager
        .authorize(
            input(&operation, &call, &context, false),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Permit(p) => p,
        _ => return Err("expected preauthorization".into()),
    };
    let exec = ExecTool::new(files, store.clone(), permit.clone(), ExecConfig::default());
    let result = exec.execute(&call, &context, &Signal::default()).await?;
    assert!(result.ok);
    assert_eq!(
        result.data.as_ref().and_then(|v| v["stdout"].as_str()),
        Some("hello\n")
    );
    assert!(!ExecGate::authorize(
        permit.as_ref(),
        &plan,
        &call,
        &context
    )?);
    let next = command_call("b", "hello");
    assert!(matches!(
        manager
            .authorize(
                input(&operation, &next, &context, false),
                &deny_model(),
                &SharedBudget::default(),
                &config()
            )
            .await?,
        Authorization::NeedsConfirmation(_)
    ));
    Ok(())
}

#[tokio::test]
async fn preauthorization_never_bypasses_plan_deny_or_initial_mode_binding()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, mut context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let inspect = ExecTool::new(
        files,
        store.clone(),
        Arc::new(DummyGate),
        ExecConfig::default(),
    );
    let call = command_call("a", "hello");
    let plan = inspect.plan(&call, &context)?;
    let operation = Operation::exec(&plan)?;
    let json = command_input(temp.path(), "hello");
    let modes = ModeController::new(WorkMode::Work);
    let denied = manager(
        policy(temp.path(), "[[rules]]\neffect='deny'\ntool='exec'")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        &run,
        Preauthorizations::from_json(&json, temp.path())?,
        Arc::new(Signal::default()),
    )?;
    assert!(matches!(
        denied
            .authorize(
                input(&operation, &call, &context, false),
                &deny_model(),
                &SharedBudget::default(),
                &config()
            )
            .await?,
        Authorization::Denied("policy_denied")
    ));
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        &run,
        Preauthorizations::from_json(&json, temp.path())?,
        Arc::new(Signal::default()),
    )?;
    modes.request(WorkMode::Plan)?;
    context.run.work_mode = WorkMode::Plan;
    assert!(matches!(
        manager
            .authorize(
                input(&operation, &call, &context, false),
                &deny_model(),
                &SharedBudget::default(),
                &config()
            )
            .await?,
        Authorization::Denied("policy_denied")
    ));
    modes.request(WorkMode::Work)?;
    context.run.work_mode = WorkMode::Work;
    assert!(matches!(
        manager
            .authorize(
                input(&operation, &call, &context, false),
                &deny_model(),
                &SharedBudget::default(),
                &config()
            )
            .await?,
        Authorization::NeedsConfirmation(_)
    ));
    Ok(())
}

#[tokio::test]
async fn permit_binds_command_arguments_working_directory_and_environment()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let inspect = ExecTool::new(
        files,
        store.clone(),
        Arc::new(DummyGate),
        ExecConfig::default(),
    );
    let call = command_call("a", "hello");
    let plan = inspect.plan(&call, &context)?;
    let operation = Operation::exec(&plan)?;
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store,
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let request = match manager
        .authorize(
            input(&operation, &call, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("expected pending".into()),
    };
    let permit = match manager.resolve(&request.approval_id, true)? {
        Authorization::Permit(p) => p,
        _ => return Err("expected permit".into()),
    };
    let mut changed = plan.clone();
    changed.environment_digest = "different".into();
    assert!(!ExecGate::authorize(
        permit.as_ref(),
        &changed,
        &call,
        &context
    )?);
    let mut changed = plan.clone();
    changed.argv.push("extra".into());
    assert!(!ExecGate::authorize(
        permit.as_ref(),
        &changed,
        &call,
        &context
    )?);
    let other = support::temp_workspace()?;
    let mut changed = plan.clone();
    changed.cwd = other.path().into();
    assert!(!ExecGate::authorize(
        permit.as_ref(),
        &changed,
        &call,
        &context
    )?);
    Ok(())
}

#[test]
fn permission_changes_are_rejected_while_call_active_and_approval_file_is_strict()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let modes = ModeController::new(WorkMode::Work);
    let version = PermissionRevision::new(modes.clone(), PermissionMode::Auto);
    assert!(modes.begin(modes.snapshot()?.revision)?);
    assert!(!version.change_if_idle(PermissionMode::Bypass)?);
    assert_eq!(version.current()?, 0);
    modes.finish()?;
    assert!(version.change_if_idle(PermissionMode::Bypass)?);
    assert_eq!(version.current()?, 1);
    for bad in [
        r#"{"schema_version":2,"commands":[]}"#,
        r#"{"schema_version":1,"commands":[{"argv":["/bin/echo","*"],"cwd":"."}]}"#,
        r#"{"schema_version":1,"commands":[{"argv":[],"cwd":"."}]}"#,
        r#"{"schema_version":1,"commands":[],"other":true}"#,
    ] {
        assert!(Preauthorizations::from_json(bad, temp.path()).is_err());
    }
    Ok(())
}

#[tokio::test]
async fn active_command_queues_mode_switch_and_rejects_permission_change()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let inspect = ExecTool::new(
        files.clone(),
        store.clone(),
        Arc::new(DummyGate),
        ExecConfig::default(),
    );
    let call = ToolCall {
        call_id: "a".into(),
        name: "exec".into(),
        arguments: serde_json::json!({"argv":["/bin/sh","-c","echo ready > marker; sleep 1"]})
            .to_string(),
    };
    let plan = inspect.plan(&call, &context)?;
    let operation = Operation::exec(&plan)?;
    let modes = ModeController::new(WorkMode::Work);
    let revision = PermissionRevision::new(modes.clone(), run.permissions);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        revision.clone(),
        store.clone(),
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let request = match manager
        .authorize(
            input(&operation, &call, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("pending".into()),
    };
    let permit = match manager.resolve(&request.approval_id, true)? {
        Authorization::Permit(p) => p,
        _ => return Err("permit".into()),
    };
    let exec = ExecTool::new(files, store, permit, ExecConfig::default());
    let controller = async {
        for _ in 0..200 {
            if temp.path().join("marker").exists() {
                modes.request(WorkMode::Plan)?;
                let snapshot = modes.snapshot()?;
                assert_eq!(snapshot.current, WorkMode::Work);
                assert_eq!(snapshot.pending, Some(WorkMode::Plan));
                assert!(!revision.change_if_idle(PermissionMode::Bypass)?);
                return Ok::<(), BoundaryError>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Err(BoundaryError(
            ErrorInfo {
                code: "test".into(),
                message: "command did not start".into(),
            },
            None,
            None,
        ))
    };
    let run_signal = Signal::default();
    let (result, control) = tokio::join!(exec.execute(&call, &context, &run_signal), controller);
    control?;
    assert!(result?.ok);
    assert_eq!(modes.snapshot()?.current, WorkMode::Plan);
    assert!(revision.change_if_idle(PermissionMode::Bypass)?);
    Ok(())
}

#[tokio::test]
async fn preauthorization_requires_exact_argv_and_cwd() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let other = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[other.path().into()],
        false,
        ReadLimits::default(),
    )?);
    let inspect = ExecTool::new(
        files,
        store.clone(),
        Arc::new(DummyGate),
        ExecConfig::default(),
    );
    let approved = command_input(temp.path(), "hello");
    let preauth = Preauthorizations::from_json(&approved, temp.path())?;
    let policy = Arc::new(Policy::new(
        temp.path(),
        PermissionConfig::from_toml(&format!("additional_roots=['{}']", other.path().display()))?,
    )?);
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store,
        &run,
        preauth,
        Arc::new(Signal::default()),
    )?;
    let mismatch = command_call("a", "hello-extra");
    let plan = inspect.plan(&mismatch, &context)?;
    let operation = Operation::exec(&plan)?;
    assert!(matches!(
        manager
            .authorize(
                input(&operation, &mismatch, &context, false),
                &deny_model(),
                &SharedBudget::default(),
                &config()
            )
            .await?,
        Authorization::NeedsConfirmation(_)
    ));
    let other_call = ToolCall {
        call_id: "b".into(),
        name: "exec".into(),
        arguments: serde_json::json!({"argv":["/bin/echo","hello"],"cwd":other.path()}).to_string(),
    };
    let plan = inspect.plan(&other_call, &context)?;
    let operation = Operation::exec(&plan)?;
    let response = manager
        .authorize(
            input(&operation, &other_call, &context, false),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?;
    let request = match response {
        Authorization::NeedsConfirmation(r) => r,
        _ => return Err("expected needs confirmation".into()),
    };
    assert_eq!(request.cwd, other.path().canonicalize()?);
    Ok(())
}

#[cfg(unix)]
struct SwapLinkGate {
    inner: Arc<saya_core::approvals::OneShotPermit>,
    link: std::path::PathBuf,
    target: std::path::PathBuf,
    checks: AtomicUsize,
}
#[cfg(unix)]
impl WriteGate for SwapLinkGate {
    fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        target: &Path,
    ) -> Result<bool, BoundaryError> {
        let result = WriteGate::authorize(self.inner.as_ref(), call, context, target)?;
        if result && self.checks.fetch_add(1, Ordering::SeqCst) == 1 {
            std::fs::remove_file(&self.link).map_err(|e| {
                BoundaryError(
                    ErrorInfo {
                        code: "test".into(),
                        message: e.to_string(),
                    },
                    None,
                    None,
                )
            })?;
            std::os::unix::fs::symlink(&self.target, &self.link).map_err(|e| {
                BoundaryError(
                    ErrorInfo {
                        code: "test".into(),
                        message: e.to_string(),
                    },
                    None,
                    None,
                )
            })?;
        }
        Ok(result)
    }
    fn finish(&self) -> Result<(), BoundaryError> {
        WriteGate::finish(self.inner.as_ref())
    }
}
#[cfg(unix)]
#[tokio::test]
async fn symlink_swap_after_authorization_cannot_redirect_a_write()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("good.txt"), "same")?;
    std::fs::write(temp.path().join("secret.txt"), "same")?;
    std::os::unix::fs::symlink("good.txt", temp.path().join("link.txt"))?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let version = files.read_file(Path::new("good.txt"), 1, 1)?.version;
    let rules = format!(
        "[[rules]]\neffect='deny'\ntool='write_file'\npath_glob='{}'\n",
        temp.path().canonicalize()?.join("secret.txt").display()
    );
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), &rules)?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let call = ToolCall {
        call_id: "swap".into(),
        name: "write_file".into(),
        arguments:
            serde_json::json!({"path":"link.txt","content":"modified","expected_version":version})
                .to_string(),
    };
    let operation = Operation::file(temp.path(), "write_file", Path::new("link.txt"))?;
    let request = match manager
        .authorize(
            input(&operation, &call, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("expected approval".into()),
    };
    let permit = match manager.resolve(&request.approval_id, true)? {
        Authorization::Permit(p) => p,
        _ => return Err("expected permit".into()),
    };
    let gate = Arc::new(SwapLinkGate {
        inner: permit,
        link: temp.path().join("link.txt"),
        target: temp.path().join("secret.txt"),
        checks: AtomicUsize::new(0),
    });
    let tool = WriteTool::new(
        files,
        store,
        gate,
        WriteToolKind::WriteFile,
        DEFAULT_WRITE_MAX_BYTES,
    );
    let result = tool.execute(&call, &context, &Signal::default()).await?;
    assert_eq!(
        result.error.as_ref().map(|e| e.code.as_str()),
        Some("conflict")
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("secret.txt"))?,
        "same"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("good.txt"))?,
        "same"
    );
    Ok(())
}

#[tokio::test]
async fn waiting_confirmation_is_invalidated_by_mode_or_policy_change()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    for change_mode in [true, false] {
        let modes = ModeController::new(WorkMode::Work);
        let revision = PermissionRevision::new(modes.clone(), run.permissions);
        let manager = manager(
            policy(temp.path(), "")?,
            modes.clone(),
            revision.clone(),
            store.clone(),
            &run,
            Preauthorizations::default(),
            Arc::new(Signal::default()),
        )?;
        let call = write_call(if change_mode { "a" } else { "b" }, "hello");
        let operation = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
        let request = match manager
            .authorize(
                input(&operation, &call, &context, true),
                &deny_model(),
                &SharedBudget::default(),
                &config(),
            )
            .await?
        {
            Authorization::Pending(r) => r,
            _ => return Err("pending".into()),
        };
        if change_mode {
            modes.request(WorkMode::Code)?;
        } else {
            assert!(revision.change_if_idle(PermissionMode::Bypass)?);
        }
        assert!(matches!(
            manager.resolve(&request.approval_id, true)?,
            Authorization::Stale
        ));
    }
    assert!(!temp.path().join("out.txt").exists());
    Ok(())
}

#[tokio::test]
async fn persisted_confirmation_redacts_known_secrets_and_does_not_store_grants()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let signal = Arc::new(Signal::default());
    let modes = ModeController::new(WorkMode::Work);
    let manager = ApprovalManager::new(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        ApprovalSetup {
            run_id: run.id,
            session_id: run.session_id,
            initial_permissions: run.permissions,
            preauthorizations: Preauthorizations::default(),
            known_secrets: vec!["secret-token".into()],
            cancellation: signal,
        },
    )?;
    let call = write_call("a", "secret-token");
    let operation = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
    let mut request_input = input(&operation, &call, &context, false);
    request_input.summary = "goal secret-token";
    let request = match manager
        .authorize(
            request_input,
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::NeedsConfirmation(r) => r,
        _ => return Err("needs confirmation".into()),
    };
    let serialized = serde_json::to_string(&request)?;
    assert!(!serialized.contains("secret-token"));
    assert!(serialized.contains("[REDACTED]"));
    assert_eq!(store.requests.lock().map_err(|_| "lock")?.len(), 1);
    assert!(!temp.path().join("out.txt").exists());
    Ok(())
}

#[tokio::test]
async fn duplicate_pending_is_reused_and_denial_invalidates_other_unspent_permit()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes.clone(), run.permissions),
        store.clone(),
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let op = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
    let first = write_call("a", "hello");
    let req_a = match manager
        .authorize(
            input(&op, &first, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("pending".into()),
    };
    let repeated = match manager
        .authorize(
            input(&op, &first, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("pending".into()),
    };
    assert_eq!(req_a.approval_id, repeated.approval_id);
    assert_eq!(store.requests.lock().map_err(|_| "lock")?.len(), 1);
    let second = write_call("b", "hello");
    let req_b = match manager
        .authorize(
            input(&op, &second, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(r) => r,
        _ => return Err("pending".into()),
    };
    let permit = match manager.resolve(&req_b.approval_id, true)? {
        Authorization::Permit(p) => p,
        _ => return Err("permit".into()),
    };
    assert!(matches!(
        manager.resolve(&req_a.approval_id, false)?,
        Authorization::Denied("user_denied")
    ));
    assert!(!WriteGate::authorize(
        permit.as_ref(),
        &second,
        &context,
        &temp.path().join("out.txt")
    )?);
    Ok(())
}

#[tokio::test]
async fn concurrent_identical_requests_share_one_confirmation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes, run.permissions),
        store.clone(),
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let call = write_call("same", "hello");
    let op = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
    let model = deny_model();
    let budget = SharedBudget::default();
    let config = config();
    let (a, b) = tokio::join!(
        manager.authorize(input(&op, &call, &context, true), &model, &budget, &config),
        manager.authorize(input(&op, &call, &context, true), &model, &budget, &config)
    );
    let (Authorization::Pending(a), Authorization::Pending(b)) = (a?, b?) else {
        return Err("expected pending confirmations".into());
    };
    assert_eq!(a.approval_id, b.approval_id);
    assert_eq!(store.requests.lock().map_err(|_| "lock")?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn permission_revision_cannot_use_another_run_controller()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, _) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let unrelated = ModeController::new(WorkMode::Work);
    let result = manager(
        policy(temp.path(), "")?,
        modes,
        PermissionRevision::new(unrelated, run.permissions),
        store,
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    );
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn mode_permission_matrix_drives_real_read_and_write_adapters()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        permissions::Decision,
        tools::read::{ReadTool, ReadToolKind},
    };
    for mode in [
        WorkMode::Plan,
        WorkMode::Build,
        WorkMode::Code,
        WorkMode::Work,
    ] {
        for permissions in [
            PermissionMode::Auto,
            PermissionMode::Review,
            PermissionMode::Bypass,
        ] {
            let temp = support::temp_workspace()?;
            std::fs::write(temp.path().join("input.txt"), "visible")?;
            let (mut session, mut run, mut context) = fixture(temp.path(), permissions);
            session.work_mode = mode;
            run.work_mode = mode;
            context.run = run.clone();
            let store = store_for(&session, &run).await?;
            let modes = ModeController::new(mode);
            let policy = policy(temp.path(), "")?;
            let manager = manager(
                policy.clone(),
                modes.clone(),
                PermissionRevision::new(modes, run.permissions),
                store.clone(),
                &run,
                Preauthorizations::default(),
                Arc::new(Signal::default()),
            )?;
            let files = Arc::new(ReadTools::new(
                temp.path(),
                &[],
                permissions == PermissionMode::Bypass,
                ReadLimits::default(),
            )?);
            let read = ToolCall {
                call_id: "read".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path":"input.txt", "start_line":1,"line_count":1})
                    .to_string(),
            };
            let read_op = Operation::file(temp.path(), "read_file", Path::new("input.txt"))?;
            assert_eq!(
                policy.evaluate(mode, permissions, &read_op).decision,
                Decision::Allow
            );
            let read_permit = match manager
                .authorize(
                    input(&read_op, &read, &context, true),
                    &deny_model(),
                    &SharedBudget::default(),
                    &config(),
                )
                .await?
            {
                Authorization::Permit(permit) => permit,
                _ => return Err("read should be allowed".into()),
            };
            assert!(
                ReadTool::with_gate(files.clone(), ReadToolKind::ReadFile, read_permit)
                    .execute(&read, &context, &Signal::default())
                    .await?
                    .ok
            );
            let write = write_call("write", "written");
            let write_op = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
            let auth = manager
                .authorize(
                    input(&write_op, &write, &context, true),
                    &deny_model(),
                    &SharedBudget::default(),
                    &config(),
                )
                .await?;
            let permit = match (mode, permissions, auth) {
                (WorkMode::Plan, _, Authorization::Denied(_)) => None,
                (_, PermissionMode::Review, Authorization::Pending(request)) => {
                    match manager.resolve(&request.approval_id, true)? {
                        Authorization::Permit(permit) => Some(permit),
                        _ => return Err("approval failed".into()),
                    }
                }
                (
                    _,
                    PermissionMode::Auto | PermissionMode::Bypass,
                    Authorization::Permit(permit),
                ) => Some(permit),
                _ => {
                    return Err(
                        format!("unexpected authorization for {mode:?}/{permissions:?}").into(),
                    );
                }
            };
            if let Some(permit) = permit {
                let adapter = WriteTool::new(
                    files,
                    store,
                    permit,
                    WriteToolKind::WriteFile,
                    DEFAULT_WRITE_MAX_BYTES,
                );
                assert!(
                    adapter
                        .execute(&write, &context, &Signal::default())
                        .await?
                        .ok
                );
                assert_eq!(
                    std::fs::read_to_string(temp.path().join("out.txt"))?,
                    "written"
                );
            } else {
                assert!(!temp.path().join("out.txt").exists());
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn changing_context_alone_cannot_elevate_permission_mode()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run, mut context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let revision = PermissionRevision::new(modes.clone(), run.permissions);
    let manager = manager(
        policy(temp.path(), "")?,
        modes,
        revision.clone(),
        store,
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let call = write_call("write", "hello");
    let op = Operation::file(temp.path(), "write_file", Path::new("out.txt"))?;
    context.run.permissions = PermissionMode::Bypass;
    let result = manager
        .authorize(
            input(&op, &call, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await;
    assert!(result.is_err());
    assert!(!temp.path().join("out.txt").exists());
    assert!(revision.change_if_idle(PermissionMode::Bypass)?);
    assert!(matches!(
        manager
            .authorize(
                input(&op, &call, &context, true),
                &deny_model(),
                &SharedBudget::default(),
                &config()
            )
            .await?,
        Authorization::Permit(_)
    ));
    Ok(())
}

#[tokio::test]
async fn read_gate_enforces_ask_one_shot_and_explicit_deny()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        permissions::PolicyGate,
        tools::read::{ReadTool, ReadToolKind},
    };
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("secret.txt"), "private")?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let rules = "[[rules]]\neffect='ask'\ntool='read_file'";
    let manager = manager(
        policy(temp.path(), rules)?,
        modes.clone(),
        PermissionRevision::new(modes, run.permissions),
        store,
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let call = ToolCall {
        call_id: "read".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path":"secret.txt", "start_line":1,"line_count":1})
            .to_string(),
    };
    let op = Operation::file(temp.path(), "read_file", Path::new("secret.txt"))?;
    let request = match manager
        .authorize(
            input(&op, &call, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Pending(request) => request,
        _ => return Err("expected read confirmation".into()),
    };
    let permit = match manager.resolve(&request.approval_id, true)? {
        Authorization::Permit(permit) => permit,
        _ => return Err("expected read permit".into()),
    };
    let tool = ReadTool::with_gate(files.clone(), ReadToolKind::ReadFile, permit);
    assert!(tool.execute(&call, &context, &Signal::default()).await?.ok);
    let second = tool.execute(&call, &context, &Signal::default()).await?;
    assert_eq!(
        second.error.as_ref().map(|e| e.code.as_str()),
        Some("permission_denied")
    );
    let deny = Arc::new(PolicyGate::new(policy(
        temp.path(),
        "[[rules]]\neffect='deny'\ntool='read_file'",
    )?));
    let tool = ReadTool::with_gate(files, ReadToolKind::ReadFile, deny);
    let blocked = tool.execute(&call, &context, &Signal::default()).await?;
    assert_eq!(
        blocked.error.as_ref().map(|e| e.code.as_str()),
        Some("permission_denied")
    );
    assert!(blocked.data.is_none());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn read_permit_rejects_a_retargeted_symlink() -> Result<(), Box<dyn std::error::Error>> {
    use saya_core::tools::read::{ReadTool, ReadToolKind};
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("a.txt"), "allowed")?;
    std::fs::write(temp.path().join("b.txt"), "secret")?;
    std::os::unix::fs::symlink("a.txt", temp.path().join("link.txt"))?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Auto);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let manager = manager(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes, run.permissions),
        store,
        &run,
        Preauthorizations::default(),
        Arc::new(Signal::default()),
    )?;
    let call = ToolCall {
        call_id: "read".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path":"link.txt", "start_line":1,"line_count":1})
            .to_string(),
    };
    let op = Operation::file(temp.path(), "read_file", Path::new("link.txt"))?;
    let permit = match manager
        .authorize(
            input(&op, &call, &context, true),
            &deny_model(),
            &SharedBudget::default(),
            &config(),
        )
        .await?
    {
        Authorization::Permit(permit) => permit,
        _ => return Err("expected permit".into()),
    };
    std::fs::remove_file(temp.path().join("link.txt"))?;
    std::os::unix::fs::symlink("b.txt", temp.path().join("link.txt"))?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let tool = ReadTool::with_gate(files, ReadToolKind::ReadFile, permit);
    let blocked = tool.execute(&call, &context, &Signal::default()).await?;
    assert_eq!(
        blocked.error.as_ref().map(|e| e.code.as_str()),
        Some("permission_denied")
    );
    assert!(blocked.data.is_none());
    Ok(())
}

async fn dispatch_call(
    dispatch: &saya_core::dispatch::ToolDispatcher,
    call: &ToolCall,
    context: &ToolContext,
    should_ask: bool,
) -> Result<saya_core::contract::ToolResult, Box<dyn std::error::Error>> {
    let auth = dispatch
        .authorize(
            call,
            context,
            "test task",
            true,
            saya_core::dispatch::DispatchReview {
                model: &deny_model(),
                budget: &SharedBudget::default(),
                config: &config(),
            },
        )
        .await?;
    let permit = match auth {
        Authorization::Permit(p) if !should_ask => p,
        Authorization::Pending(request) if should_ask => {
            match dispatch.resolve(&request.approval_id, true)? {
                Authorization::Permit(p) => p,
                _ => return Err("expected approved permit".into()),
            }
        }
        _ => return Err("unexpected authorization".into()),
    };
    Ok(dispatch.execute(call, context, permit).await?)
}

#[tokio::test]
async fn dispatcher_integrates_all_seven_tools_and_confirmation()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::dispatch::ToolDispatcher;
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("source.txt"), "old\n")?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let modes = ModeController::new(WorkMode::Work);
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let version = files.read_file(Path::new("source.txt"), 1, 1)?.version;
    let dispatch = ToolDispatcher::new(
        policy(temp.path(), "")?,
        modes.clone(),
        PermissionRevision::new(modes, run.permissions),
        store.clone(),
        ApprovalSetup {
            run_id: run.id,
            session_id: run.session_id,
            initial_permissions: run.permissions,
            preauthorizations: Preauthorizations::default(),
            known_secrets: vec![],
            cancellation: Arc::new(Signal::default()),
        },
        files,
        ExecConfig::default(),
    )?;
    for (id, name, args) in [
        (
            "r",
            "read_file",
            serde_json::json!({"path":"source.txt","start_line":1,"line_count":1}),
        ),
        ("l", "list_dir", serde_json::json!({"path":"."})),
        (
            "f",
            "find_files",
            serde_json::json!({"path":".","glob":"*.txt"}),
        ),
        (
            "s",
            "search_text",
            serde_json::json!({"path":".","pattern":"old"}),
        ),
    ] {
        let call = ToolCall {
            call_id: id.into(),
            name: name.into(),
            arguments: args.to_string(),
        };
        assert!(
            dispatch_call(&dispatch, &call, &context, false).await?.ok,
            "{name}"
        );
    }
    let write = write_call("w", "new file");
    assert!(dispatch_call(&dispatch, &write, &context, true).await?.ok);
    let patch = ToolCall { call_id: "p".into(), name: "apply_patch".into(), arguments: serde_json::json!({"path":"source.txt", "patch":"--- a/source.txt\n+++ b/source.txt\n@@ -1 +1 @@\n-old\n+new\n", "expected_version":version}).to_string() };
    assert!(dispatch_call(&dispatch, &patch, &context, true).await?.ok);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("source.txt"))?,
        "new\n"
    );
    let command = ToolCall {
        call_id: "x".into(),
        name: "exec".into(),
        arguments: serde_json::json!({"argv":["/bin/echo","ok"]}).to_string(),
    };
    let result = dispatch_call(&dispatch, &command, &context, true).await?;
    assert!(result.ok);
    assert_eq!(
        result
            .data
            .as_ref()
            .and_then(|v| v.get("stdout"))
            .and_then(|v| v.as_str()),
        Some("ok\n")
    );
    let history = store
        .load_session(session.id)
        .await?
        .ok_or("missing session")?
        .history;
    assert_eq!(history.len(), 7);
    Ok(())
}

#[tokio::test]
async fn dispatcher_rejects_foreign_permit_and_noninteractive_never_executes()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::dispatch::ToolDispatcher;
    let temp = support::temp_workspace()?;
    let (session, run, context) = fixture(temp.path(), PermissionMode::Review);
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let modes = ModeController::new(WorkMode::Work);
    let revision = PermissionRevision::new(modes.clone(), run.permissions);
    let setup = || ApprovalSetup {
        run_id: run.id,
        session_id: run.session_id,
        initial_permissions: run.permissions,
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        cancellation: Arc::new(Signal::default()),
    };
    let first = ToolDispatcher::new(
        policy(temp.path(), "")?,
        modes.clone(),
        revision.clone(),
        store.clone(),
        setup(),
        files.clone(),
        ExecConfig::default(),
    )?;
    let second = ToolDispatcher::new(
        policy(temp.path(), "[[rules]]\neffect='deny'\ntool='write_file'")?,
        modes,
        revision,
        store,
        setup(),
        files,
        ExecConfig::default(),
    )?;
    let call = write_call("unattended", "should not exist");
    let auth = first
        .authorize(
            &call,
            &context,
            "task",
            false,
            saya_core::dispatch::DispatchReview {
                model: &deny_model(),
                budget: &SharedBudget::default(),
                config: &config(),
            },
        )
        .await?;
    let request = match auth {
        Authorization::NeedsConfirmation(request) => request,
        _ => return Err("expected needs confirmation".into()),
    };
    assert!(!temp.path().join("out.txt").exists());
    let permit = match first.resolve(&request.approval_id, true)? {
        Authorization::Permit(p) => p,
        _ => return Err("expected permit".into()),
    };
    let result = second.execute(&call, &context, permit).await;
    assert!(
        matches!(result, Err(BoundaryError(ErrorInfo { code, .. }, _, _)) if code == "permission_denied")
    );
    assert!(!temp.path().join("out.txt").exists());
    let denied = second
        .authorize(
            &call,
            &context,
            "task",
            true,
            saya_core::dispatch::DispatchReview {
                model: &deny_model(),
                budget: &SharedBudget::default(),
                config: &config(),
            },
        )
        .await?;
    assert!(matches!(denied, Authorization::Denied(_)));
    Ok(())
}
