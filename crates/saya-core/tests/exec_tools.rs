mod support;
use saya_core::{
    boundary::{Cancellation, Operation, Store, StoreChange, Tool, ToolContext},
    contract::{
        BoundaryError, ErrorInfo, ModelProtocol, PermissionMode, Run, RunId, Session, SessionId,
        ToolCall, WorkMode,
    },
    memory::MemoryStore,
    tools::{
        exec::{ExecConfig, ExecGate, ExecPlan, ExecTool},
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct LaterCancel;
impl Cancellation for LaterCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
        })
    }
}
struct Gate {
    allowed: bool,
    calls: AtomicUsize,
}
impl ExecGate for Gate {
    fn authorize(
        &self,
        _: &ExecPlan,
        _: &ToolCall,
        _: &ToolContext,
    ) -> Result<bool, BoundaryError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.allowed)
    }
}
fn fixture(path: &Path) -> (Session, Run) {
    let session = Session {
        id: SessionId::new(),
        workspace: path.into(),
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
        permissions: PermissionMode::Auto,
    };
    (session, run)
}
async fn store_for(session: &Session, run: &Run) -> Result<Arc<MemoryStore>, BoundaryError> {
    let store = Arc::new(MemoryStore::default());
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    Ok(store)
}
fn call(id: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: id.into(),
        name: "exec".into(),
        arguments: args.to_string(),
    }
}
fn tool(
    root: &Path,
    store: Arc<dyn Store>,
    config: ExecConfig,
) -> Result<ExecTool, Box<dyn std::error::Error>> {
    Ok(ExecTool::new(
        Arc::new(ReadTools::new(root, &[], false, ReadLimits::default())?),
        store,
        Arc::new(Gate {
            allowed: true,
            calls: AtomicUsize::new(0),
        }),
        config,
    ))
}

#[tokio::test]
async fn executes_argv_without_implicit_shell_and_caps_both_streams()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let config = ExecConfig {
        stdout_max: 6,
        stderr_max: 5,
        ..ExecConfig::default()
    };
    let tool = tool(temp.path(), store, config)?;
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    let plain = tool
        .execute(
            &call(
                "a",
                serde_json::json!({"argv":["/bin/echo", "x;touch SHOULD_NOT_EXIST"]}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert!(plain.ok);
    let data = plain.data.ok_or("missing data")?;
    assert_eq!(data["stdout"], "x;touc");
    assert_eq!(data["stdout_truncated"], true);
    assert!(!temp.path().join("SHOULD_NOT_EXIST").exists());
    let both = tool.execute(&call("b", serde_json::json!({"argv":["/bin/sh","-c","printf 123456789; printf abcdefghi >&2"]})), &ctx, &NeverCancel).await?;
    assert!(both.ok);
    let data = both.data.ok_or("missing data")?;
    assert_eq!(data["stdout"], "123456");
    assert_eq!(data["stderr"], "abcde");
    assert_eq!(data["stdout_truncated"], true);
    assert_eq!(data["stderr_truncated"], true);
    Ok(())
}

#[tokio::test]
async fn filters_credentials_and_redacts_captured_output() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let secret = std::env::var("HOME")?;
    let mut config = ExecConfig::default();
    config.secret_env_names.insert("HOME".into());
    let tool = tool(temp.path(), store.clone(), config)?;
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    let result = tool
        .execute(
            &call("a", serde_json::json!({"argv":["/bin/echo",secret]})),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert!(result.ok);
    assert_eq!(
        result.data.as_ref().and_then(|v| v["stdout"].as_str()),
        Some("[REDACTED]\n")
    );
    let result = tool
        .execute(
            &call("b", serde_json::json!({"argv":["/usr/bin/env"]})),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert!(
        !result
            .data
            .as_ref()
            .and_then(|v| v["stdout"].as_str())
            .unwrap_or("")
            .lines()
            .any(|line| line.starts_with("HOME="))
    );
    let saved = store
        .load_session(session.id)
        .await?
        .ok_or("missing session")?;
    assert!(!format!("{:?}", saved.history).contains(&secret));
    Ok(())
}

struct FailIntent;
impl Store for FailIntent {
    fn load_session(&self, _: SessionId) -> Operation<'_, Option<Session>> {
        Box::pin(async { Ok(None) })
    }
    fn commit<'a>(&'a self, _: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async {
            Err(BoundaryError(
                ErrorInfo {
                    code: "storage_error".into(),
                    message: "injected".into(),
                },
                None,
                None,
            ))
        })
    }
}
#[tokio::test]
async fn plan_intent_failure_and_outside_cwd_never_spawn() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = support::temp_workspace()?;
    let external = support::temp_workspace()?;
    let (_, run) = fixture(temp.path());
    let tool = tool(temp.path(), Arc::new(FailIntent), ExecConfig::default())?;
    let ctx = ToolContext {
        run: run.clone(),
        workspace: temp.path().into(),
    };
    let task = call(
        "a",
        serde_json::json!({"argv":["/bin/sh","-c","touch marker"]}),
    );
    assert_eq!(
        tool.execute(&task, &ctx, &NeverCancel)
            .await
            .expect_err("intent failure")
            .0
            .code,
        "storage_error"
    );
    assert!(!temp.path().join("marker").exists());
    let plan = ToolContext {
        run: Run {
            work_mode: WorkMode::Plan,
            ..run
        },
        workspace: temp.path().into(),
    };
    assert_eq!(
        tool.execute(&task, &plan, &NeverCancel)
            .await?
            .error
            .as_ref()
            .map(|e| e.code.as_str()),
        Some("mode_denied")
    );
    let outside = tool
        .execute(
            &call(
                "b",
                serde_json::json!({"argv":["/bin/echo","x"],"cwd":external.path()}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        outside.error.as_ref().map(|e| e.code.as_str()),
        Some("path_denied")
    );
    Ok(())
}

#[tokio::test]
async fn exited_leader_cleans_up_descendant_with_closed_pipes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let tool = tool(temp.path(), store, ExecConfig::default())?;
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    let result = tool.execute(
        &call("descendant", serde_json::json!({"argv":["/bin/sh","-c","(sleep 1; printf escaped > marker) >/dev/null 2>&1 & exit 0"]})),
        &ctx,
        &NeverCancel,
    ).await?;
    assert!(result.ok, "{result:?}");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!temp.path().join("marker").exists());
    Ok(())
}

#[tokio::test]
async fn cancellation_terminates_process_group_and_records_result()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let tool = tool(temp.path(), store, ExecConfig::default())?;
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    let result = tool
        .execute(
            &call(
                "a",
                serde_json::json!({"argv":["/bin/sh","-c","sleep 30 & wait"]}),
            ),
            &ctx,
            &LaterCancel,
        )
        .await?;
    assert!(!result.ok);
    assert_eq!(
        result.data.as_ref().map(|v| &v["cancelled"]),
        Some(&serde_json::json!(true))
    );
    assert_eq!(
        result.error.as_ref().map(|e| e.code.as_str()),
        Some("cancelled")
    );
    Ok(())
}

#[tokio::test]
async fn timeout_force_kills_term_ignoring_group_after_grace_period()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let config = ExecConfig {
        default_timeout: Duration::from_secs(1),
        max_timeout: Duration::from_secs(2),
        ..ExecConfig::default()
    };
    let tool = tool(temp.path(), store, config)?;
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    // This child writes a marker after eight seconds unless the whole group is killed.
    let task = call(
        "a",
        serde_json::json!({"argv":["/bin/sh","-c","trap '' TERM; (sleep 8; echo escaped > marker) & wait"]}),
    );
    let start = std::time::Instant::now();
    let result = tool.execute(&task, &ctx, &NeverCancel).await?;
    assert_eq!(
        result.data.as_ref().map(|v| &v["timed_out"]),
        Some(&serde_json::json!(true))
    );
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert!(start.elapsed() < Duration::from_secs(9));
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!temp.path().join("marker").exists());
    Ok(())
}

struct FailResult {
    inner: Arc<MemoryStore>,
}
impl Store for FailResult {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            if changes
                .iter()
                .any(|c| matches!(c, StoreChange::ToolFinished { .. }))
            {
                return Err(BoundaryError(
                    ErrorInfo {
                        code: "storage_error".into(),
                        message: "result save failed".into(),
                    },
                    None,
                    None,
                ));
            }
            self.inner.commit(changes).await
        })
    }
}
#[tokio::test]
async fn failed_result_save_retains_the_observed_command_result()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = Arc::new(FailResult {
        inner: store_for(&session, &run).await?,
    });
    let tool = tool(temp.path(), store, ExecConfig::default())?;
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    let error = tool
        .execute(
            &call("a", serde_json::json!({"argv":["/bin/echo","hello"]})),
            &ctx,
            &NeverCancel,
        )
        .await
        .expect_err("must report storage failure");
    assert_eq!(error.0.code, "storage_error");
    assert_eq!(
        error
            .2
            .as_ref()
            .and_then(|r| r.data.as_ref())
            .and_then(|v| v["stdout"].as_str()),
        Some("hello\n")
    );
    Ok(())
}

#[tokio::test]
async fn excessive_timeout_and_secret_env_reference_are_rejected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let mut config = ExecConfig::default();
    config.secret_env_names.insert("HOME".into());
    let tool = tool(temp.path(), store, config)?;
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    let timeout = tool
        .execute(
            &call(
                "a",
                serde_json::json!({"argv":["/bin/echo","x"],"timeout_seconds":601}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        timeout.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_args")
    );
    let secret = tool.execute(&call("b", serde_json::json!({"argv":["/bin/echo","x"],"env":{"TEST":{"source":"reference","value":"HOME"}}})), &ctx, &NeverCancel).await?;
    assert_eq!(
        secret.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_args")
    );
    Ok(())
}
