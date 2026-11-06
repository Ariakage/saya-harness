#[path = "fixtures/http_support/mod.rs"]
mod fixture;
mod support;
use saya_core::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, Operation as ModelOperation, Store,
        ToolDefinition,
    },
    contract::{
        HistoryItem, ModelProtocol, ModelPurpose, PermissionMode, Run, RunId, RunStatus, Session,
        SessionId, WorkMode,
    },
    memory::MemoryStore,
    permissions::{Decision, Operation, PermissionConfig, Policy},
    provider::{
        ModelSettings,
        anthropic::{AnthropicModel, AnthropicVersion},
        chat::{ChatCompatibility, ChatModel},
        responses::{ResponsesCompatibility, ResponsesModel},
        router::ProfileRouter,
        transport::{Authentication, HttpConfig, HttpTransport},
    },
    review::{ReviewConfig, ReviewDecisionSource, ReviewInput, SharedBudget, review_operation},
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::{ExecConfig, ExecPlan},
        read::{ReadLimits, ReadTools},
    },
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
fn transport(url: String) -> Result<Arc<HttpTransport>, Box<dyn std::error::Error>> {
    Ok(Arc::new(HttpTransport::new(HttpConfig {
        base_url: url,
        allow_remote_http: false,
        authentication: Authentication::None,
        headers_env: BTreeMap::new(),
        connect_timeout: Duration::from_secs(1),
        idle_timeout: Duration::from_secs(2),
        total_timeout: Duration::from_secs(5),
    })?))
}
fn settings(profile: &str, model: &str, stream: bool) -> ModelSettings {
    ModelSettings {
        profile: profile.into(),
        model: model.into(),
        context_window_tokens: 128_000,
        max_output_tokens: 8192,
        strict_tools: false,
        stream,
    }
}
fn adapter(
    protocol: ModelProtocol,
    url: String,
    profile: &str,
) -> Result<Arc<dyn Model>, Box<dyn std::error::Error>> {
    let client = transport(url)?;
    let settings = settings(profile, "review-model", false);
    Ok(match protocol {
        ModelProtocol::ChatCompletions => Arc::new(ChatModel::new(
            client,
            settings,
            ChatCompatibility::default(),
        )),
        ModelProtocol::Responses => Arc::new(ResponsesModel::new(
            client,
            settings,
            ResponsesCompatibility::default(),
        )),
        ModelProtocol::AnthropicMessages => Arc::new(AnthropicModel::new(
            client,
            settings,
            AnthropicVersion::default(),
        )),
    })
}
fn response(protocol: ModelProtocol, content: &str) -> String {
    match protocol{
  ModelProtocol::ChatCompletions=>json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}}]}),
  ModelProtocol::Responses=>json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":content}]}]}),
  ModelProtocol::AnthropicMessages=>json!({"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":content}]}),
 }.to_string()
}
fn operation(root: &Path) -> Result<Operation, Box<dyn std::error::Error>> {
    let exe = std::fs::canonicalize("/bin/echo")?;
    Ok(Operation::exec(&ExecPlan {
        argv: vec![exe.to_string_lossy().into_owned(), "safe".into()],
        cwd: root.into(),
        timeout: Duration::from_secs(2),
        environment_digest: "fixture".into(),
    })?)
}
fn input(op: &Operation) -> ReviewInput<'_> {
    ReviewInput {
        mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
        operation: op,
        arguments: json!({"argv":["echo","safe"],"cwd":"."}),
        goal_summary: "fixture-secret should be redacted",
    }
}
struct Unused;
impl Model for Unused {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> ModelOperation<'a, Box<dyn ModelStream>> {
        Box::pin(async { panic!("wrong profile was selected") })
    }
}

#[tokio::test]
async fn all_three_protocols_use_default_or_explicit_review_profile()
-> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        for explicit in [false, true] {
            let (url, requests) = fixture::server(vec![(
                "application/json",
                response(protocol, r#"{"decision":"allow","reason":"bounded"}"#),
            )])?;
            let selected = if explicit { "review" } else { "execution" };
            let selected_model = adapter(protocol, url, selected)?;
            let mut models: BTreeMap<String, Arc<dyn Model>> = BTreeMap::new();
            if explicit {
                models.insert("execution".into(), Arc::new(Unused));
            }
            models.insert(selected.into(), selected_model);
            let router = ProfileRouter::new("execution".into(), models)?;
            let temp = support::temp_workspace()?;
            let policy = Policy::new(temp.path(), PermissionConfig::default())?;
            let op = operation(temp.path())?;
            let mut config = ReviewConfig::new("execution".into());
            if explicit {
                config.review_profile = Some("review".into());
            }
            config.known_secrets.push("fixture-secret".into());
            let budget = SharedBudget::default();
            let outcome =
                review_operation(&policy, input(&op), &router, &budget, &config, &Never).await?;
            assert_eq!(
                (outcome.decision, outcome.source),
                (Decision::Allow, ReviewDecisionSource::Model)
            );
            assert_eq!(budget.used()?, 1);
            let body = requests.recv_timeout(Duration::from_secs(1))?;
            let serialized = body.to_string();
            assert!(!serialized.contains("fixture-secret"));
            assert!(body.get("tools").is_none());
            match protocol {
                ModelProtocol::ChatCompletions => assert_eq!(body["max_completion_tokens"], 1024),
                ModelProtocol::Responses => assert_eq!(body["max_output_tokens"], 1024),
                ModelProtocol::AnthropicMessages => assert_eq!(body["max_tokens"], 1024),
            }
            assert!(serialized.contains("REDACTED"));
        }
    }
    Ok(())
}
#[tokio::test]
async fn missing_review_profile_falls_back_and_execution_switch_is_rejected()
-> Result<(), Box<dyn std::error::Error>> {
    let mut models: BTreeMap<String, Arc<dyn Model>> = BTreeMap::new();
    models.insert("execution".into(), Arc::new(Unused));
    let router = ProfileRouter::new("execution".into(), models)?;
    let temp = support::temp_workspace()?;
    let policy = Policy::new(temp.path(), PermissionConfig::default())?;
    let op = operation(temp.path())?;
    let mut config = ReviewConfig::new("execution".into());
    config.review_profile = Some("missing".into());
    let result = review_operation(
        &policy,
        input(&op),
        &router,
        &SharedBudget::default(),
        &config,
        &Never,
    )
    .await?;
    assert_eq!(
        (result.decision, result.source),
        (Decision::Ask, ReviewDecisionSource::HumanFallback)
    );
    let request = ModelRequest {
        request_id: "id".into(),
        purpose: ModelPurpose::Execution,
        profile: "other".into(),
        instructions: String::new(),
        max_output_tokens: None,
        history: vec![],
        tools: vec![],
    };
    let error = match router.start(&request, &Never).await {
        Ok(_) => return Err("execution switched profile".into()),
        Err(e) => e,
    };
    assert_eq!(error.0.code, "invalid_config");
    let mut request = request;
    request.purpose = ModelPurpose::Review;
    request.profile = "execution".into();
    request.tools.push(ToolDefinition {
        name: "exec".into(),
        description: String::new(),
        input_schema: json!({"type":"object"}),
    });
    let error = match router.start(&request, &Never).await {
        Ok(_) => return Err("review accepted tools".into()),
        Err(e) => e,
    };
    assert_eq!(error.0.code, "invalid_config");
    Ok(())
}
#[tokio::test]
async fn runtime_routes_review_to_separate_protocol_and_respects_denial()
-> Result<(), Box<dyn std::error::Error>> {
    let first = fixture::sse(&[
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"cmd","type":"function","function":{"name":"exec","arguments":"{\"argv\":[\"/bin/sh\",\"-c\",\"touch marker\"]}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
    ]);
    let second = fixture::sse(&[
        json!({"choices":[{"index":0,"delta":{"content":"denied"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ]);
    let (exec_url, exec_requests) = fixture::server(vec![
        ("text/event-stream", first),
        ("text/event-stream", second),
    ])?;
    let (review_url, review_requests) = fixture::server(vec![(
        "application/json",
        response(
            ModelProtocol::AnthropicMessages,
            r#"{"decision":"deny","reason":"fixture denial"}"#,
        ),
    )])?;
    let execution: Arc<dyn Model> = Arc::new(ChatModel::new(
        transport(exec_url)?,
        settings("execution", "exec-model", true),
        ChatCompatibility::default(),
    ));
    let review = adapter(ModelProtocol::AnthropicMessages, review_url, "review")?;
    let mut profiles = BTreeMap::new();
    profiles.insert("execution".into(), execution);
    profiles.insert("review".into(), review);
    let router = Arc::new(ProfileRouter::new("execution".into(), profiles)?);
    let temp = support::temp_workspace()?;
    let mut session = Session {
        id: SessionId::new(),
        workspace: temp.path().into(),
        work_mode: WorkMode::Work,
        profile: "execution".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "exec-model".into(),
        history: vec![HistoryItem::UserText("attempt command".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    let mut review_config = ReviewConfig::new("execution".into());
    review_config.review_profile = Some("review".into());
    let store = Arc::new(MemoryStore::default());
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: router,
        store: store.clone(),
        policy: Arc::new(Policy::new(temp.path(), PermissionConfig::default())?),
        files: Arc::new(ReadTools::new(
            temp.path(),
            &[],
            false,
            ReadLimits::default(),
        )?),
        cancellation: Arc::new(Never),
        review: review_config,
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "attempt command".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    assert!(!temp.path().join("marker").exists());
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("session missing")?
            .history,
        session.history
    );
    let _ = exec_requests.recv_timeout(Duration::from_secs(1))?;
    let second = exec_requests.recv_timeout(Duration::from_secs(1))?;
    assert!(second["messages"].as_array().is_some_and(|v| {
        v.iter()
            .any(|m| m["role"] == "tool" && m["tool_call_id"] == "cmd")
    }));
    let review = review_requests.recv_timeout(Duration::from_secs(1))?;
    assert!(review.get("tools").is_none());
    assert_eq!(review["max_tokens"], 1024);
    assert_eq!(review["messages"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[tokio::test]
async fn runtime_rejects_mismatched_review_execution_profile_before_start()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let mut session = Session {
        id: SessionId::new(),
        workspace: temp.path().into(),
        work_mode: WorkMode::Work,
        profile: "execution".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "model".into(),
        history: vec![HistoryItem::UserText("task".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    let store = Arc::new(MemoryStore::default());
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: Arc::new(Unused),
        store: store.clone(),
        policy: Arc::new(Policy::new(temp.path(), PermissionConfig::default())?),
        files: Arc::new(ReadTools::new(
            temp.path(),
            &[],
            false,
            ReadLimits::default(),
        )?),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("wrong".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(
        result.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_config")
    );
    assert!(store.load_session(session.id).await?.is_none());
    Ok(())
}
