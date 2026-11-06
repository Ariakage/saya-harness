#[path = "fixtures/http_support/mod.rs"]
mod fixture;
mod support;
use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, ModelRequest, ModelUpdate, Store, ToolDefinition},
    contract::{
        HistoryItem, ModelProtocol, ModelPurpose, PermissionMode, Run, RunId, RunStatus, Session,
        SessionId, ToolResult, WorkMode,
    },
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    provider::{
        ModelSettings,
        chat::{ChatCompatibility, ChatModel},
        transport::{Authentication, HttpConfig, HttpTransport},
    },
    review::ReviewConfig,
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::{Future, pending},
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
fn settings(stream: bool) -> ModelSettings {
    ModelSettings {
        profile: "test".into(),
        model: "fixture-model".into(),
        context_window_tokens: 128_000,
        max_output_tokens: 8192,
        strict_tools: false,
        stream,
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
fn request() -> ModelRequest {
    ModelRequest {
        request_id: "one".into(),
        purpose: ModelPurpose::Execution,
        profile: "test".into(),
        instructions: "be helpful".into(),
        dynamic_instructions: None,
        max_output_tokens: None,
        history: vec![HistoryItem::UserText("hi".into())],
        tools: vec![],
    }
}
fn chunk(delta: Value, finish: Value) -> Value {
    json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":null})
}
async fn collect(
    model: &dyn Model,
    request: &ModelRequest,
) -> Result<
    (Vec<HistoryItem>, Option<saya_core::contract::Usage>, String),
    Box<dyn std::error::Error>,
> {
    let mut stream = model.start(request, &Never).await?;
    let mut text = String::new();
    loop {
        match stream.next(&Never).await? {
            Some(ModelUpdate::TextDelta(delta)) => text.push_str(&delta),
            Some(ModelUpdate::Progress) => {}
            Some(ModelUpdate::Finished { items, usage }) => return Ok((items, usage, text)),
            None => return Err("missing finished event".into()),
        }
    }
}
#[tokio::test]
async fn streams_text_and_usage_with_explicit_storage_disabled()
-> Result<(), Box<dyn std::error::Error>> {
    let body = fixture::sse(&[
        chunk(json!({"role":"assistant"}), Value::Null),
        chunk(json!({"content":"hello "}), Value::Null),
        chunk(json!({"content":"world"}), Value::Null),
        chunk(json!({}), json!("stop")),
        json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3}}),
    ]);
    let (url, requests) = fixture::server(vec![("text/event-stream", body)])?;
    let model = ChatModel::new(
        transport(url)?,
        settings(true),
        ChatCompatibility::default(),
    );
    let (items, usage, text) = collect(&model, &request()).await?;
    assert_eq!(text, "hello world");
    assert_eq!(items, vec![HistoryItem::AssistantText(text)]);
    let usage = usage.ok_or("missing usage")?;
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.estimated),
        (Some(11), Some(3), false)
    );
    let request = requests.recv_timeout(Duration::from_secs(1))?;
    assert_eq!(request["store"], false);
    assert_eq!(request["stream"], true);
    assert_eq!(request["stream_options"]["include_usage"], true);
    assert_eq!(request["model"], "fixture-model");
    assert_eq!(request["max_completion_tokens"], 8192);
    assert_eq!(request["messages"][0]["role"], "system");
    Ok(())
}
#[tokio::test]
async fn interleaved_tool_fragments_round_trip_with_original_ids()
-> Result<(), Box<dyn std::error::Error>> {
    let first = fixture::sse(&[
        chunk(
            json!({"tool_calls":[{"index":0,"id":"call-a","type":"function","function":{"name":"read_file","arguments":"{\"path\":"}},{"index":1,"id":"call-b","type":"function","function":{"name":"list_dir","arguments":"{\"path\":"}}]}),
            Value::Null,
        ),
        chunk(
            json!({"tool_calls":[{"index":1,"function":{"arguments":"\".\"}"}},{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}),
            Value::Null,
        ),
        chunk(json!({}), json!("tool_calls")),
    ]);
    let second = fixture::sse(&[
        chunk(json!({"content":"done"}), Value::Null),
        chunk(json!({}), json!("stop")),
    ]);
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", first),
        ("text/event-stream", second),
    ])?;
    let model = ChatModel::new(
        transport(url)?,
        settings(true),
        ChatCompatibility::default(),
    );
    let mut req = request();
    let (items, _, _) = collect(&model, &req).await?;
    assert_eq!(items.len(), 2);
    let mut ids = Vec::new();
    for item in &items {
        if let HistoryItem::ToolCall(call) = item {
            ids.push(call.call_id.clone());
            assert!(serde_json::from_str::<Value>(&call.arguments)?.is_object());
        }
    }
    assert_eq!(ids, ["call-a", "call-b"]);
    req.history.extend(items);
    for id in &ids {
        req.history.push(HistoryItem::ToolResult {
            call_id: id.clone(),
            result: ToolResult {
                ok: true,
                data: Some(json!({"value":"ok"})),
                error: None,
                truncated: false,
            },
        });
    }
    assert_eq!(collect(&model, &req).await?.2, "done");
    let _first = requests.recv_timeout(Duration::from_secs(1))?;
    let second = requests.recv_timeout(Duration::from_secs(1))?;
    let messages = second["messages"].as_array().ok_or("missing messages")?;
    let assistant = messages
        .iter()
        .find(|m| m["tool_calls"].is_array())
        .ok_or("missing assistant calls")?;
    assert_eq!(assistant["tool_calls"][0]["id"], "call-a");
    assert_eq!(assistant["tool_calls"][1]["id"], "call-b");
    let results: Vec<_> = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["tool_call_id"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(results, ["call-a", "call-b"]);
    Ok(())
}
#[tokio::test]
async fn complete_json_maps_text_tool_and_usage() -> Result<(), Box<dyn std::error::Error>> {
    let response = json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":"checking","tool_calls":[{"id":"call-1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":2}});
    let (url, requests) = fixture::server(vec![("application/json", response.to_string())])?;
    let model = ChatModel::new(
        transport(url)?,
        settings(false),
        ChatCompatibility {
            legacy_max_tokens: true,
            omit_stream_usage: true,
        },
    );
    let (items, usage, _) = collect(&model, &request()).await?;
    assert!(
        matches!(&items[..],[HistoryItem::AssistantText(text),HistoryItem::ToolCall(call)] if text=="checking" && call.call_id=="call-1")
    );
    assert_eq!(usage.ok_or("missing usage")?.output_tokens, Some(2));
    let request = requests.recv_timeout(Duration::from_secs(1))?;
    assert_eq!(request["max_tokens"], 8192);
    assert_eq!(request["stream"], false);
    assert!(request.get("stream_options").is_none());
    Ok(())
}
#[tokio::test]
async fn rejects_truncated_invalid_and_refused_streams_without_finished()
-> Result<(), Box<dyn std::error::Error>> {
    let scenarios=[
        ("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n".to_owned(),"incomplete_model_response"),
        (fixture::sse(&[chunk(json!({}),json!("length"))]),"output_limit"),
        (fixture::sse(&[chunk(json!({"refusal":"no"}),Value::Null)]),"provider_refusal"),
        (fixture::sse(&[chunk(json!({}),json!("mystery"))]),"invalid_model_response"),
        (fixture::sse(&[chunk(json!({"tool_calls":[{"index":0,"id":"x","type":"function","function":{"name":"read_file","arguments":"{"}}]}),Value::Null),chunk(json!({}),json!("tool_calls"))]),"invalid_model_response"),
    ];
    for (body, expected) in scenarios {
        let (url, _) = fixture::server(vec![("text/event-stream", body)])?;
        let model = ChatModel::new(
            transport(url)?,
            settings(true),
            ChatCompatibility::default(),
        );
        let mut stream = model.start(&request(), &Never).await?;
        loop {
            match stream.next(&Never).await {
                Ok(Some(ModelUpdate::Finished { .. })) => {
                    return Err("unexpected completion".into());
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err("unexpected EOF".into()),
                Err(e) => {
                    assert_eq!(e.0.code, expected);
                    break;
                }
            }
        }
    }
    Ok(())
}
#[tokio::test]
async fn strict_schema_and_context_budget_fail_before_network()
-> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = settings(true);
    cfg.strict_tools = true;
    let model = ChatModel::new(
        transport("http://127.0.0.1:1/v1".into())?,
        cfg,
        ChatCompatibility::default(),
    );
    let mut req = request();
    req.tools.push(ToolDefinition {
        name: "tool".into(),
        description: "test".into(),
        input_schema: json!({"type":"object","additionalProperties":{"type":"string"}}),
    });
    let err = match model.start(&req, &Never).await {
        Ok(_) => return Err("strict open map accepted".into()),
        Err(e) => e,
    };
    assert_eq!(err.0.code, "invalid_config");
    let mut cfg = settings(true);
    cfg.context_window_tokens = 10_000;
    cfg.max_output_tokens = 9000;
    let model = ChatModel::new(
        transport("http://127.0.0.1:1/v1".into())?,
        cfg,
        ChatCompatibility::default(),
    );
    let err = match model.start(&request(), &Never).await {
        Ok(_) => return Err("context guard bypassed".into()),
        Err(e) => e,
    };
    assert_eq!(err.0.code, "context_budget");
    Ok(())
}
#[tokio::test]
async fn chat_adapter_drives_runtime_tool_loop() -> Result<(), Box<dyn std::error::Error>> {
    let first = fixture::sse(&[
        chunk(
            json!({"tool_calls":[{"index":0,"id":"call-write","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"out.txt\",\"content\":\"ok\",\"expected_version\":null}"}}]}),
            Value::Null,
        ),
        chunk(json!({}), json!("tool_calls")),
    ]);
    let second = fixture::sse(&[
        chunk(json!({"content":"done"}), Value::Null),
        chunk(json!({}), json!("stop")),
    ]);
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", first),
        ("text/event-stream", second),
    ])?;
    let model = Arc::new(ChatModel::new(
        transport(url)?,
        settings(true),
        ChatCompatibility::default(),
    ));
    let temp = support::temp_workspace()?;
    let mut session = Session {
        id: SessionId::new(),
        workspace: temp.path().into(),
        work_mode: WorkMode::Work,
        profile: "test".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture-model".into(),
        history: vec![HistoryItem::UserText("create the file".into())],
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
        model,
        store: store.clone(),
        policy: Arc::new(Policy::new(temp.path(), PermissionConfig::default())?),
        files: Arc::new(ReadTools::new(
            temp.path(),
            &[],
            false,
            ReadLimits::default(),
        )?),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("test".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "create file".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })?;
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(std::fs::read_to_string(temp.path().join("out.txt"))?, "ok");
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history,
        session.history
    );
    let first = requests.recv_timeout(Duration::from_secs(1))?;
    let second = requests.recv_timeout(Duration::from_secs(1))?;
    let first_system = first["messages"][0]["content"]
        .as_str()
        .ok_or("missing system")?;
    let second_system = second["messages"][0]["content"]
        .as_str()
        .ok_or("missing system")?;
    assert_eq!(
        first_system.split("# Dynamic request context").next(),
        second_system.split("# Dynamic request context").next()
    );
    assert!(second_system.contains("\"captured_at\""));
    assert!(
        second["messages"]
            .as_array()
            .ok_or("missing messages")?
            .iter()
            .any(|m| m["role"] == "tool" && m["tool_call_id"] == "call-write")
    );
    Ok(())
}

#[tokio::test]
async fn strict_schema_is_explicit_and_review_requests_do_not_gain_tools()
-> Result<(), Box<dyn std::error::Error>> {
    let response = fixture::sse(&[
        chunk(json!({"content":"ok"}), Value::Null),
        chunk(json!({}), json!("stop")),
    ]);
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", response.clone()),
        ("text/event-stream", response),
    ])?;
    let mut cfg = settings(true);
    cfg.strict_tools = true;
    let model = ChatModel::new(transport(url)?, cfg, ChatCompatibility::default());
    let mut req = request();
    req.tools.push(ToolDefinition {name:"lookup".into(),description:"find".into(),input_schema:json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false})});
    collect(&model, &req).await?;
    let first = requests.recv_timeout(Duration::from_secs(1))?;
    assert_eq!(first["tools"][0]["function"]["strict"], true);
    let mut review = request();
    review.purpose = ModelPurpose::Review;
    review.max_output_tokens = Some(1024);
    collect(&model, &review).await?;
    let second = requests.recv_timeout(Duration::from_secs(1))?;
    assert!(second.get("tools").is_none());
    assert_eq!(second["max_completion_tokens"], 1024);
    review.tools = req.tools.clone();
    let err = match model.start(&review, &Never).await {
        Ok(_) => return Err("review request accepted tools".into()),
        Err(error) => error,
    };
    assert_eq!(err.0.code, "invalid_config");
    Ok(())
}

#[test]
fn session_model_and_protocol_cannot_change_under_a_profile()
-> Result<(), Box<dyn std::error::Error>> {
    let model = ChatModel::new(
        transport("http://127.0.0.1:1/v1".into())?,
        settings(true),
        ChatCompatibility::default(),
    );
    let temp = support::temp_workspace()?;
    let mut session = Session {
        id: SessionId::new(),
        workspace: temp.path().into(),
        work_mode: WorkMode::Work,
        profile: "test".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture-model".into(),
        history: vec![],
    };
    model.validate_session(&session)?;
    session.model = "different".into();
    assert_eq!(
        model
            .validate_session(&session)
            .expect_err("changed model")
            .0
            .code,
        "invalid_config"
    );
    session.model = "fixture-model".into();
    session.protocol = ModelProtocol::Responses;
    assert_eq!(
        model
            .validate_session(&session)
            .expect_err("changed protocol")
            .0
            .code,
        "invalid_config"
    );
    Ok(())
}
