use saya_core::{
    approvals::{ApprovalSetup, Authorization, PermissionRevision, Preauthorizations},
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, StoreChange,
        Tool, ToolContext,
    },
    contract::{
        ApprovalRequest, BoundaryError, ErrorInfo, HistoryItem, ModelProtocol, PermissionMode, Run,
        RunId, RunStatus, Session, SessionId, ToolCall, ToolResult, WorkMode,
    },
    dispatch::{DispatchReview, ToolDispatcher},
    memory::MemoryStore,
    mode::ModeController,
    permissions::{Decision, Operation as PermissionOperation, PermissionConfig, Policy},
    review::{ReviewConfig, SharedBudget},
    runtime::{AgentRuntime, ApprovalHandler, RuntimeLimits, RuntimeSetup},
    sqlite::SqliteStore,
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
        todo::{TodoItem, TodoStatus, TodoTool, current_todos, result_todos},
    },
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
};

struct Cancel(bool);
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if !self.0 {
                pending::<()>().await;
            }
        })
    }
}
fn error(code: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: code.into(),
        },
        None,
        None,
    )
}
fn fixture(root: &Path, mode: WorkMode) -> Session {
    Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: mode,
        profile: "fixture".into(),
        protocol: ModelProtocol::Responses,
        model: "fixture".into(),
        history: vec![HistoryItem::UserText(
            "Implement and verify the task".into(),
        )],
    }
}
fn context(session: &Session, permissions: PermissionMode) -> ToolContext {
    ToolContext {
        workspace: session.workspace.clone(),
        run: Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: session.work_mode,
            permissions,
        },
    }
}
fn todo(id: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        id: id.into(),
        content: format!("Task {id}"),
        status,
    }
}
fn call(id: &str, todos: &[TodoItem]) -> ToolCall {
    ToolCall {
        call_id: id.into(),
        name: "todo_write".into(),
        arguments: json!({"todos": todos}).to_string(),
    }
}
fn result(todos: &[TodoItem]) -> ToolResult {
    ToolResult {
        ok: true,
        data: Some(json!({"todos": todos})),
        error: None,
        truncated: false,
    }
}
fn pair(id: &str, todos: &[TodoItem]) -> Vec<HistoryItem> {
    vec![
        HistoryItem::ToolCall(call(id, todos)),
        HistoryItem::ToolResult {
            call_id: id.into(),
            result: result(todos),
        },
    ]
}

#[tokio::test]
async fn adapter_validates_limits_unicode_and_full_replacements()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let context = context(&fixture(root.path(), WorkMode::Plan), PermissionMode::Auto);
    let tool = TodoTool;
    let items = vec![TodoItem {
        id: "界".repeat(64),
        content: "文".repeat(500),
        status: TodoStatus::InProgress,
    }];
    let updated = tool
        .execute(&call("first", &items), &context, &Cancel(false))
        .await?;
    assert_eq!(result_todos(&updated), Some(items));
    assert_eq!(
        result_todos(
            &tool
                .execute(&call("clear", &[]), &context, &Cancel(false))
                .await?
        ),
        Some(vec![])
    );
    assert!(std::fs::read_dir(root.path())?.next().is_none());
    assert_eq!(
        tool.definition().input_schema["properties"]["todos"]["maxItems"],
        64
    );
    assert_eq!(
        tool.execute(&call("cancel", &[]), &context, &Cancel(true))
            .await
            .unwrap_err()
            .0
            .code,
        "cancelled"
    );

    let mut invalid = vec![
        json!({"todos": [todo("same", TodoStatus::Pending), todo("same", TodoStatus::Completed)]}),
        json!({"todos": [todo("a", TodoStatus::InProgress), todo("b", TodoStatus::InProgress)]}),
        json!({"todos": [{"id": "", "content": "text", "status": "pending"}]}),
        json!({"todos": [{"id": "id", "content": " \n ", "status": "pending"}]}),
        json!({"todos": [{"id": "id", "content": "text", "status": "unknown"}]}),
        json!({"todos": [{"id": "x".repeat(65), "content": "text", "status": "pending"}]}),
        json!({"todos": [{"id": "id", "content": "文".repeat(501), "status": "pending"}]}),
        json!({"todos": [], "unknown": "PRIVATE"}),
        json!({"todos": [{"id": "id", "content": "text", "status": "pending", "extra": true}]}),
        json!({"todos": (0..65).map(|id| todo(&id.to_string(), TodoStatus::Pending)).collect::<Vec<_>>()}),
    ];
    // This satisfies per-item character limits but exceeds the aggregate byte limit.
    invalid.push(json!({"todos": (0..64).map(|id| TodoItem { id: id.to_string(), content: "文".repeat(500), status: TodoStatus::Pending }).collect::<Vec<_>>()}));
    for value in invalid {
        assert!(
            result_todos(&ToolResult {
                data: Some(value.clone()),
                ..result(&[])
            })
            .is_none()
        );
        let call = ToolCall {
            call_id: "bad".into(),
            name: "todo_write".into(),
            arguments: value.to_string(),
        };
        let error = tool
            .execute(&call, &context, &Cancel(false))
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "invalid_args");
        assert!(!error.0.message.contains("PRIVATE"));
    }
    let mut duplicate = call("duplicate", &[]);
    duplicate.arguments = r#"{"todos":[],"todos":[]}"#.into();
    assert_eq!(
        tool.execute(&duplicate, &context, &Cancel(false))
            .await
            .unwrap_err()
            .0
            .code,
        "invalid_args"
    );
    Ok(())
}

#[test]
fn snapshots_require_matching_successful_pairs_and_preserve_previous_on_bad_history() {
    let first = vec![todo("first", TodoStatus::Pending)];
    let mut history = pair("one", &first);
    assert_eq!(current_todos(&history), first);
    let other = vec![todo("other", TodoStatus::Completed)];
    history.push(HistoryItem::UserText(json!({"todos": other}).to_string()));
    history.push(HistoryItem::ToolResult {
        call_id: "orphan".into(),
        result: result(&other),
    });
    history.push(HistoryItem::ToolResult {
        call_id: "one".into(),
        result: result(&other),
    });
    history.extend(pair("one", &other));
    assert_eq!(current_todos(&history), first);
    for malformed in [
        ToolResult {
            ok: false,
            ..result(&other)
        },
        ToolResult {
            truncated: true,
            ..result(&other)
        },
        ToolResult {
            error: Some(ErrorInfo {
                code: "failed".into(),
                message: "failed".into(),
            }),
            ..result(&other)
        },
        ToolResult {
            data: Some(json!({"todos": [], "extra": true})),
            ..result(&other)
        },
        ToolResult {
            data: Some(json!({"todos": [{"id": "x", "content": "x", "status": "invalid"}]})),
            ..result(&other)
        },
    ] {
        history.push(HistoryItem::ToolCall(call("failed", &other)));
        history.push(HistoryItem::ToolResult {
            call_id: "failed".into(),
            result: malformed,
        });
        assert_eq!(current_todos(&history), first);
    }
    let mut malformed = call("mismatched", &first);
    malformed.arguments = "{\"todos\":false}".into();
    history.push(HistoryItem::ToolCall(malformed));
    history.push(HistoryItem::ToolResult {
        call_id: "mismatched".into(),
        result: result(&other),
    });
    assert_eq!(current_todos(&history), first);
    history.extend(pair("two", &other));
    assert_eq!(current_todos(&history), other);
    history.extend(pair("clear", &[]));
    assert!(current_todos(&history).is_empty());
}

#[test]
fn canonical_redacted_results_survive_history_derivation_and_prompt_injection()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let mut session = fixture(root.path(), WorkMode::Work);
    let original = vec![TodoItem {
        id: "task".into(),
        content: "Check fixture-only-private-value".into(),
        status: TodoStatus::Pending,
    }];
    let saved = saya_core::redaction::tool_result(
        &result(&original),
        &["fixture-only-private-value".into()],
    )?;
    session.history.extend([
        HistoryItem::ToolCall(call("redacted", &original)),
        HistoryItem::ToolResult {
            call_id: "redacted".into(),
            result: saved.clone(),
        },
    ]);
    let canonical = vec![TodoItem {
        content: "Check [REDACTED]".into(),
        ..original[0].clone()
    }];
    assert_eq!(result_todos(&saved), Some(canonical.clone()));
    assert_eq!(current_todos(&session.history), canonical);
    let HistoryItem::UserText(data) =
        saya_core::prompt::runtime_context(&session, &context(&session, PermissionMode::Auto).run)?
    else {
        return Err("context".into());
    };
    assert!(!data.contains("fixture-only-private-value"));
    assert_eq!(
        serde_json::from_str::<Value>(&data)?["saya_runtime_context_v1"]["todos"],
        json!(canonical)
    );
    Ok(())
}

#[test]
fn todo_permissions_allow_plan_metadata_and_keep_manual_and_explicit_rules()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let policy = Policy::new(root.path(), PermissionConfig::default())?;
    let operation = PermissionOperation::todo_write();
    for mode in [
        WorkMode::Plan,
        WorkMode::Build,
        WorkMode::Code,
        WorkMode::Work,
    ] {
        for permission in [
            PermissionMode::Auto,
            PermissionMode::Bypass,
            PermissionMode::Manual,
        ] {
            assert_eq!(
                policy.evaluate(mode, permission, &operation).decision,
                if permission == PermissionMode::Manual {
                    Decision::Ask
                } else {
                    Decision::Allow
                }
            );
        }
    }
    let deny = Policy::new(
        root.path(),
        PermissionConfig::from_toml("[[rules]]\neffect='deny'\ntool='todo_write'\n")?,
    )?;
    for permission in [
        PermissionMode::Auto,
        PermissionMode::Bypass,
        PermissionMode::Manual,
    ] {
        assert_eq!(
            deny.evaluate(WorkMode::Plan, permission, &operation)
                .decision,
            Decision::Deny
        );
    }
    let ask = Policy::new(
        root.path(),
        PermissionConfig::from_toml("[[rules]]\neffect='ask'\ntool='todo_write'\n")?,
    )?;
    assert_eq!(
        ask.evaluate(WorkMode::Work, PermissionMode::Auto, &operation)
            .decision,
        Decision::Ask
    );
    Ok(())
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
    requests: Mutex<Vec<ModelRequest>>,
}
impl FakeModel {
    fn new(turns: Vec<Vec<HistoryItem>>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            requests: Mutex::new(vec![]),
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
            self.requests.lock().unwrap().push(request.clone());
            let turn = self
                .turns
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| error("unexpected_request"))?;
            Ok(Box::new(Stream(Some(turn))) as Box<dyn ModelStream>)
        })
    }
}
fn runtime(
    session: &Session,
    model: Arc<dyn Model>,
    store: Arc<dyn Store>,
    permission: PermissionMode,
    confirmation: Option<Arc<dyn ApprovalHandler>>,
) -> Result<AgentRuntime, BoundaryError> {
    AgentRuntime::new(RuntimeSetup {
        run: context(session, permission).run,
        model,
        store,
        policy: Arc::new(Policy::new(
            &session.workspace,
            PermissionConfig::default(),
        )?),
        files: Arc::new(
            ReadTools::new(&session.workspace, &[], false, ReadLimits::default())
                .map_err(|_| error("fixture"))?,
        ),
        cancellation: Arc::new(Cancel(false)),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "Implement and verify".into(),
        confirmation,
        limits: RuntimeLimits::default(),
    })
}
fn request_todos(request: &ModelRequest) -> Value {
    let HistoryItem::UserText(context) = &request.history[0] else {
        panic!("runtime context")
    };
    serde_json::from_str::<Value>(context).unwrap()["saya_runtime_context_v1"]["todos"].clone()
}

#[tokio::test]
async fn runtime_updates_reach_the_next_request_and_sqlite_resume_without_replaying()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let data = tempfile::tempdir()?;
    let mut session = fixture(root.path(), WorkMode::Plan);
    let first = vec![
        todo("plan", TodoStatus::InProgress),
        todo("verify", TodoStatus::Pending),
    ];
    let done = vec![
        todo("plan", TodoStatus::Completed),
        todo("verify", TodoStatus::Completed),
    ];
    let model = Arc::new(FakeModel::new(vec![
        vec![HistoryItem::ToolCall(call("create", &first))],
        vec![
            HistoryItem::AssistantText("Planning evidence gathered".into()),
            HistoryItem::ToolCall(call("complete", &done)),
        ],
        vec![HistoryItem::AssistantText("Verified plan ready".into())],
    ]));
    let store = Arc::new(SqliteStore::open(data.path(), vec![])?);
    let outcome = runtime(
        &session,
        model.clone(),
        store.clone(),
        PermissionMode::Auto,
        None,
    )?
    .run(&mut session)
    .await?;
    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(outcome.text.as_deref(), Some("Verified plan ready"));
    assert_eq!(current_todos(&session.history), done);
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(request_todos(&requests[0]), json!([]));
        assert_eq!(request_todos(&requests[1]), json!(first));
        assert_eq!(request_todos(&requests[2]), json!(done));
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|tool| tool.name == "todo_write")
        );
        assert!(
            !requests[0]
                .tools
                .iter()
                .any(|tool| tool.name == "write_file")
        );
        assert!(
            requests[0]
                .instructions
                .contains("Mark an item completed only when observed results")
        );
    }
    let id = session.id;
    drop(store);
    let store = Arc::new(SqliteStore::open(data.path(), vec![])?);
    let mut resumed = store.load_session(id).await?.ok_or("saved session")?;
    assert_eq!(current_todos(&resumed.history), done);
    let resumed_model = Arc::new(FakeModel::new(vec![vec![HistoryItem::AssistantText(
        "Awaiting your next task".into(),
    )]]));
    runtime(
        &resumed,
        resumed_model.clone(),
        store.clone(),
        PermissionMode::Auto,
        None,
    )?
    .run(&mut resumed)
    .await?;
    assert_eq!(
        request_todos(&resumed_model.requests.lock().unwrap()[0]),
        json!(done)
    );
    assert_eq!(
        resumed
            .history
            .iter()
            .filter(|item| matches!(item, HistoryItem::ToolCall(_)))
            .count(),
        2
    );
    assert!(std::fs::read_dir(root.path())?.next().is_none());
    Ok(())
}

struct Approve(Mutex<Vec<String>>);
impl ApprovalHandler for Approve {
    fn confirm<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            self.0.lock().unwrap().push(request.call_id.clone());
            Ok(Some(true))
        })
    }
}
#[tokio::test]
async fn manual_approval_is_required_for_each_todo_call_and_missing_confirmation_keeps_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let initial = vec![todo("old", TodoStatus::Pending)];
    let mut session = fixture(root.path(), WorkMode::Work);
    session.history.extend(pair("history", &initial));
    let model = Arc::new(FakeModel::new(vec![vec![HistoryItem::ToolCall(call(
        "ask",
        &[],
    ))]]));
    let outcome = runtime(
        &session,
        model,
        Arc::new(MemoryStore::default()),
        PermissionMode::Manual,
        None,
    )?
    .run(&mut session)
    .await?;
    assert_eq!(outcome.status, RunStatus::NeedsConfirmation);
    assert_eq!(current_todos(&session.history), initial);

    let mut session = fixture(root.path(), WorkMode::Plan);
    let model = Arc::new(FakeModel::new(vec![
        vec![
            HistoryItem::ToolCall(call("a", &initial)),
            HistoryItem::ToolCall(call("b", &[])),
        ],
        vec![HistoryItem::AssistantText("Cleared".into())],
    ]));
    let approval = Arc::new(Approve(Mutex::new(vec![])));
    let outcome = runtime(
        &session,
        model,
        Arc::new(MemoryStore::default()),
        PermissionMode::Manual,
        Some(approval.clone()),
    )?
    .run(&mut session)
    .await?;
    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(*approval.0.lock().unwrap(), ["a", "b"]);
    assert!(current_todos(&session.history).is_empty());
    Ok(())
}

#[tokio::test]
async fn malformed_todo_calls_return_paired_errors_and_allow_model_correction()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let mut session = fixture(root.path(), WorkMode::Code);
    let mut bad = call("invalid", &[]);
    bad.arguments = "{\"todos\":false}".into();
    let expected = vec![todo("fixed", TodoStatus::Pending)];
    let model = Arc::new(FakeModel::new(vec![
        vec![HistoryItem::ToolCall(bad)],
        vec![HistoryItem::ToolCall(call("valid", &expected))],
        vec![HistoryItem::AssistantText("Ready".into())],
    ]));
    let outcome = runtime(
        &session,
        model.clone(),
        Arc::new(MemoryStore::default()),
        PermissionMode::Auto,
        None,
    )?
    .run(&mut session)
    .await?;
    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(current_todos(&session.history), expected);
    let requests = model.requests.lock().unwrap();
    assert!(requests[1].history.iter().any(|item| matches!(item, HistoryItem::ToolResult { call_id, result } if call_id == "invalid" && result.error.as_ref().is_some_and(|error| error.code == "invalid_args"))));
    assert_eq!(request_todos(&requests[1]), json!([]));
    Ok(())
}

struct FailResultStore(MemoryStore);
impl Store for FailResultStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.0.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            if changes
                .iter()
                .any(|change| matches!(change, StoreChange::ToolFinished { .. }))
            {
                return Err(error("storage_error"));
            }
            self.0.commit(changes).await
        })
    }
}
#[tokio::test]
async fn failed_result_persistence_does_not_publish_a_new_task_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let mut session = fixture(root.path(), WorkMode::Work);
    let initial = vec![todo("old", TodoStatus::Pending)];
    session.history.extend(pair("history", &initial));
    let model = Arc::new(FakeModel::new(vec![vec![HistoryItem::ToolCall(call(
        "replace",
        &[],
    ))]]));
    let store = Arc::new(FailResultStore(MemoryStore::default()));
    let error = runtime(
        &session,
        model.clone(),
        store.clone(),
        PermissionMode::Auto,
        None,
    )?
    .run(&mut session)
    .await
    .unwrap_err();
    assert_eq!(error.0.code, "storage_error");
    assert!(error.2.is_some());
    let saved = store.load_session(session.id).await?.ok_or("saved")?;
    assert_eq!(current_todos(&saved.history), initial);
    assert_eq!(current_todos(&session.history), initial);
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert!(!saved.history.iter().any(
        |item| matches!(item, HistoryItem::ToolResult { call_id, .. } if call_id == "replace")
    ));
    Ok(())
}

#[tokio::test]
async fn todo_permits_bind_exact_arguments_and_are_single_use_across_mode_changes()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let session = fixture(root.path(), WorkMode::Work);
    let context = context(&session, PermissionMode::Auto);
    let cancellation = Arc::new(Signal(tokio::sync::watch::channel(false).0));
    let store = Arc::new(CancelIntentStore {
        inner: MemoryStore::default(),
        cancellation: cancellation.clone(),
    });
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(context.run.clone()),
        ])
        .await?;
    let modes = ModeController::new(WorkMode::Work);
    let revision = PermissionRevision::new(modes.clone(), PermissionMode::Auto);
    let dispatcher = ToolDispatcher::new(
        Arc::new(Policy::new(root.path(), PermissionConfig::default())?),
        modes.clone(),
        revision,
        store.clone(),
        ApprovalSetup {
            run_id: context.run.id,
            session_id: session.id,
            initial_permissions: PermissionMode::Auto,
            preauthorizations: Preauthorizations::default(),
            known_secrets: vec![],
            cancellation,
        },
        Arc::new(ReadTools::new(
            root.path(),
            &[],
            false,
            ReadLimits::default(),
        )?),
        ExecConfig::default(),
    )?;
    let model = FakeModel::new(vec![]);
    let budget = SharedBudget::new(4, std::time::Duration::from_secs(10));
    let config = ReviewConfig::new("fixture".into());
    let original = call("once", &[todo("a", TodoStatus::Pending)]);
    let Authorization::Permit(permit) = dispatcher
        .authorize(
            &original,
            &context,
            "task",
            false,
            DispatchReview {
                model: &model,
                budget: &budget,
                config: &config,
            },
        )
        .await?
    else {
        return Err("expected auto metadata permission".into());
    };
    let changed = call("once", &[todo("changed", TodoStatus::Completed)]);
    assert_eq!(
        dispatcher
            .execute(&changed, &context, permit.clone())
            .await
            .unwrap_err()
            .0
            .code,
        "stale_approval"
    );
    store
        .commit(&[StoreChange::History {
            session_id: session.id,
            item: HistoryItem::ToolCall(original.clone()),
        }])
        .await?;
    assert!(
        dispatcher
            .execute(&original, &context, permit.clone())
            .await?
            .ok
    );
    assert_eq!(
        dispatcher
            .execute(&original, &context, permit)
            .await
            .unwrap_err()
            .0
            .code,
        "stale_approval"
    );

    let next = call("mode-change", &[]);
    let Authorization::Permit(permit) = dispatcher
        .authorize(
            &next,
            &context,
            "task",
            false,
            DispatchReview {
                model: &model,
                budget: &budget,
                config: &config,
            },
        )
        .await?
    else {
        return Err("expected metadata permission".into());
    };
    modes.request(WorkMode::Plan)?;
    assert_eq!(
        dispatcher
            .execute(&next, &context, permit)
            .await
            .unwrap_err()
            .0
            .code,
        "stale_approval"
    );
    assert_eq!(
        current_todos(
            &store
                .load_session(session.id)
                .await?
                .ok_or("saved")?
                .history
        ),
        vec![todo("a", TodoStatus::Pending)]
    );
    let mut plan_context = context;
    plan_context.run.work_mode = WorkMode::Plan;
    let cancelled = call("cancel-during-intent", &[]);
    let Authorization::Permit(permit) = dispatcher
        .authorize(
            &cancelled,
            &plan_context,
            "task",
            false,
            DispatchReview {
                model: &model,
                budget: &budget,
                config: &config,
            },
        )
        .await?
    else {
        return Err("expected metadata permission".into());
    };
    store
        .commit(&[StoreChange::History {
            session_id: session.id,
            item: HistoryItem::ToolCall(cancelled.clone()),
        }])
        .await?;
    assert_eq!(
        dispatcher
            .execute(&cancelled, &plan_context, permit)
            .await
            .unwrap_err()
            .0
            .code,
        "cancelled"
    );
    let saved = store.load_session(session.id).await?.ok_or("saved")?;
    assert_eq!(
        current_todos(&saved.history),
        vec![todo("a", TodoStatus::Pending)]
    );
    assert!(saved.history.iter().any(|item| matches!(item, HistoryItem::ToolResult { call_id, result } if call_id == "cancel-during-intent" && result.error.as_ref().is_some_and(|error| error.code == "cancelled"))));
    assert!(model.requests.lock().unwrap().is_empty());
    Ok(())
}

struct Signal(tokio::sync::watch::Sender<bool>);
impl Cancellation for Signal {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut receiver = self.0.subscribe();
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}
struct CancelIntentStore {
    inner: MemoryStore,
    cancellation: Arc<Signal>,
}
impl Store for CancelIntentStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            self.inner.commit(changes).await?;
            if changes.iter().any(|change| matches!(change, StoreChange::ToolIntent { call, .. } if call.call_id == "cancel-during-intent")) {
                self.cancellation.0.send_replace(true);
            }
            Ok(())
        })
    }
}
