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
        extra_safety_tokens: 0,
        max_output_tokens: 8192,
        multimodal: Default::default(),
        upstream_max_output_tokens: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        thinking_effort: None,
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
        media: Default::default(),
        tools: vec![],
    }
}
fn chunk(delta: Value, finish: Value) -> Value {
    json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":null})
}
#[tokio::test]
async fn combined_reasoning_and_visible_deltas_are_delivered_independently()
-> Result<(), Box<dyn std::error::Error>> {
    let body = fixture::sse(&[
        chunk(
            json!({"role":"assistant","reasoning_content":"first ","content":"visible "}),
            Value::Null,
        ),
        chunk(
            json!({"reasoning_content":"second","content":"answer"}),
            Value::Null,
        ),
        chunk(json!({}), json!("stop")),
    ]);
    let (url, _) = fixture::server(vec![("text/event-stream", body)])?;
    let model = ChatModel::new(
        transport(url)?,
        settings(true),
        ChatCompatibility::default(),
    );
    let request = request();
    let mut stream = model.start(&request, &Never).await?;
    let mut updates = Vec::new();
    loop {
        match stream.next(&Never).await? {
            Some(ModelUpdate::ThinkingDelta { text }) => updates.push(("thinking", text)),
            Some(ModelUpdate::TextDelta(text)) => updates.push(("visible", text)),
            Some(ModelUpdate::Finished { items, .. }) => {
                assert!(
                    matches!(&items[..], [HistoryItem::ProviderItem { value, .. }, HistoryItem::AssistantText(text)]
                    if value["reasoning_content"]=="first second" && text=="visible answer")
                );
                break;
            }
            Some(ModelUpdate::Progress) => {}
            None => return Err("missing completion".into()),
        }
    }
    assert_eq!(
        updates,
        vec![
            ("thinking", "first ".into()),
            ("visible", "visible ".into()),
            ("thinking", "second".into()),
            ("visible", "answer".into()),
        ]
    );
    Ok(())
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
            Some(ModelUpdate::Progress | ModelUpdate::ThinkingDelta { .. }) => {}
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
async fn enabled_media_is_sent_as_chat_content_parts() -> Result<(), Box<dyn std::error::Error>> {
    let response = json!({
        "choices":[{
            "index":0,
            "finish_reason":"stop",
            "message":{"role":"assistant","content":"ok"}
        }],
        "usage":{"prompt_tokens":9,"completion_tokens":1}
    })
    .to_string();
    let (url, requests) = fixture::server(vec![("application/json", response)])?;
    let root = tempfile::tempdir()?;
    let image = root.path().join("image.png");
    let audio = root.path().join("audio.wav");
    let video = root.path().join("video.mp4");
    std::fs::write(&image, b"png")?;
    std::fs::write(&audio, b"wav")?;
    std::fs::write(&video, b"mp4")?;
    let mut cfg = settings(false);
    cfg.multimodal = saya_core::config::MediaCapabilities {
        image: true,
        audio: true,
        video: true,
    };
    let capabilities = cfg.multimodal;
    let model = ChatModel::new(transport(url)?, cfg, ChatCompatibility::default());
    let mut req = request();
    let session_id = SessionId::new();
    let store = Arc::new(saya_core::output_store::OutputStore::open(
        root.path().join("outputs"),
        Default::default(),
        Arc::new(saya_core::credentials::SecretRegistry::default()),
    )?);
    let input = format!(
        "Inspect [Image 1](<{}>) [Audio 1](<{}>) [Video 1](<{}>)",
        image.display(),
        audio.display(),
        video.display()
    );
    let capture = saya_core::media::capture_user(&input, capabilities, &store, session_id)?;
    req.history = vec![HistoryItem::UserText(capture.text.clone())];
    req.media = Arc::new(saya_core::media::load_history(
        &req.history,
        Some(&store),
        session_id,
    )?);
    std::fs::remove_file(&image)?;
    std::fs::remove_file(&audio)?;
    std::fs::remove_file(&video)?;
    drop(capture);
    collect(&model, &req).await?;

    let body = requests.recv_timeout(Duration::from_secs(1))?;
    let parts = body["messages"][1]["content"]
        .as_array()
        .ok_or("missing multimodal content")?;
    let image = parts
        .iter()
        .find(|part| part["type"] == "image_url")
        .ok_or("missing image part")?;
    assert!(
        image["image_url"]["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("data:image/png;base64,"))
    );
    let audio = parts
        .iter()
        .find(|part| part["type"] == "input_audio")
        .ok_or("missing audio part")?;
    assert_eq!(audio["input_audio"]["format"], "wav");
    let video = parts
        .iter()
        .find(|part| part["type"] == "file")
        .ok_or("missing video part")?;
    assert_eq!(video["file"]["filename"], "video.mp4");
    assert!(
        video["file"]["file_data"]
            .as_str()
            .is_some_and(|url| url.starts_with("data:video/mp4;base64,"))
    );
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
    let mut full = request();
    full.history = vec![HistoryItem::UserText("x".repeat(40_000))];
    let err = match model.start(&full, &Never).await {
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
            json!({"reasoning_content":"opaque-runtime-first"}),
            Value::Null,
        ),
        chunk(
            json!({"tool_calls":[{"index":0,"id":"call-write","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"out.txt\",\"content\":\"ok\",\"expected_version\":null}"}}]}),
            Value::Null,
        ),
        chunk(json!({}), json!("tool_calls")),
    ]);
    let second = fixture::sse(&[
        chunk(
            json!({"reasoning_content":"opaque-runtime-final"}),
            Value::Null,
        ),
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
    assert_eq!(result.text.as_deref(), Some("done"));
    assert_eq!(
        session
            .history
            .iter()
            .filter(|item| matches!(item, HistoryItem::ProviderItem { .. }))
            .count(),
        2
    );
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
    assert!(
        second["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "assistant"
                && message["reasoning_content"] == "opaque-runtime-first")
    );
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

fn reasoning_response(
    stream: bool,
    visible: &str,
    reasoning: Option<&str>,
    call: Option<&str>,
) -> (&'static str, String) {
    let mut message = json!({"role":"assistant","content":visible});
    if let Some(reasoning) = reasoning {
        message["reasoning_content"] = json!(reasoning);
    }
    if let Some(id) = call {
        message["tool_calls"] = json!([{"id":id,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]);
    }
    let finish = if call.is_some() { "tool_calls" } else { "stop" };
    if stream {
        let mut events = Vec::new();
        if let Some(reasoning) = reasoning {
            let middle = reasoning.floor_char_boundary(reasoning.len() / 2);
            events.push(chunk(
                json!({"reasoning_content":&reasoning[..middle]}),
                Value::Null,
            ));
            events.push(chunk(
                json!({"reasoning_content":&reasoning[middle..]}),
                Value::Null,
            ));
        }
        events.push(chunk(json!({"content":visible}), Value::Null));
        if call.is_some() {
            message["tool_calls"][0]["index"] = json!(0);
            events.push(chunk(
                json!({"tool_calls":message["tool_calls"]}),
                Value::Null,
            ));
        }
        events.push(chunk(json!({}), json!(finish)));
        ("text/event-stream", fixture::sse(&events))
    } else {
        (
            "application/json",
            json!({"choices":[{"index":0,"finish_reason":finish,"message":message}]}).to_string(),
        )
    }
}

#[tokio::test]
async fn reasoning_is_opaque_and_replayed_per_turn_after_sqlite_restore()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        boundary::StoreChange,
        export::{ExportOptions, ExportScope, SessionExport},
        sqlite::{SnapshotLimits, SqliteStore},
    };
    for stream in [false, true] {
        let (url, requests) = fixture::server(vec![
            reasoning_response(stream, "checking", Some("opaque-first"), Some("call-a")),
            reasoning_response(
                stream,
                "checking again",
                Some("opaque-second"),
                Some("call-b"),
            ),
            reasoning_response(stream, "done", Some("opaque-final"), None),
            reasoning_response(stream, "followup", None, None),
        ])?;
        let mut config = settings(stream);
        config.thinking_effort = Some(saya_core::config::ThinkingEffort::Max);
        let model = ChatModel::new(transport(url)?, config, ChatCompatibility::default());
        let temp = support::temp_workspace()?;
        let root = temp.path().join("store");
        let mut session = Session {
            id: SessionId::new(),
            workspace: temp.path().into(),
            work_mode: WorkMode::Work,
            profile: "test".into(),
            protocol: ModelProtocol::ChatCompletions,
            model: "fixture-model".into(),
            history: request().history,
        };
        SqliteStore::open(&root, vec![])?
            .create_session(&session, "fixture")
            .await?;
        for turn in 0..4 {
            let mut req = request();
            req.history = session.history.clone();
            req.tools = vec![ToolDefinition {
                name: "read_file".into(),
                description: "read".into(),
                input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
            }];
            let (items, _, delta) = collect(&model, &req).await?;
            assert!(!delta.contains("opaque-"));
            if turn < 3 {
                assert!(matches!(
                    &items[0],
                    HistoryItem::ProviderItem {
                        protocol: ModelProtocol::ChatCompletions,
                        value
                    } if value.as_object().unwrap().len() == 1
                ));
            } else {
                assert_eq!(items, vec![HistoryItem::AssistantText("followup".into())]);
            }
            let calls: Vec<_> = items
                .iter()
                .filter_map(|item| {
                    if let HistoryItem::ToolCall(call) = item {
                        Some(call.call_id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            session.history.extend(items);
            for call_id in calls {
                session.history.push(HistoryItem::ToolResult {
                    call_id,
                    result: ToolResult {
                        ok: true,
                        data: Some(json!({"text":"a"})),
                        error: None,
                        truncated: false,
                    },
                });
            }
            if turn == 2 {
                session
                    .history
                    .push(HistoryItem::UserText("next task".into()));
            }
            let store = SqliteStore::open(&root, vec![])?;
            store
                .commit(&[StoreChange::Session(session.clone())])
                .await?;
            drop(store);
            session = SqliteStore::open(&root, vec![])?
                .load_session(session.id)
                .await?
                .ok_or("session lost")?;
        }
        let mut captured = Vec::new();
        for _ in 0..4 {
            captured.push(requests.recv_timeout(Duration::from_secs(1))?);
        }
        let assistants: Vec<_> = captured[3]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["role"] == "assistant")
            .collect();
        assert_eq!(assistants.len(), 3);
        for (index, (text, reasoning)) in [
            ("checking", "opaque-first"),
            ("checking again", "opaque-second"),
            ("done", "opaque-final"),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(assistants[index]["content"], text);
            assert_eq!(assistants[index]["reasoning_content"], reasoning);
        }
        assert_eq!(assistants[0]["tool_calls"][0]["id"], "call-a");
        assert_eq!(assistants[1]["tool_calls"][0]["id"], "call-b");
        assert!(assistants[2].get("tool_calls").is_none());
        let snapshot = SqliteStore::read_snapshot(
            &root,
            session.id,
            SnapshotLimits::default(),
            Arc::new(Never),
        )
        .await?;
        for scope in [ExportScope::Transcript, ExportScope::Full] {
            let export = SessionExport::from_snapshot(
                &snapshot,
                ExportOptions {
                    scope,
                    include_tools: true,
                    ..Default::default()
                },
                &[],
            )?;
            for bytes in [export.to_json()?, export.to_markdown()?] {
                assert!(!String::from_utf8(bytes)?.contains("opaque-"));
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn invalid_reasoning_replay_is_rejected_before_http() -> Result<(), Box<dyn std::error::Error>>
{
    let model = ChatModel::new(
        transport("http://127.0.0.1:1/v1".into())?,
        settings(false),
        ChatCompatibility::default(),
    );
    let opaque = |value| HistoryItem::ProviderItem {
        protocol: ModelProtocol::ChatCompletions,
        value,
    };
    let valid = || opaque(json!({"reasoning_content":"opaque"}));
    let assistant = || HistoryItem::AssistantText("visible".into());
    let mut cases = vec![
        vec![valid()],
        vec![valid(), valid(), assistant()],
        vec![valid(), HistoryItem::UserText("user".into())],
        vec![valid(), HistoryItem::AssistantPartial("partial".into())],
        vec![valid(), HistoryItem::AssistantText(String::new())],
        vec![
            HistoryItem::ProviderItem {
                protocol: ModelProtocol::Responses,
                value: json!({"reasoning_content":"opaque"}),
            },
            assistant(),
        ],
    ];
    for value in [
        json!({}),
        json!({"reasoning_content":null}),
        json!({"reasoning_content":7}),
        json!({"reasoning_content":[]}),
        json!({"reasoning_content":"opaque","role":"assistant"}),
    ] {
        cases.push(vec![opaque(value), assistant()]);
    }
    cases.push(vec![
        opaque(json!({"reasoning_content":"a".repeat(16*1024*1024+1)})),
        assistant(),
    ]);
    cases.push(vec![
        opaque(json!({"reasoning_content":"a".repeat(8*1024*1024)})),
        HistoryItem::AssistantText("b".repeat(8 * 1024 * 1024 + 1)),
    ]);
    for history in cases {
        let mut req = request();
        req.history = history;
        let error = match model.start(&req, &Never).await {
            Ok(_) => return Err("invalid replay reached transport".into()),
            Err(error) => error,
        };
        assert_eq!(error.0.code, "invalid_history");
    }
    Ok(())
}

#[tokio::test]
async fn malformed_or_reasoning_only_responses_never_finish_as_visible_output()
-> Result<(), Box<dyn std::error::Error>> {
    for stream in [false, true] {
        for reasoning in [
            json!(true),
            json!([]),
            json!({"content":"bad"}),
            json!("opaque-only"),
        ] {
            let body = if stream {
                fixture::sse(&[
                    chunk(json!({"reasoning_content":reasoning}), Value::Null),
                    chunk(json!({}), json!("stop")),
                ])
            } else {
                json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","reasoning_content":reasoning}}]}).to_string()
            };
            let (url, _) = fixture::server(vec![(
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
                body,
            )])?;
            let model = ChatModel::new(
                transport(url)?,
                settings(stream),
                ChatCompatibility::default(),
            );
            let error = collect(&model, &request())
                .await
                .err()
                .ok_or("invalid response finished")?;
            assert_eq!(
                error
                    .downcast_ref::<saya_core::contract::BoundaryError>()
                    .ok_or("wrong error type")?
                    .0
                    .code,
                "invalid_model_response"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn sqlite_rejects_secret_bearing_chat_reasoning_without_partial_history_writes()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{boundary::StoreChange, sqlite::SqliteStore};
    let temp = support::temp_workspace()?;
    let store = SqliteStore::open(temp.path(), vec!["fixture-private-token".into()])?;
    let session = Session {
        id: SessionId::new(),
        workspace: temp.path().into(),
        work_mode: WorkMode::Work,
        profile: "test".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture-model".into(),
        history: vec![HistoryItem::UserText("original message".into())],
    };
    store.create_session(&session, "fixture").await?;
    let error = store
        .commit(&[
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::UserText("must remain uncommitted".into()),
            },
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::ProviderItem {
                    protocol: ModelProtocol::ChatCompletions,
                    value: json!({"reasoning_content": "opaque fixture-private-token"}),
                },
            },
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::AssistantText("visible".into()),
            },
        ])
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "storage_sensitive_data");
    assert!(!error.0.message.contains("fixture-private-token"));
    assert_eq!(
        store
            .load_session(session.id)
            .await?
            .ok_or("missing session")?
            .history,
        session.history
    );
    Ok(())
}
