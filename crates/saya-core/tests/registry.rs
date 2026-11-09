use saya_core::{
    approvals::{ApprovalSetup, Authorization, PermissionRevision, Preauthorizations},
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, Operation as FutureResult, Store,
        StoreChange, Tool, ToolContext, ToolDefinition,
    },
    contract::{
        BoundaryError, ErrorInfo, ModelProtocol, PermissionMode, Run, RunId, Session, SessionId,
        ToolCall, ToolResult, WorkMode,
    },
    dispatch::{DispatchReview, ToolDispatcher},
    memory::MemoryStore,
    mode::ModeController,
    permissions::{Decision, Operation, PermissionConfig, Policy},
    registry::{ExternalAccess, ToolRegistry},
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use serde_json::{Value, json};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
struct Cancel(AtomicBool);
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct NoModel;
impl Model for NoModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> FutureResult<'a, Box<dyn ModelStream>> {
        Box::pin(async {
            Err(BoundaryError(
                ErrorInfo {
                    code: "fixture_model_error".into(),
                    message: "unavailable".into(),
                },
                None,
                None,
            ))
        })
    }
}
struct Adapter {
    schema: Value,
    events: Arc<Mutex<Vec<String>>>,
    pause: Option<Arc<ExecutionPause>>,
    uncertain: bool,
}
#[derive(Default)]
struct ExecutionPause {
    started: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl Tool for Adapter {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read.file".into(),
            description: "Server claims this is read-only".into(),
            input_schema: self.schema.clone(),
        }
    }
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> FutureResult<'a, ToolResult> {
        Box::pin(async move {
            assert!(!cancel.is_cancelled());
            assert_eq!(call.name, "read.file");
            self.events.lock().unwrap().push("execute".into());
            if let Some(pause) = &self.pause {
                pause.started.notify_one();
                pause.resume.notified().await;
            }
            if self.uncertain {
                return Err(BoundaryError(
                    ErrorInfo {
                        code: "outcome_unknown".into(),
                        message: "remote operation has no terminal result".into(),
                    },
                    None,
                    None,
                ));
            }
            Ok(ToolResult {
                ok: true,
                data: Some(json!({"text":"fixture-secret"})),
                error: None,
                truncated: false,
            })
        })
    }
}
fn adapter(schema: Value, events: Arc<Mutex<Vec<String>>>) -> Arc<dyn Tool> {
    Arc::new(Adapter {
        schema,
        events,
        pause: None,
        uncertain: false,
    })
}
fn schema() -> Value {
    json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false})
}

#[test]
fn host_classification_controls_plan_manual_and_bypass() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let policy = Policy::new(root.path(), PermissionConfig::default())?;
    for access in [ExternalAccess::ReadOnly, ExternalAccess::Unclassified] {
        let mut registry = ToolRegistry::default();
        let name = registry.register_external(
            "files",
            "generation-1",
            adapter(schema(), Arc::default()),
            access,
        )?;
        let operation = Operation::external(registry.get(&name).unwrap().descriptor())?;
        for mode in [
            WorkMode::Plan,
            WorkMode::Work,
            WorkMode::Code,
            WorkMode::Build,
        ] {
            for permissions in [
                PermissionMode::Auto,
                PermissionMode::Manual,
                PermissionMode::Bypass,
            ] {
                let expected = match (mode, access, permissions) {
                    (WorkMode::Plan, ExternalAccess::Unclassified, _) => Decision::Deny,
                    (_, _, PermissionMode::Manual) => Decision::Ask,
                    (_, ExternalAccess::Unclassified, PermissionMode::Auto) => {
                        Decision::ModelReview
                    }
                    _ => Decision::Allow,
                };
                assert_eq!(
                    policy.evaluate(mode, permissions, &operation).decision,
                    expected
                );
            }
        }
        let deny =
            PermissionConfig::from_toml(&format!("[[rules]]\ntool='{name}'\neffect='deny'\n"))?;
        let policy = Policy::new(root.path(), deny)?;
        assert_eq!(
            policy
                .evaluate(WorkMode::Work, PermissionMode::Bypass, &operation)
                .decision,
            Decision::Deny
        );
    }
    Ok(())
}

#[test]
fn schemas_are_offline_bounded_and_arguments_follow_the_discovered_contract()
-> Result<(), Box<dyn std::error::Error>> {
    for invalid in [
        json!({"type":"array"}),
        json!({"type":"object","$ref":"https://127.0.0.1:9/private"}),
        json!({"type":"object","$ref":"file:///etc/passwd"}),
        json!({"type":"object","$defs":{"item":{"type":"string"}},"properties":{"nested":{"$id":"https://example.invalid/nested","$defs":{"item":{"$ref":"#/$defs/item"}},"$ref":"#/$defs/item"}}}),
        json!({"type":"object","$defs":{"cycle":{"$ref":"#/$defs/cycle"}},"$ref":"#/$defs/cycle"}),
        json!({"type":"object","properties":{"a":{"type":"string","pattern":"(?=unsupported)"}}}),
        json!({"type":"object","description":"x".repeat(65537)}),
    ] {
        assert!(
            ToolRegistry::default()
                .register_external(
                    "files",
                    "v1",
                    adapter(invalid, Arc::default()),
                    ExternalAccess::Unclassified
                )
                .is_err()
        );
    }
    let mut registry = ToolRegistry::default();
    let name = registry.register_external(
        "files",
        "v1",
        adapter(schema(), Arc::default()),
        ExternalAccess::Unclassified,
    )?;
    let entry = registry.get(&name).unwrap();
    for args in [
        json!({}),
        json!({"value":4}),
        json!({"value":"x","extra":true}),
        json!({"__saya_operation":{},"value":"x"}),
    ] {
        assert!(
            entry
                .validate_arguments(&ToolCall {
                    call_id: "c".into(),
                    name: name.clone(),
                    arguments: args.to_string()
                })
                .is_err()
        );
    }
    entry.validate_arguments(&ToolCall {
        call_id: "c".into(),
        name: name.clone(),
        arguments: json!({"value":"ok"}).to_string(),
    })?;
    assert!(
        registry
            .register_external(
                "files",
                "v1",
                adapter(schema(), Arc::default()),
                ExternalAccess::ReadOnly
            )
            .is_err()
    );
    let fingerprint = registry
        .get(&name)
        .unwrap()
        .descriptor()
        .fingerprint
        .clone();
    assert!(
        registry
            .replace_external(
                "files",
                "v2",
                vec![(
                    adapter(json!({"type":"array"}), Arc::default()),
                    ExternalAccess::ReadOnly
                )]
            )
            .is_err()
    );
    assert_eq!(
        registry.get(&name).unwrap().descriptor().fingerprint,
        fingerprint
    );
    Ok(())
}

#[test]
fn credentials_in_external_catalogues_are_rejected_without_changing_schema() {
    let mut registry = ToolRegistry::with_known_secrets(vec!["fixture-secret".into()]);
    let shape = json!({"type":"object","properties":{"token":{"enum":["fixture-secret"]}}});
    assert_eq!(
        registry
            .register_external(
                "files",
                "v1",
                adapter(shape, Arc::default()),
                ExternalAccess::ReadOnly
            )
            .unwrap_err()
            .0
            .code,
        "credential_exposure"
    );
}

#[test]
fn aliases_are_stable_and_approval_identity_tracks_schema_access_and_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let register = |revision: &str, access, shape: Value| -> Result<_, BoundaryError> {
        let mut registry = ToolRegistry::default();
        let name = registry.register_external(
            "files_with_a_long_server_identifier",
            "v1",
            adapter(shape.clone(), Arc::default()),
            access,
        );
        assert!(name.is_err());
        let name = registry.register_external(
            "files",
            revision,
            adapter(shape, Arc::default()),
            access,
        )?;
        Ok((
            name.clone(),
            registry.get(&name).unwrap().descriptor().clone(),
        ))
    };
    let (name, first) = register("v1", ExternalAccess::Unclassified, schema())?;
    let (same, second) = register("v2", ExternalAccess::Unclassified, schema())?;
    assert_eq!(name, same);
    assert!(name.len() <= 64 && name.starts_with("mcp__files__"));
    assert_ne!(first.fingerprint, second.fingerprint);
    let (_, trusted) = register("v1", ExternalAccess::ReadOnly, schema())?;
    assert_ne!(first.fingerprint, trusted.fingerprint);
    let (_, changed_schema) =
        register("v1", ExternalAccess::Unclassified, json!({"type":"object"}))?;
    assert_ne!(first.fingerprint, changed_schema.fingerprint);
    let arguments = json!({"value":"x"});
    assert_ne!(
        Operation::external(&first)?.normalized_arguments(arguments.clone())?,
        Operation::external(&second)?.normalized_arguments(arguments)?
    );
    Ok(())
}

#[derive(Default)]
struct AuditStore {
    inner: MemoryStore,
    events: Arc<Mutex<Vec<String>>>,
    pause_intent: AtomicBool,
    intent_written: tokio::sync::Notify,
    resume_intent: tokio::sync::Notify,
}
impl Store for AuditStore {
    fn load_session(&self, id: SessionId) -> FutureResult<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> FutureResult<'a, ()> {
        Box::pin(async move {
            self.inner.commit(changes).await?;
            for change in changes {
                let event = match change {
                    StoreChange::ToolIntent { .. } => "intent",
                    StoreChange::ToolFinished { .. } => "finished",
                    StoreChange::ToolState {
                        state: saya_core::contract::ToolState::Unknown,
                        ..
                    } => "unknown",
                    _ => continue,
                };
                self.events.lock().unwrap().push(event.into());
                if event == "intent" && self.pause_intent.swap(false, Ordering::AcqRel) {
                    self.intent_written.notify_one();
                    self.resume_intent.notified().await;
                }
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn external_dispatch_uses_one_shot_approval_intent_result_and_redaction()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let session = Session {
        id: SessionId::new(),
        workspace: root.path().canonicalize()?,
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
        permissions: PermissionMode::Manual,
    };
    let context = ToolContext {
        run: run.clone(),
        workspace: session.workspace.clone(),
    };
    let events = Arc::new(Mutex::new(vec![]));
    let store = Arc::new(AuditStore {
        inner: MemoryStore::default(),
        events: events.clone(),
        ..Default::default()
    });
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    let modes = ModeController::new(WorkMode::Work);
    let dispatcher = ToolDispatcher::new(
        Arc::new(Policy::new(root.path(), PermissionConfig::default())?),
        modes.clone(),
        PermissionRevision::new(modes, run.permissions),
        store.clone(),
        ApprovalSetup {
            run_id: run.id,
            session_id: session.id,
            initial_permissions: run.permissions,
            preauthorizations: Preauthorizations::default(),
            known_secrets: vec!["fixture-secret".into()],
            cancellation: Arc::new(Cancel::default()),
        },
        Arc::new(ReadTools::new(
            root.path(),
            &[],
            false,
            ReadLimits::default(),
        )?),
        ExecConfig::default(),
    )?;
    let name = dispatcher.register_external(
        "files",
        "v1",
        adapter(schema(), events.clone()),
        ExternalAccess::Unclassified,
    )?;
    let call = ToolCall {
        call_id: "external-1".into(),
        name,
        arguments: json!({"value":"ok"}).to_string(),
    };
    let auth = dispatcher
        .authorize(
            &call,
            &context,
            "fixture task",
            true,
            DispatchReview {
                model: &NoModel,
                budget: &SharedBudget::default(),
                config: &ReviewConfig::new("fixture".into()),
            },
        )
        .await?;
    let Authorization::Pending(request) = auth else {
        return Err("manual approval expected".into());
    };
    assert!(events.lock().unwrap().is_empty());
    let Authorization::Permit(permit) = dispatcher.resolve(&request.approval_id, true)? else {
        return Err("permit expected".into());
    };
    let result = dispatcher.execute(&call, &context, permit.clone()).await?;
    assert!(result.ok);
    assert!(!serde_json::to_string(&result)?.contains("fixture-secret"));
    assert_eq!(*events.lock().unwrap(), ["intent", "execute", "finished"]);
    assert!(dispatcher.execute(&call, &context, permit).await.is_err());
    assert_eq!(*events.lock().unwrap(), ["intent", "execute", "finished"]);
    let mut next = call.clone();
    next.call_id = "external-2".into();
    let auth = dispatcher
        .authorize(
            &next,
            &context,
            "fixture task",
            true,
            DispatchReview {
                model: &NoModel,
                budget: &SharedBudget::default(),
                config: &ReviewConfig::new("fixture".into()),
            },
        )
        .await?;
    let Authorization::Pending(request) = auth else {
        return Err("manual approval expected".into());
    };
    let Authorization::Permit(permit) = dispatcher.resolve(&request.approval_id, true)? else {
        return Err("permit expected".into());
    };
    dispatcher.replace_external(
        "files",
        "v2",
        vec![(
            adapter(schema(), events.clone()),
            ExternalAccess::Unclassified,
        )],
    )?;
    assert!(dispatcher.execute(&next, &context, permit).await.is_err());
    assert_eq!(*events.lock().unwrap(), ["intent", "execute", "finished"]);
    next.call_id = "external-3".into();
    let auth = dispatcher
        .authorize(
            &next,
            &context,
            "fixture task",
            true,
            DispatchReview {
                model: &NoModel,
                budget: &SharedBudget::default(),
                config: &ReviewConfig::new("fixture".into()),
            },
        )
        .await?;
    let Authorization::Pending(request) = auth else {
        return Err("approval expected".into());
    };
    let Authorization::Permit(permit) = dispatcher.resolve(&request.approval_id, true)? else {
        return Err("permit expected".into());
    };
    let dispatcher = Arc::new(dispatcher);
    store.pause_intent.store(true, Ordering::Release);
    let task_dispatcher = dispatcher.clone();
    let task_context = context.clone();
    let task =
        tokio::spawn(async move { task_dispatcher.execute(&next, &task_context, permit).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.intent_written.notified(),
    )
    .await?;
    dispatcher.replace_external(
        "files",
        "v3",
        vec![(
            adapter(schema(), events.clone()),
            ExternalAccess::Unclassified,
        )],
    )?;
    store.resume_intent.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), task).await???;
    assert!(!result.ok);
    assert_eq!(
        result.error.as_ref().map(|error| error.code.as_str()),
        Some("stale_approval")
    );
    assert_eq!(
        *events.lock().unwrap(),
        ["intent", "execute", "finished", "intent", "finished"]
    );
    let pause = Arc::new(ExecutionPause::default());
    let blocked = Arc::new(Adapter {
        schema: schema(),
        events: events.clone(),
        pause: Some(pause.clone()),
        uncertain: false,
    });
    dispatcher.replace_external("files", "v4", vec![(blocked, ExternalAccess::Unclassified)])?;
    let mut active = call.clone();
    active.call_id = "external-4".into();
    let auth = dispatcher
        .authorize(
            &active,
            &context,
            "fixture task",
            true,
            DispatchReview {
                model: &NoModel,
                budget: &SharedBudget::default(),
                config: &ReviewConfig::new("fixture".into()),
            },
        )
        .await?;
    let Authorization::Pending(request) = auth else {
        return Err("approval expected".into());
    };
    let Authorization::Permit(permit) = dispatcher.resolve(&request.approval_id, true)? else {
        return Err("permit expected".into());
    };
    let task_dispatcher = dispatcher.clone();
    let task_context = context.clone();
    let task = tokio::spawn(async move {
        task_dispatcher
            .execute(&active, &task_context, permit)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.started.notified()).await?;
    let error = dispatcher
        .replace_external(
            "files",
            "v5",
            vec![(
                adapter(schema(), events.clone()),
                ExternalAccess::Unclassified,
            )],
        )
        .unwrap_err();
    assert_eq!(error.0.code, "registry_busy");
    pause.resume.notify_one();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await???
            .ok
    );
    dispatcher.replace_external(
        "files",
        "v5",
        vec![(
            adapter(schema(), events.clone()),
            ExternalAccess::Unclassified,
        )],
    )?;
    let uncertain = Arc::new(Adapter {
        schema: schema(),
        events: events.clone(),
        pause: None,
        uncertain: true,
    });
    dispatcher.replace_external(
        "files",
        "v6",
        vec![(uncertain, ExternalAccess::Unclassified)],
    )?;
    let mut lost = call;
    lost.call_id = "external-5".into();
    let auth = dispatcher
        .authorize(
            &lost,
            &context,
            "fixture task",
            true,
            DispatchReview {
                model: &NoModel,
                budget: &SharedBudget::default(),
                config: &ReviewConfig::new("fixture".into()),
            },
        )
        .await?;
    let Authorization::Pending(request) = auth else {
        return Err("approval expected".into());
    };
    let Authorization::Permit(permit) = dispatcher.resolve(&request.approval_id, true)? else {
        return Err("permit expected".into());
    };
    let result = dispatcher.execute(&lost, &context, permit).await?;
    assert_eq!(
        result.error.as_ref().map(|error| error.code.as_str()),
        Some("outcome_unknown")
    );
    assert_eq!(
        events.lock().unwrap().last().map(String::as_str),
        Some("unknown")
    );
    Ok(())
}
