//! End-to-end component tests for the DEV-079 web and browser surfaces.
use crate::{
    approvals::{ApprovalSetup, Authorization, PermissionRevision, Preauthorizations},
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, Operation as FutureResult, Store,
        StoreChange,
    },
    browser::{BrowserSettings, BrowserState, FixtureProvider},
    contract::{
        BoundaryError, ErrorInfo, HistoryItem, ModelProtocol, PermissionMode, Run, RunId, Session,
        SessionId, ToolCall, ToolState, WorkMode,
    },
    credentials::SecretRegistry,
    dispatch::{DispatchReview, ToolDispatcher},
    memory::MemoryStore,
    mode::ModeController,
    network::{NetworkError, NetworkErrorKind, NetworkResolver},
    output_store::{OutputCoverage, OutputLimits, OutputStore, OutputStream},
    permissions::{PermissionConfig, Policy},
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
    web::{WebRuntime, WebSearchSettings, WebSettings},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    future::Future,
    net::IpAddr,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[derive(Default)]
struct Cancel(AtomicBool);
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            while !self.0.load(Ordering::Acquire) {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
    }
}

#[derive(Default)]
struct RecordingStore {
    inner: MemoryStore,
    states: Mutex<Vec<ToolState>>,
}
impl Store for RecordingStore {
    fn load_session<'a>(&'a self, id: SessionId) -> FutureResult<'a, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> FutureResult<'a, ()> {
        Box::pin(async move {
            self.inner.commit(changes).await?;
            self.states
                .lock()
                .unwrap()
                .extend(changes.iter().filter_map(|change| match change {
                    StoreChange::ToolState { state, .. } => Some(*state),
                    _ => None,
                }));
            Ok(())
        })
    }
}

struct Finished(Option<crate::boundary::ModelUpdate>);
impl ModelStream for Finished {
    fn next<'a>(
        &'a mut self,
        _: &'a dyn Cancellation,
    ) -> FutureResult<'a, Option<crate::boundary::ModelUpdate>> {
        Box::pin(async move { Ok(self.0.take()) })
    }
}

struct DecisionModel {
    decision: &'static str,
}
impl Model for DecisionModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> FutureResult<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let item = HistoryItem::AssistantText(format!(
                "{{\"decision\":\"{}\",\"reason\":\"fixture review\"}}",
                self.decision
            ));
            Ok(
                Box::new(Finished(Some(crate::boundary::ModelUpdate::Finished {
                    items: vec![item],
                    usage: None,
                }))) as Box<dyn ModelStream>,
            )
        })
    }
}

fn run(mode: WorkMode, permissions: PermissionMode) -> Run {
    Run {
        id: RunId::new(),
        session_id: SessionId::new(),
        work_mode: mode,
        permissions,
    }
}

async fn dispatcher(
    root: &Path,
    run: &Run,
    rules: Option<&str>,
) -> Result<(ToolDispatcher, crate::boundary::ToolContext), BoundaryError> {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    dispatcher_with_store(root, run, rules, store).await
}

async fn dispatcher_with_store(
    root: &Path,
    run: &Run,
    rules: Option<&str>,
    store: Arc<dyn Store>,
) -> Result<(ToolDispatcher, crate::boundary::ToolContext), BoundaryError> {
    let config = match rules {
        Some(text) => PermissionConfig::from_toml(text)?,
        None => PermissionConfig::default(),
    };
    let policy = Arc::new(Policy::new(root, config)?);
    let modes = ModeController::new(run.work_mode);
    store
        .commit(&[
            StoreChange::Session(Session {
                id: run.session_id,
                workspace: root.to_path_buf(),
                work_mode: run.work_mode,
                profile: "fixture".into(),
                protocol: ModelProtocol::ChatCompletions,
                model: "fixture".into(),
                history: Vec::new(),
            }),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    let dispatcher = ToolDispatcher::new(
        policy,
        modes.clone(),
        PermissionRevision::new(modes, run.permissions),
        store,
        ApprovalSetup {
            run_id: run.id,
            session_id: run.session_id,
            initial_permissions: run.permissions,
            preauthorizations: Preauthorizations::default(),
            known_secrets: Vec::new(),
            cancellation: Arc::new(Cancel::default()),
        },
        Arc::new(
            ReadTools::new(
                root,
                &[],
                matches!(run.permissions, PermissionMode::Bypass),
                ReadLimits::default(),
            )
            .map_err(|error| {
                BoundaryError(
                    ErrorInfo {
                        code: error.code.into(),
                        message: error.message,
                    },
                    None,
                    None,
                )
            })?,
        ),
        ExecConfig::default(),
    )?;
    Ok((
        dispatcher,
        crate::boundary::ToolContext {
            run: run.clone(),
            workspace: root.to_path_buf(),
        },
    ))
}

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    ToolCall {
        call_id: format!(
            "call-{name}-{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::AcqRel)
        ),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}

async fn authorize(
    dispatcher: &ToolDispatcher,
    call: &ToolCall,
    context: &crate::boundary::ToolContext,
    decision: &'static str,
    interactive: bool,
) -> Authorization {
    let model = DecisionModel { decision };
    let budget = SharedBudget::default();
    let review = ReviewConfig::new("fixture".into());
    dispatcher
        .authorize(
            call,
            context,
            "fixture task",
            interactive,
            DispatchReview {
                model: &model,
                budget: &budget,
                config: &review,
            },
        )
        .await
        .unwrap()
}

async fn execute_one(
    dispatcher: &ToolDispatcher,
    call: &ToolCall,
    context: &crate::boundary::ToolContext,
    decision: &'static str,
    interactive: bool,
) -> crate::contract::ToolResult {
    let permit = match authorize(dispatcher, call, context, decision, interactive).await {
        Authorization::Permit(permit) => permit,
        Authorization::Pending(request) => {
            let Authorization::Permit(permit) =
                dispatcher.resolve(&request.approval_id, true).unwrap()
            else {
                panic!("approved request did not produce a permit");
            };
            permit
        }
        other => panic!(
            "authorization did not produce a permit: {}",
            match other {
                Authorization::Denied(reason) => reason,
                Authorization::NeedsConfirmation(_) => "needs confirmation",
                Authorization::Stale => "stale",
                _ => "unknown",
            }
        ),
    };
    dispatcher.execute(call, context, permit).await.unwrap()
}

fn register_web(
    dispatcher: &ToolDispatcher,
    settings: WebSettings,
    resolver: Option<Arc<dyn NetworkResolver>>,
) -> Result<(), BoundaryError> {
    let runtime = Arc::new(WebRuntime::new(settings, resolver)?);
    crate::web::register_tools(&runtime, dispatcher)
}

fn register_browser(
    dispatcher: &ToolDispatcher,
    run: &Run,
    provider: Arc<FixtureProvider>,
) -> Result<(), BoundaryError> {
    let state = Arc::new(BrowserState::new(
        BrowserSettings {
            enabled: true,
            ..Default::default()
        },
        provider,
        run.clone(),
    )?);
    crate::browser::register_tools(&state, dispatcher)
}

fn web_settings(origin: &str) -> WebSettings {
    let loopback = origin.starts_with("http://127.")
        || origin.starts_with("http://localhost:")
        || origin == "http://localhost";
    WebSettings {
        enabled: true,
        allowed_origins: vec![origin.into()],
        localhost_development_origin: loopback.then(|| origin.into()),
        max_redirects: 5,
        request_timeout_ms: 5000,
        max_response_bytes: 64 * 1024,
        max_content_chars: 16 * 1024,
        search: None,
    }
}

async fn scenario(
    root: &Path,
    mode: WorkMode,
    permissions: PermissionMode,
    rules: Option<&str>,
) -> (ToolDispatcher, crate::boundary::ToolContext) {
    let run = run(mode, permissions);
    let (dispatcher, context) = dispatcher(root, &run, rules).await.unwrap();
    register_web(&dispatcher, web_settings("https://example.com"), None).unwrap();
    register_browser(&dispatcher, &run, Arc::new(FixtureProvider::full())).unwrap();
    (dispatcher, context)
}

#[tokio::test]
async fn exact_tool_surface_and_plan_read_only_filtering() {
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Plan, PermissionMode::Auto);
    let (dispatcher, _) = dispatcher(temp.path(), &run, None).await.unwrap();
    register_web(
        &dispatcher,
        WebSettings {
            search: Some(WebSearchSettings {
                endpoint: "https://example.com/search".into(),
                max_results: 5,
            }),
            ..web_settings("https://example.com")
        },
        None,
    )
    .unwrap();
    register_browser(&dispatcher, &run, Arc::new(FixtureProvider::full())).unwrap();

    let names = |mode| {
        let mut names = dispatcher
            .definitions(mode)
            .unwrap()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let plan = names(WorkMode::Plan);
    assert!(plan.iter().any(|name| name == "fetch_url"));
    assert!(plan.iter().any(|name| name == "web_search"));
    assert!(!plan.iter().any(|name| name.starts_with("browser_")));
    let work = names(WorkMode::Work);
    for required in [
        "fetch_url",
        "web_search",
        "browser_open",
        "browser_close",
        "browser_navigate",
        "browser_snapshot",
        "browser_click",
        "browser_fill",
        "browser_press",
        "browser_wait",
        "browser_screenshot",
        "browser_download",
        "browser_upload",
    ] {
        assert!(work.iter().any(|name| name == required), "{required}");
    }
    assert!(!work.iter().any(|name| name.contains("selector")));
    assert!(!work.iter().any(|name| name == "evaluate"));
}

#[tokio::test]
async fn web_search_is_registered_only_with_an_explicit_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, _) = dispatcher(temp.path(), &run, None).await.unwrap();
    register_web(&dispatcher, web_settings("https://example.com"), None).unwrap();
    let names = dispatcher
        .definitions(WorkMode::Work)
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&"fetch_url".to_string()));
    assert!(!names.contains(&"web_search".to_string()));
}

#[tokio::test]
async fn permission_matrix_for_web_and_browser_effects() {
    use crate::registry::{ToolEffect, builtin_effect};

    assert_eq!(
        builtin_effect("browser_open"),
        Some(ToolEffect::BrowserWrite)
    );
    assert_eq!(
        builtin_effect("browser_close"),
        Some(ToolEffect::BrowserWrite)
    );
    assert_eq!(
        builtin_effect("browser_navigate"),
        Some(ToolEffect::BrowserWrite)
    );
    assert_eq!(
        builtin_effect("browser_snapshot"),
        Some(ToolEffect::BrowserRead)
    );
    assert_eq!(
        builtin_effect("browser_wait"),
        Some(ToolEffect::BrowserRead)
    );
    assert_eq!(
        builtin_effect("browser_screenshot"),
        Some(ToolEffect::BrowserRead)
    );
    for name in [
        "browser_click",
        "browser_fill",
        "browser_press",
        "browser_download",
        "browser_upload",
    ] {
        assert_eq!(
            builtin_effect(name),
            Some(ToolEffect::BrowserWrite),
            "{name}"
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let fetch = call("fetch_url", json!({"url":"https://example.com"}));
    let open = call("browser_open", json!({}));
    let navigate = call(
        "browser_navigate",
        json!({"page":"page_abc","url":"https://example.com"}),
    );
    let snapshot = call("browser_snapshot", json!({"page":"page_abc"}));
    let click = call(
        "browser_click",
        json!({"page":"page_abc","document":"doc_abc","element_ref":"el_abc"}),
    );

    // PLAN allows WebRead only; every browser effect is denied.
    let (dispatcher, context) =
        scenario(temp.path(), WorkMode::Plan, PermissionMode::Auto, None).await;
    assert!(matches!(
        authorize(&dispatcher, &fetch, &context, "allow", true).await,
        Authorization::Permit(_)
    ));
    for browser in [&open, &navigate, &snapshot, &click] {
        assert!(matches!(
            authorize(&dispatcher, browser, &context, "allow", true).await,
            Authorization::Denied("policy_denied")
        ));
    }
    // PLAN remains read-only in bypass mode.
    let (dispatcher, context) =
        scenario(temp.path(), WorkMode::Plan, PermissionMode::Bypass, None).await;
    for browser in [&open, &snapshot, &click] {
        assert!(matches!(
            authorize(&dispatcher, browser, &context, "allow", true).await,
            Authorization::Denied("policy_denied")
        ));
    }

    // auto allows WebRead/BrowserRead; BrowserWrite goes through review.
    let (dispatcher, context) =
        scenario(temp.path(), WorkMode::Work, PermissionMode::Auto, None).await;
    assert!(matches!(
        authorize(&dispatcher, &fetch, &context, "allow", true).await,
        Authorization::Permit(_)
    ));
    assert!(matches!(
        authorize(&dispatcher, &snapshot, &context, "allow", true).await,
        Authorization::Permit(_)
    ));
    for write in [&open, &navigate, &click] {
        let Authorization::Permit(permit) =
            authorize(&dispatcher, write, &context, "allow", true).await
        else {
            panic!("expected reviewed browser write permit");
        };
        assert_eq!(
            permit.source(),
            crate::approvals::PermitSource::IndependentModelReview
        );
    }

    // manual asks for each call, including reads.
    let (dispatcher, context) =
        scenario(temp.path(), WorkMode::Work, PermissionMode::Manual, None).await;
    for browser in [&open, &snapshot, &click] {
        assert!(matches!(
            authorize(&dispatcher, browser, &context, "allow", true).await,
            Authorization::Pending(_)
        ));
    }

    // explicit deny wins over auto and bypass for built-in effects.
    let (dispatcher, context) = scenario(
        temp.path(),
        WorkMode::Work,
        PermissionMode::Auto,
        Some("[[rules]]\neffect = \"deny\"\ntool = \"browser_click\"\n"),
    )
    .await;
    assert!(matches!(
        authorize(&dispatcher, &click, &context, "allow", true).await,
        Authorization::Denied("policy_denied")
    ));

    // bypass allows browser writes without review, but PLAN still denies.
    let (dispatcher, context) =
        scenario(temp.path(), WorkMode::Work, PermissionMode::Bypass, None).await;
    for browser in [&open, &navigate, &snapshot, &click] {
        assert!(matches!(
            authorize(&dispatcher, browser, &context, "allow", true).await,
            Authorization::Permit(_)
        ));
    }
}

#[tokio::test]
async fn browser_dispatch_round_trip_uses_opaque_references_and_capture() {
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let limits = OutputLimits {
        max_object_bytes: 4096,
        max_run_bytes: 8192,
        max_store_bytes: 65536,
        max_objects: 8,
        tail_bytes: 8,
        preview_bytes: 8,
        max_page_bytes: 8192,
        max_page_lines: 64,
        max_search_matches: 8,
    };
    let outputs = Arc::new(
        OutputStore::open(
            temp.path().join("outputs"),
            limits,
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    dispatcher.set_output_store(outputs.clone()).unwrap();
    let state = Arc::new(
        BrowserState::new(
            BrowserSettings {
                enabled: true,
                max_snapshot_bytes: 1024,
                ..Default::default()
            },
            Arc::new(FixtureProvider::full()),
            run.clone(),
        )
        .unwrap(),
    );
    crate::browser::register_tools(&state, &dispatcher).unwrap();

    let opened = execute_one(
        &dispatcher,
        &call("browser_open", json!({})),
        &context,
        "allow",
        true,
    )
    .await;
    let page = opened.data.as_ref().unwrap()["page"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(page.starts_with("page_"));
    let snapshot = execute_one(
        &dispatcher,
        &call("browser_snapshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let data = snapshot.data.as_ref().unwrap();
    assert_eq!(data["source"], "untrusted_browser");
    assert_eq!(data["page"], page);
    assert!(data["document"].as_str().unwrap().starts_with("doc_"));
    let document = data["document"].as_str().unwrap().to_string();
    let element = data["elements"][0]["element_ref"].as_str().unwrap();
    assert!(!element.contains("fixture-element"));
    let output_ref = data["output_refs"][0]["output_ref"].as_str().unwrap();
    assert!(!data["text"].as_str().unwrap_or("").contains("Fixture page"));
    let metadata = outputs.describe(run.session_id, output_ref).unwrap();
    assert_eq!(metadata.scope.stream, OutputStream::Browser);
    assert_eq!(metadata.coverage, OutputCoverage::Complete);
    assert!(metadata.artifact_complete);
    assert!(!snapshot.truncated);
    // The OutputStore capture includes the host projection, not only body text.
    let captured = outputs
        .read(
            run.session_id,
            output_ref,
            crate::output_store::ReadOptions {
                cursor: None,
                max_bytes: 8192,
                max_lines: 64,
            },
        )
        .unwrap();
    let captured = captured
        .chunks
        .into_iter()
        .map(|chunk| chunk.text)
        .collect::<String>();
    assert_eq!(metadata.capture_limit_bytes, 1024);
    for token in ["document", "doc_", "element_ref"] {
        assert!(
            captured.contains(token),
            "capture missing {token}: {captured:?}"
        );
    }

    let clicked = execute_one(
        &dispatcher,
        &call(
            "browser_click",
            json!({"page":page,"document":document,"element_ref":element}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    assert_eq!(clicked.data.as_ref().unwrap()["document_stale"], true);
    assert!(clicked.data.as_ref().unwrap()["document_revision"].is_u64());
    let stale = execute_one(
        &dispatcher,
        &call(
            "browser_click",
            json!({"page":page,"document":document,"element_ref":element}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    assert_eq!(stale.error.as_ref().unwrap().code, "stale_element");
}

#[tokio::test]
async fn unknown_browser_action_is_not_replayed_by_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let provider = Arc::new(FixtureProvider::full());
    register_browser(&dispatcher, &run, provider.clone()).unwrap();
    let opened = execute_one(
        &dispatcher,
        &call("browser_open", json!({})),
        &context,
        "allow",
        true,
    )
    .await;
    let page = opened.data.as_ref().unwrap()["page"]
        .as_str()
        .unwrap()
        .to_string();
    let snapshot = execute_one(
        &dispatcher,
        &call("browser_snapshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let element = snapshot.data.as_ref().unwrap()["elements"][0]["element_ref"]
        .as_str()
        .unwrap()
        .to_string();
    let document = snapshot.data.as_ref().unwrap()["document"]
        .as_str()
        .unwrap()
        .to_string();
    provider.fail_next_unknown();
    let unknown = execute_one(
        &dispatcher,
        &call(
            "browser_click",
            json!({"page":page,"document":document,"element_ref":element}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    assert_eq!(unknown.error.as_ref().unwrap().code, "outcome_unknown");
    let replay = execute_one(
        &dispatcher,
        &call(
            "browser_navigate",
            json!({"page":page,"url":"https://example.com"}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    assert_eq!(replay.error.as_ref().unwrap().code, "outcome_unknown");
    let recovered = execute_one(
        &dispatcher,
        &call("browser_snapshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    assert!(recovered.ok);
}

#[tokio::test]
async fn cancelled_dispatched_browser_action_persists_unknown_state() {
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let store = Arc::new(RecordingStore::default());
    let (dispatcher, context) = dispatcher_with_store(temp.path(), &run, None, store.clone())
        .await
        .unwrap();
    let provider = Arc::new(FixtureProvider::full());
    let state = Arc::new(
        BrowserState::new(
            BrowserSettings {
                enabled: true,
                ..Default::default()
            },
            provider.clone(),
            run.clone(),
        )
        .unwrap(),
    );
    crate::browser::register_tools(&state, &dispatcher).unwrap();
    let opened = execute_one(
        &dispatcher,
        &call("browser_open", json!({})),
        &context,
        "allow",
        true,
    )
    .await;
    let page = opened.data.as_ref().unwrap()["page"]
        .as_str()
        .unwrap()
        .to_string();

    provider.stall_next_action();
    let navigate = call(
        "browser_navigate",
        json!({"page":page,"url":"https://example.com"}),
    );
    let permit = match authorize(&dispatcher, &navigate, &context, "allow", true).await {
        Authorization::Permit(permit) => permit,
        _ => panic!("authorization did not produce a permit"),
    };
    let cancel = Arc::new(Cancel::default());
    let trigger = cancel.clone();
    let task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        trigger.0.store(true, Ordering::Release);
    });
    let result = dispatcher
        .execute_with_cancel(&navigate, &context, permit, cancel.as_ref(), false)
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(result.error.as_ref().unwrap().code, "outcome_unknown");
    assert!(
        store.states.lock().unwrap().contains(&ToolState::Unknown),
        "cancelled dispatch was not persisted as unknown"
    );
}

#[tokio::test]
async fn provider_truncated_snapshot_uses_limit_reached() {
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let outputs = Arc::new(
        OutputStore::open(
            temp.path().join("outputs"),
            OutputLimits {
                max_object_bytes: 4096,
                max_run_bytes: 8192,
                max_store_bytes: 65536,
                max_objects: 8,
                tail_bytes: 8,
                preview_bytes: 8,
                max_page_bytes: 8192,
                max_page_lines: 64,
                max_search_matches: 8,
            },
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    dispatcher.set_output_store(outputs.clone()).unwrap();
    let provider = Arc::new(FixtureProvider::full());
    let state = Arc::new(
        BrowserState::new(
            BrowserSettings {
                enabled: true,
                ..Default::default()
            },
            provider.clone(),
            run.clone(),
        )
        .unwrap(),
    );
    crate::browser::register_tools(&state, &dispatcher).unwrap();
    let opened = execute_one(
        &dispatcher,
        &call("browser_open", json!({})),
        &context,
        "allow",
        true,
    )
    .await;
    let page = opened.data.as_ref().unwrap()["page"]
        .as_str()
        .unwrap()
        .to_string();
    provider.truncate_next_snapshot();
    let snapshot = execute_one(
        &dispatcher,
        &call("browser_snapshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let data = snapshot.data.as_ref().unwrap();
    assert_eq!(data["truncated"], true);
    assert!(snapshot.truncated);
    let output_ref = data["output_refs"][0]["output_ref"].as_str().unwrap();
    let metadata = outputs.describe(run.session_id, output_ref).unwrap();
    assert_eq!(
        metadata.completion,
        crate::output_store::CaptureCompletion::LimitReached
    );
    assert!(!metadata.artifact_complete);
}

#[tokio::test]
async fn browser_snapshot_capture_uses_configured_snapshot_limit() {
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let outputs = Arc::new(
        OutputStore::open(
            temp.path().join("outputs"),
            OutputLimits {
                max_object_bytes: 4096,
                max_run_bytes: 8192,
                max_store_bytes: 65536,
                max_objects: 8,
                tail_bytes: 4,
                preview_bytes: 8,
                max_page_bytes: 8192,
                max_page_lines: 64,
                max_search_matches: 8,
            },
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    dispatcher.set_output_store(outputs.clone()).unwrap();
    let state = Arc::new(
        BrowserState::new(
            BrowserSettings {
                enabled: true,
                max_snapshot_bytes: 8,
                ..Default::default()
            },
            Arc::new(FixtureProvider::full()),
            run.clone(),
        )
        .unwrap(),
    );
    crate::browser::register_tools(&state, &dispatcher).unwrap();
    let opened = execute_one(
        &dispatcher,
        &call("browser_open", json!({})),
        &context,
        "allow",
        true,
    )
    .await;
    let page = opened.data.as_ref().unwrap()["page"]
        .as_str()
        .unwrap()
        .to_string();
    let snapshot = execute_one(
        &dispatcher,
        &call("browser_snapshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let output_ref = snapshot.data.as_ref().unwrap()["output_refs"][0]["output_ref"]
        .as_str()
        .unwrap();
    let metadata = outputs.describe(run.session_id, output_ref).unwrap();
    assert_eq!(metadata.capture_limit_bytes, 8);
}

#[tokio::test]
async fn withheld_browser_capture_falls_back_to_inline_text() {
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let outputs = Arc::new(
        OutputStore::open(
            temp.path().join("outputs"),
            OutputLimits {
                max_object_bytes: 16,
                max_run_bytes: 16,
                max_store_bytes: 64,
                max_objects: 8,
                tail_bytes: 8,
                preview_bytes: 8,
                max_page_bytes: 8192,
                max_page_lines: 64,
                max_search_matches: 8,
            },
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    dispatcher.set_output_store(outputs.clone()).unwrap();
    let state = Arc::new(
        BrowserState::new(
            BrowserSettings {
                enabled: true,
                max_snapshot_bytes: 16,
                ..Default::default()
            },
            Arc::new(FixtureProvider::full()),
            run.clone(),
        )
        .unwrap(),
    );
    crate::browser::register_tools(&state, &dispatcher).unwrap();
    let opened = execute_one(
        &dispatcher,
        &call("browser_open", json!({})),
        &context,
        "allow",
        true,
    )
    .await;
    let page = opened.data.as_ref().unwrap()["page"]
        .as_str()
        .unwrap()
        .to_string();
    // The first snapshot exhausts this Run's retention quota.
    execute_one(
        &dispatcher,
        &call("browser_snapshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let second = execute_one(
        &dispatcher,
        &call("browser_snapshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let data = second.data.as_ref().unwrap();
    assert_eq!(data["truncated"], true);
    assert!(second.truncated);
    assert!(
        data["text"].as_str().is_some_and(|text| !text.is_empty()),
        "withheld capture returned no inline observation: {data}"
    );
    let output_ref = data["output_refs"][0]["output_ref"].as_str().unwrap();
    let metadata = outputs.describe(run.session_id, output_ref).unwrap();
    assert_eq!(metadata.retained_bytes, 0);
    assert!(metadata.withheld_reason.is_some());
}

#[tokio::test]
async fn long_search_projection_keeps_only_result_metadata() {
    let long_title = "T".repeat(2048);
    let body = format!(
        r#"{{"results":[{{"title":"{long_title}","url":"https://example.com/one","snippet":"first"}},{{"title":"{long_title}","url":"https://example.com/two","snippet":"second"}}]}}"#
    );
    let (port, server) = spawn_server(vec![response(
        "200 OK",
        "application/json",
        body.as_bytes(),
    )])
    .await;
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    register_web(
        &dispatcher,
        WebSettings {
            max_content_chars: 32 * 1024,
            search: Some(WebSearchSettings {
                endpoint: format!("http://127.0.0.1:{port}/search"),
                max_results: 5,
            }),
            ..web_settings(&format!("http://127.0.0.1:{port}"))
        },
        None,
    )
    .unwrap();
    let result = execute_one(
        &dispatcher,
        &call("web_search", json!({"query":"rust"})),
        &context,
        "allow",
        true,
    )
    .await;
    let data = result.data.as_ref().unwrap();
    assert_eq!(data["result_count"], 2);
    assert!(
        data.get("results").is_none(),
        "search projection retained the full results array: {data}"
    );
    server.await.unwrap();
}

// ---------------------------------------------------------------------------
// Controlled local HTTP fixtures
// ---------------------------------------------------------------------------

struct FixedResolver {
    answers: BTreeMap<String, Vec<IpAddr>>,
    fail: bool,
}
impl NetworkResolver for FixedResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        _: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, NetworkError>> + Send + 'a>> {
        Box::pin(async move {
            if self.fail {
                return Err(NetworkError {
                    kind: NetworkErrorKind::DnsFailed,
                });
            }
            Ok(self.answers.get(host).cloned().unwrap_or_default())
        })
    }
}

struct HangingResolver;
impl NetworkResolver for HangingResolver {
    fn resolve<'a>(
        &'a self,
        _: &'a str,
        _: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, NetworkError>> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

async fn spawn_server(responses: Vec<Vec<u8>>) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut head = Vec::new();
            let mut buffer = [0u8; 1024];
            while let Ok(read) = stream.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                head.extend_from_slice(&buffer[..read]);
                if head.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = stream.write_all(&response).await;
            let _ = stream.shutdown().await;
        }
    });
    (port, handle)
}

fn response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut value = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if status.starts_with("30") {
        value.push_str("Location: /next\r\n");
    }
    value.push_str("\r\n");
    value
        .into_bytes()
        .into_iter()
        .chain(body.iter().copied())
        .collect()
}

fn redirected(location: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

#[tokio::test]
async fn fetch_pins_resolved_loopback_and_marks_untrusted_text() {
    let (port, server) = spawn_server(vec![response(
        "200 OK",
        "text/html; charset=utf-8",
        b"<p>Hello <b>pinned</b> world</p>",
    )])
    .await;
    let resolver = Arc::new(FixedResolver {
        answers: BTreeMap::from([("localhost".to_string(), vec![IpAddr::from([127, 0, 0, 1])])]),
        fail: false,
    });
    let runtime = WebRuntime::new(
        web_settings(&format!("http://localhost:{port}")),
        Some(resolver),
    )
    .unwrap();
    let document = runtime
        .fetch(
            &format!("http://localhost:{port}/page"),
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap();
    assert_eq!(document.status, 200);
    assert!(document.body.contains("Hello pinned world"));
    assert!(!document.truncated);
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_follows_bounded_redirects_and_rejects_disallowed_targets() {
    let (port, server) = spawn_server(vec![
        redirected("/next"),
        response("200 OK", "text/plain", b"redirected body"),
    ])
    .await;
    let runtime = WebRuntime::new(web_settings(&format!("http://127.0.0.1:{port}")), None).unwrap();
    let document = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/start"),
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap();
    assert_eq!(document.redirects, 1);
    assert_eq!(document.body, "redirected body");
    server.await.unwrap();

    let (port, server) = spawn_server(vec![redirected("http://example.com/private")]).await;
    let runtime = WebRuntime::new(web_settings(&format!("http://127.0.0.1:{port}")), None).unwrap();
    let error = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/start"),
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "origin_denied");
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_enforces_redirect_limit_and_bounded_body() {
    let responses = (0..6).map(|_| redirected("/loop")).collect::<Vec<_>>();
    let (port, server) = spawn_server(responses).await;
    let runtime = WebRuntime::new(
        WebSettings {
            max_redirects: 5,
            ..web_settings(&format!("http://127.0.0.1:{port}"))
        },
        None,
    )
    .unwrap();
    let error = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/loop"),
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "too_many_redirects");
    server.await.unwrap();

    let (port, server) =
        spawn_server(vec![response("200 OK", "text/plain", b"0123456789abcdef")]).await;
    let runtime = WebRuntime::new(web_settings(&format!("http://127.0.0.1:{port}")), None).unwrap();
    let document = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/big"),
            reqwest::Method::GET,
            Some(8),
            &Cancel::default(),
        )
        .await
        .unwrap();
    assert_eq!(document.body, "01234567");
    assert!(document.truncated);
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_rejects_metadata_dns_special_schemes_and_cancellation() {
    let resolver = Arc::new(FixedResolver {
        answers: BTreeMap::from([(
            "metadata.test".to_string(),
            vec![IpAddr::from([169, 254, 169, 254])],
        )]),
        fail: false,
    });
    let settings = WebSettings {
        enabled: true,
        allowed_origins: vec!["http://metadata.test".into()],
        localhost_development_origin: None,
        ..WebSettings::default()
    };
    let runtime = WebRuntime::new(settings, Some(resolver)).unwrap();
    let error = runtime
        .fetch(
            "http://metadata.test/latest",
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "address_denied");

    let resolver = Arc::new(FixedResolver {
        answers: BTreeMap::from([(
            "private.test".to_string(),
            vec![IpAddr::from([10, 0, 0, 7])],
        )]),
        fail: false,
    });
    let runtime = WebRuntime::new(
        WebSettings {
            enabled: true,
            allowed_origins: vec!["http://private.test".into()],
            ..WebSettings::default()
        },
        Some(resolver),
    )
    .unwrap();
    let error = runtime
        .fetch(
            "http://private.test/",
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "address_denied");

    let resolver = Arc::new(FixedResolver {
        answers: BTreeMap::new(),
        fail: true,
    });
    let runtime = WebRuntime::new(
        WebSettings {
            enabled: true,
            allowed_origins: vec!["http://dns.test".into()],
            ..WebSettings::default()
        },
        Some(resolver),
    )
    .unwrap();
    let error = runtime
        .fetch(
            "http://dns.test/",
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "dns_failed");

    let runtime = WebRuntime::new(web_settings("http://127.0.0.1:1"), None).unwrap();
    for url in [
        "file:///etc/passwd",
        "http://user:pass@127.0.0.1/",
        "http://127.0.0.1/#fragment",
    ] {
        let error = runtime
            .fetch(url, reqwest::Method::GET, None, &Cancel::default())
            .await
            .unwrap_err();
        assert_eq!(error.0.code, "invalid_target", "{url}");
    }
    let cancel = Cancel::default();
    cancel.0.store(true, Ordering::Release);
    let error = runtime
        .fetch("http://127.0.0.1:1/", reqwest::Method::GET, None, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "cancelled");
}

#[tokio::test]
async fn fetch_rejects_unsupported_content_and_invalid_utf8() {
    let (port, server) = spawn_server(vec![response(
        "200 OK",
        "application/octet-stream",
        b"binary",
    )])
    .await;
    let runtime = WebRuntime::new(web_settings(&format!("http://127.0.0.1:{port}")), None).unwrap();
    let error = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/binary"),
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "unsupported_content_type");
    server.await.unwrap();

    let (port, server) =
        spawn_server(vec![response("200 OK", "text/plain", &[0xff, 0xfe, 0xfd])]).await;
    let runtime = WebRuntime::new(web_settings(&format!("http://127.0.0.1:{port}")), None).unwrap();
    let error = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/utf8"),
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "invalid_utf8");
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_search_endpoint_returns_untrusted_results() {
    let body =
        br#"{"results":[{"title":"One","url":"https://example.com/one","snippet":"first"}]}"#;
    let (port, server) = spawn_server(vec![response("200 OK", "application/json", body)]).await;
    let settings = WebSettings {
        search: Some(WebSearchSettings {
            endpoint: format!("http://127.0.0.1:{port}/search"),
            max_results: 3,
        }),
        ..web_settings(&format!("http://127.0.0.1:{port}"))
    };
    let runtime = WebRuntime::new(settings, None).unwrap();
    let (results, document) = runtime
        .search("rust", None, &Cancel::default())
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "One");
    assert!(document.final_url.contains("q=rust"));
    server.await.unwrap();
}

#[tokio::test]
async fn web_dispatch_captures_long_body_in_output_store() {
    let body = "A".repeat(256);
    let (port, server) =
        spawn_server(vec![response("200 OK", "text/plain", body.as_bytes())]).await;
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let limits = OutputLimits {
        max_object_bytes: 32,
        max_run_bytes: 32,
        max_store_bytes: 256,
        max_objects: 8,
        tail_bytes: 8,
        preview_bytes: 8,
        max_page_bytes: 8,
        max_page_lines: 8,
        max_search_matches: 8,
    };
    let outputs = Arc::new(
        OutputStore::open(
            temp.path().join("outputs"),
            limits,
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    dispatcher.set_output_store(outputs.clone()).unwrap();
    register_web(
        &dispatcher,
        web_settings(&format!("http://127.0.0.1:{port}")),
        None,
    )
    .unwrap();
    let result = execute_one(
        &dispatcher,
        &call(
            "fetch_url",
            json!({"url":format!("http://127.0.0.1:{port}/long")}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    let data = result.data.as_ref().unwrap();
    assert_eq!(data["source"], "untrusted_web");
    assert!(result.truncated);
    let output_ref = data["output_refs"][0]["output_ref"].as_str().unwrap();
    let metadata = outputs.describe(run.session_id, output_ref).unwrap();
    assert_eq!(metadata.scope.stream, OutputStream::Web);
    assert_eq!(metadata.coverage, OutputCoverage::RetainedRangesOnly);
    let page = outputs
        .read(
            run.session_id,
            output_ref,
            crate::output_store::ReadOptions {
                cursor: None,
                max_bytes: 8,
                max_lines: 8,
            },
        )
        .unwrap();
    assert!(!page.chunks.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn browser_shutdown_goes_through_agent_runtime_and_plan_round_trip() {
    use crate::runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup};
    use tokio::sync::Notify;

    struct WaitingModel {
        started: Arc<AtomicBool>,
        release: Arc<Notify>,
    }
    impl Model for WaitingModel {
        fn start<'a>(
            &'a self,
            _: &'a ModelRequest,
            _: &'a dyn Cancellation,
        ) -> FutureResult<'a, Box<dyn ModelStream>> {
            Box::pin(async move {
                self.started.store(true, Ordering::Release);
                self.release.notified().await;
                Ok(
                    Box::new(Finished(Some(crate::boundary::ModelUpdate::Finished {
                        items: vec![HistoryItem::AssistantText("done".into())],
                        usage: None,
                    }))) as Box<dyn ModelStream>,
                )
            })
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let mut session = Session {
        id: SessionId::new(),
        workspace: temp.path().into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture".into(),
        history: vec![HistoryItem::UserText("task".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    let store = MemoryStore::default();
    store
        .commit(&[StoreChange::Session(session.clone())])
        .await
        .unwrap();
    let provider = Arc::new(FixtureProvider::full());
    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(Notify::new());
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: Arc::new(WaitingModel {
            started: started.clone(),
            release: release.clone(),
        }),
        store: Arc::new(store),
        policy: Arc::new(Policy::new(temp.path(), PermissionConfig::default()).unwrap()),
        files: Arc::new(
            ReadTools::new(temp.path(), &[], false, ReadLimits::default())
                .map_err(|error| {
                    BoundaryError(
                        ErrorInfo {
                            code: error.code.into(),
                            message: error.message,
                        },
                        None,
                        None,
                    )
                })
                .unwrap(),
        ),
        cancellation: Arc::new(Cancel::default()),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "fixture".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
    .unwrap()
    .with_browser(
        BrowserSettings {
            enabled: true,
            ..Default::default()
        },
        provider.clone(),
    )
    .unwrap();
    let handle = runtime.handle();
    let task = tokio::spawn(async move {
        let result = runtime.run(&mut session).await;
        (session, result)
    });
    if tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !started.load(Ordering::Acquire) {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .is_err()
    {
        let (_, result) = task.await.unwrap();
        panic!("runtime did not reach the model: {result:?}");
    }
    handle.request_mode(WorkMode::Plan).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(handle.mode().unwrap().pending, Some(WorkMode::Plan));
    release.notify_waiters();
    let (session, result) = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_ok());
    assert_eq!(session.work_mode, WorkMode::Plan);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while provider.shutdown_count() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(provider.shutdown_count(), 1);
    handle.shutdown_background().await.unwrap();
    assert_eq!(provider.shutdown_count(), 1);
}

#[tokio::test]
async fn non_html_truncation_and_inline_fallbacks_are_bounded() {
    // Direct runtime: HTML-extraction and plain-text limits both report truncation.
    let body = "A".repeat(20 * 1024);
    let (port, server) =
        spawn_server(vec![response("200 OK", "text/plain", body.as_bytes())]).await;
    let runtime = WebRuntime::new(
        WebSettings {
            max_content_chars: 64,
            ..web_settings(&format!("http://127.0.0.1:{port}"))
        },
        None,
    )
    .unwrap();
    let document = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/plain"),
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap();
    assert_eq!(document.body.len(), 64);
    assert!(document.truncated);
    server.await.unwrap();

    // No OutputStore: the model receives a bounded 16 KiB prefix, never an
    // empty successful body.
    let (port, server) =
        spawn_server(vec![response("200 OK", "text/plain", body.as_bytes())]).await;
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    register_web(
        &dispatcher,
        WebSettings {
            max_content_chars: 32 * 1024,
            max_response_bytes: 64 * 1024,
            ..web_settings(&format!("http://127.0.0.1:{port}"))
        },
        None,
    )
    .unwrap();
    let result = execute_one(
        &dispatcher,
        &call(
            "fetch_url",
            json!({"url":format!("http://127.0.0.1:{port}/plain")}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    assert!(result.ok);
    let data = result.data.as_ref().unwrap();
    assert_eq!(data["body"].as_str().unwrap().len(), 16 * 1024);
    assert_eq!(data["truncated"], true);
    assert!(data.get("output_capture_errors").is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn capture_failure_keeps_inline_prefix_and_reports_errors() {
    let body = "B".repeat(20 * 1024);
    let (port, server) =
        spawn_server(vec![response("200 OK", "text/plain", body.as_bytes())]).await;
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let root = temp.path().join("outputs");
    let outputs = Arc::new(
        OutputStore::open(
            &root,
            OutputLimits {
                max_object_bytes: 4096,
                max_run_bytes: 8192,
                max_store_bytes: 65536,
                max_objects: 8,
                tail_bytes: 8,
                preview_bytes: 8,
                max_page_bytes: 8192,
                max_page_lines: 64,
                max_search_matches: 8,
            },
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::write(&root, b"not a directory").unwrap();
    dispatcher.set_output_store(outputs).unwrap();
    register_web(
        &dispatcher,
        WebSettings {
            max_content_chars: 32 * 1024,
            max_response_bytes: 64 * 1024,
            ..web_settings(&format!("http://127.0.0.1:{port}"))
        },
        None,
    )
    .unwrap();
    let result = execute_one(
        &dispatcher,
        &call(
            "fetch_url",
            json!({"url":format!("http://127.0.0.1:{port}/plain")}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    assert!(result.ok);
    let data = result.data.as_ref().unwrap();
    assert_eq!(
        data["body"]
            .as_str()
            .unwrap_or_else(|| panic!("missing body: {data}")),
        "B".repeat(16 * 1024)
    );
    assert_eq!(data["truncated"], true);
    assert!(
        data["output_capture_errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
    );
    assert!(result.truncated);
    server.await.unwrap();
}

#[tokio::test]
async fn withheld_web_capture_falls_back_to_inline_body() {
    let body = "D".repeat(16);
    let (port, server) = spawn_server(vec![
        response("200 OK", "text/plain", body.as_bytes()),
        response("200 OK", "text/plain", body.as_bytes()),
    ])
    .await;
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let outputs = Arc::new(
        OutputStore::open(
            temp.path().join("outputs"),
            OutputLimits {
                max_object_bytes: 16,
                max_run_bytes: 16,
                max_store_bytes: 64,
                max_objects: 8,
                tail_bytes: 8,
                preview_bytes: 8,
                max_page_bytes: 8192,
                max_page_lines: 64,
                max_search_matches: 8,
            },
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    dispatcher.set_output_store(outputs.clone()).unwrap();
    register_web(
        &dispatcher,
        WebSettings {
            max_response_bytes: 64,
            max_content_chars: 64,
            ..web_settings(&format!("http://127.0.0.1:{port}"))
        },
        None,
    )
    .unwrap();
    let url = format!("http://127.0.0.1:{port}/plain");
    // The first response exhausts this Run's retention quota.
    execute_one(
        &dispatcher,
        &call("fetch_url", json!({"url":url})),
        &context,
        "allow",
        true,
    )
    .await;
    let second = execute_one(
        &dispatcher,
        &call("fetch_url", json!({"url":url})),
        &context,
        "allow",
        true,
    )
    .await;
    let data = second.data.as_ref().unwrap();
    assert_eq!(data["truncated"], true);
    assert!(second.truncated);
    assert_eq!(data["body"].as_str().unwrap(), body);
    let output_ref = data["output_refs"][0]["output_ref"].as_str().unwrap();
    let metadata = outputs.describe(run.session_id, output_ref).unwrap();
    assert_eq!(metadata.retained_bytes, 0);
    assert_eq!(
        metadata.withheld_reason,
        Some(crate::output_store::WithheldReason::CaptureDisabled)
    );
    assert_eq!(
        metadata.completion,
        crate::output_store::CaptureCompletion::LimitReached
    );
    server.await.unwrap();
}

#[tokio::test]
async fn body_truncation_mid_utf8_keeps_valid_prefix() {
    let body = b"ABCD\xe2\x82\xac";
    let (port, server) = spawn_server(vec![response("200 OK", "text/plain", body)]).await;
    let runtime = WebRuntime::new(web_settings(&format!("http://127.0.0.1:{port}")), None).unwrap();
    let document = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/utf8"),
            reqwest::Method::GET,
            Some(5),
            &Cancel::default(),
        )
        .await
        .unwrap();
    assert_eq!(document.body, "ABCD");
    assert!(document.truncated);
    server.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn dns_resolution_is_bounded_by_timeout() {
    let runtime = WebRuntime::new(
        WebSettings {
            enabled: true,
            allowed_origins: vec!["http://slow.test".into()],
            request_timeout_ms: 50,
            ..WebSettings::default()
        },
        Some(Arc::new(HangingResolver)),
    )
    .unwrap();
    let error = runtime
        .fetch(
            "http://slow.test/",
            reqwest::Method::GET,
            None,
            &Cancel::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "web_timeout");
}

#[tokio::test]
async fn body_read_responds_to_cancellation() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0u8; 1024];
        let _ = stream.read(&mut buffer).await;
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\nx")
            .await;
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });
    let runtime = WebRuntime::new(web_settings(&format!("http://127.0.0.1:{port}")), None).unwrap();
    let cancel = Arc::new(Cancel::default());
    let trigger = cancel.clone();
    let task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        trigger.0.store(true, Ordering::Release);
    });
    let error = runtime
        .fetch(
            &format!("http://127.0.0.1:{port}/slow"),
            reqwest::Method::GET,
            None,
            cancel.as_ref(),
        )
        .await
        .unwrap_err();
    task.await.unwrap();
    server.abort();
    assert_eq!(error.0.code, "cancelled");
}

#[tokio::test]
async fn truncated_web_capture_is_not_marked_complete() {
    let body = "C".repeat(1024);
    let (port, server) =
        spawn_server(vec![response("200 OK", "text/plain", body.as_bytes())]).await;
    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    let outputs = Arc::new(
        OutputStore::open(
            temp.path().join("outputs"),
            OutputLimits {
                max_object_bytes: 4096,
                max_run_bytes: 8192,
                max_store_bytes: 65536,
                max_objects: 8,
                tail_bytes: 8,
                preview_bytes: 8,
                max_page_bytes: 8192,
                max_page_lines: 64,
                max_search_matches: 8,
            },
            Arc::new(SecretRegistry::default()),
        )
        .unwrap(),
    );
    dispatcher.set_output_store(outputs.clone()).unwrap();
    register_web(
        &dispatcher,
        WebSettings {
            max_content_chars: 64,
            max_response_bytes: 64 * 1024,
            ..web_settings(&format!("http://127.0.0.1:{port}"))
        },
        None,
    )
    .unwrap();
    let result = execute_one(
        &dispatcher,
        &call(
            "fetch_url",
            json!({"url":format!("http://127.0.0.1:{port}/plain")}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    let output_ref = result.data.as_ref().unwrap()["output_refs"][0]["output_ref"]
        .as_str()
        .unwrap();
    let metadata = outputs.describe(run.session_id, output_ref).unwrap();
    assert_eq!(
        metadata.completion,
        crate::output_store::CaptureCompletion::LimitReached
    );
    assert_eq!(metadata.coverage, OutputCoverage::RetainedRangesOnly);
    assert!(!metadata.artifact_complete);
    server.await.unwrap();
}

#[tokio::test]
async fn read_only_runtime_never_registers_or_dispatches_browser_tools() {
    use crate::runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup};

    let temp = tempfile::tempdir().unwrap();
    let run = run(WorkMode::Work, PermissionMode::Auto);
    let store = MemoryStore::default();
    let runtime = AgentRuntime::new_read_only(RuntimeSetup {
        run: run.clone(),
        model: Arc::new(DecisionModel { decision: "allow" }),
        store: Arc::new(store),
        policy: Arc::new(Policy::new(temp.path(), PermissionConfig::default()).unwrap()),
        files: Arc::new(
            ReadTools::new(temp.path(), &[], false, ReadLimits::default())
                .map_err(|error| {
                    BoundaryError(
                        ErrorInfo {
                            code: error.code.into(),
                            message: error.message,
                        },
                        None,
                        None,
                    )
                })
                .unwrap(),
        ),
        cancellation: Arc::new(Cancel::default()),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "fixture".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
    .unwrap();
    match runtime.with_browser(
        BrowserSettings {
            enabled: true,
            ..Default::default()
        },
        Arc::new(FixtureProvider::full()),
    ) {
        Ok(_) => panic!("read-only runtime accepted browser tools"),
        Err(error) => assert_eq!(error.0.code, "invalid_config"),
    }

    // The dispatcher boundary also hides and rejects browser effects.
    let (dispatcher, context) = dispatcher(temp.path(), &run, None).await.unwrap();
    register_browser(&dispatcher, &run, Arc::new(FixtureProvider::full())).unwrap();
    register_web(&dispatcher, web_settings("https://example.com"), None).unwrap();
    let dispatcher = dispatcher.restrict_to_read_only();
    let names = dispatcher
        .definitions(WorkMode::Work)
        .unwrap()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&"fetch_url".to_string()));
    assert!(!names.iter().any(|name| name.starts_with("browser_")));
    let snapshot = call("browser_snapshot", json!({"page":"page_abc"}));
    let model = DecisionModel { decision: "allow" };
    let budget = SharedBudget::default();
    let review = ReviewConfig::new("fixture".into());
    let outcome = dispatcher
        .authorize(
            &snapshot,
            &context,
            "fixture task",
            true,
            DispatchReview {
                model: &model,
                budget: &budget,
                config: &review,
            },
        )
        .await;
    match outcome {
        Err(error) => assert_eq!(error.0.code, "durability_required"),
        Ok(_) => panic!("read-only dispatcher authorized a browser call"),
    }
}

#[tokio::test]
async fn browser_transfers_are_untrusted_and_dev079_saves_no_screenshot_media() {
    let temp = tempfile::tempdir().unwrap();
    let (dispatcher, context) =
        scenario(temp.path(), WorkMode::Work, PermissionMode::Auto, None).await;
    let opened = execute_one(
        &dispatcher,
        &call("browser_open", json!({})),
        &context,
        "allow",
        true,
    )
    .await;
    let page = opened.data.as_ref().unwrap()["page"]
        .as_str()
        .unwrap()
        .to_string();
    let snapshot = execute_one(
        &dispatcher,
        &call("browser_snapshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let document = snapshot.data.as_ref().unwrap()["document"]
        .as_str()
        .unwrap()
        .to_string();
    let element = snapshot.data.as_ref().unwrap()["elements"][0]["element_ref"]
        .as_str()
        .unwrap()
        .to_string();

    let screenshot = execute_one(
        &dispatcher,
        &call("browser_screenshot", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let screenshot = screenshot.data.as_ref().unwrap();
    assert_eq!(screenshot["source"], "untrusted_browser");
    assert!(screenshot["screenshot"].get("media_ref").is_none());
    assert_eq!(screenshot["screenshot"]["model_image_attached"], false);

    let download = execute_one(
        &dispatcher,
        &call("browser_download", json!({"page":page})),
        &context,
        "allow",
        true,
    )
    .await;
    let download = download.data.as_ref().unwrap();
    assert_eq!(download["source"], "untrusted_browser");
    assert_eq!(download["download"]["artifact_stored"], false);

    let upload = execute_one(
        &dispatcher,
        &call(
            "browser_upload",
            json!({"page":page,"document":document,"element_ref":element,"artifact_ref":"artifact_1"}),
        ),
        &context,
        "allow",
        true,
    )
    .await;
    let upload = upload.data.as_ref().unwrap();
    assert_eq!(upload["source"], "untrusted_browser");
    assert!(upload["upload"]["document_revision"].is_u64());
}
