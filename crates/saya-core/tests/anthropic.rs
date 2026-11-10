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
        anthropic::{AnthropicModel, AnthropicVersion},
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
        tools: vec![],
    }
}
fn event(kind: &str, mut data: Value) -> Value {
    data["type"] = json!(kind);
    data
}
fn start() -> Value {
    event(
        "message_start",
        json!({"message":{"role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":0}}}),
    )
}
fn block_start(index: usize, content: Value) -> Value {
    event(
        "content_block_start",
        json!({"index":index,"content_block":content}),
    )
}
fn delta(index: usize, delta: Value) -> Value {
    event("content_block_delta", json!({"index":index,"delta":delta}))
}
fn block_stop(index: usize) -> Value {
    event("content_block_stop", json!({"index":index}))
}
#[tokio::test]
async fn thinking_deltas_exclude_signatures_and_redacted_blocks()
-> Result<(), Box<dyn std::error::Error>> {
    let mut events = vec![
        start(),
        block_start(0, json!({"type":"thinking","thinking":""})),
        delta(
            0,
            json!({"type":"thinking_delta","thinking":"public thought"}),
        ),
        delta(
            0,
            json!({"type":"signature_delta","signature":"opaque-signature"}),
        ),
        block_stop(0),
        block_start(
            1,
            json!({"type":"redacted_thinking","data":"opaque-redacted"}),
        ),
        block_stop(1),
        block_start(2, json!({"type":"text","text":"answer"})),
        block_stop(2),
    ];
    events.extend(stop("end_turn"));
    let (url, requests) = fixture::server(vec![("text/event-stream", sse(&events))])?;
    let model = AnthropicModel::new(transport(url)?, settings(true), AnthropicVersion::default());
    let request = request();
    let mut stream = model.start(&request, &Never).await?;
    let mut thinking = String::new();
    loop {
        match stream.next(&Never).await? {
            Some(ModelUpdate::ThinkingDelta { text }) => thinking.push_str(&text),
            Some(ModelUpdate::Finished { items, .. }) => {
                assert!(items.iter().any(
                    |item| matches!(item, HistoryItem::ProviderItem { value, .. }
                    if value["signature"]=="opaque-signature")
                ));
                assert!(items.iter().any(
                    |item| matches!(item, HistoryItem::AssistantText(text) if text=="answer")
                ));
                break;
            }
            Some(_) => {}
            None => return Err("missing completion".into()),
        }
    }
    assert_eq!(thinking, "public thought");
    assert!(
        requests
            .recv_timeout(Duration::from_secs(1))?
            .get("thinking")
            .is_none()
    );
    Ok(())
}
fn stop(reason: &str) -> Vec<Value> {
    vec![
        event(
            "message_delta",
            json!({"delta":{"stop_reason":reason},"usage":{"output_tokens":4}}),
        ),
        event("message_stop", json!({})),
    ]
}
fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| {
            format!(
                "event: {}\ndata: {}\n\n",
                e["type"].as_str().unwrap_or(""),
                e
            )
        })
        .collect()
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
            Some(ModelUpdate::TextDelta(s)) => text.push_str(&s),
            Some(ModelUpdate::Progress | ModelUpdate::ThinkingDelta { .. }) => {}
            Some(ModelUpdate::Finished { items, usage }) => return Ok((items, usage, text)),
            None => return Err("missing completion".into()),
        }
    }
}

#[tokio::test]
async fn explicit_cache_breakpoint_precedes_dynamic_context_and_is_opt_in()
-> Result<(), Box<dyn std::error::Error>> {
    let mut events = vec![
        start(),
        block_start(0, json!({"type":"text","text":"ok"})),
        block_stop(0),
    ];
    events.extend(stop("end_turn"));
    let body = sse(&events);
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", body.clone()),
        ("text/event-stream", body.clone()),
        ("text/event-stream", body),
    ])?;
    let transport = transport(url)?;
    let model = AnthropicModel::new(
        transport.clone(),
        settings(true),
        AnthropicVersion::default(),
    )
    .with_prompt_cache(true);
    let mut req = request();
    req.dynamic_instructions = Some("dynamic one".into());
    collect(&model, &req).await?;
    req.dynamic_instructions = Some("dynamic two".into());
    collect(&model, &req).await?;
    let fallback = AnthropicModel::new(transport, settings(true), AnthropicVersion::default());
    collect(&fallback, &req).await?;
    let first = requests.recv_timeout(Duration::from_secs(1))?;
    let second = requests.recv_timeout(Duration::from_secs(1))?;
    let third = requests.recv_timeout(Duration::from_secs(1))?;
    assert_eq!(first["system"][0], second["system"][0]);
    assert_eq!(first["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(first["system"][1]["text"], "dynamic one");
    assert_eq!(second["system"][1]["text"], "dynamic two");
    assert!(second["system"][1].get("cache_control").is_none());
    assert_eq!(third["system"], "be helpfuldynamic two");
    assert!(third.get("cache_control").is_none());
    Ok(())
}

#[tokio::test]
async fn cached_system_omits_empty_blocks_and_keeps_strict_tools_explicit()
-> Result<(), Box<dyn std::error::Error>> {
    for (instructions, dynamic) in [
        ("", ""),
        ("static", ""),
        ("", "dynamic"),
        ("static", "dynamic"),
    ] {
        let response = json!({"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"ok"}]}).to_string();
        let (url, requests) = fixture::server(vec![("application/json", response)])?;
        let model = AnthropicModel::new(
            transport(url)?,
            settings(false),
            AnthropicVersion::V20230601,
        )
        .with_prompt_cache(true);
        let mut req = request();
        req.instructions = instructions.into();
        req.dynamic_instructions = Some(dynamic.into());
        req.tools.push(ToolDefinition {
            name: "read_file".into(),
            description: "read".into(),
            input_schema: json!({"type":"object"}),
        });
        collect(&model, &req).await?;
        let body = requests.recv_timeout(Duration::from_secs(1))?;
        if instructions.is_empty() && dynamic.is_empty() {
            assert!(body.get("system").is_none());
        } else {
            let blocks = body["system"].as_array().ok_or("missing system blocks")?;
            assert!(
                blocks
                    .iter()
                    .all(|block| block["text"].as_str().is_some_and(|text| !text.is_empty()))
            );
            let text = blocks
                .iter()
                .map(|block| block["text"].as_str().unwrap())
                .collect::<String>();
            assert_eq!(text, format!("{instructions}{dynamic}"));
        }
        assert!(body["tools"][0].get("strict").is_none());
    }
    for version in [AnthropicVersion::V20230601, AnthropicVersion::V20261021] {
        let response = json!({"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"ok"}]}).to_string();
        let (url, requests) = fixture::server(vec![("application/json", response)])?;
        let mut settings = settings(false);
        settings.strict_tools = true;
        let model = AnthropicModel::new(transport(url)?, settings, version);
        let mut req = request();
        req.tools.push(ToolDefinition {
            name: "read_file".into(),
            description: "read".into(),
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        });
        collect(&model, &req).await?;
        assert_eq!(
            requests.recv_timeout(Duration::from_secs(1))?["tools"][0]["strict"],
            true
        );
    }
    Ok(())
}

#[tokio::test]
async fn cache_usage_totals_match_complete_and_cumulative_stream_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let full = json!({
        "role":"assistant", "stop_reason":"end_turn",
        "content":[{"type":"text","text":"ok"}],
        "usage":{"input_tokens":9,"output_tokens":4,"cache_creation_input_tokens":2,"cache_read_input_tokens":3}
    });
    let mut first = start();
    first["message"]["usage"]["cache_creation_input_tokens"] = json!(10);
    first["message"]["usage"]["cache_read_input_tokens"] = json!(20);
    let mut events = vec![
        first,
        block_start(0, json!({"type":"text","text":"ok"})),
        block_stop(0),
    ];
    let mut end = stop("end_turn");
    end[0]["usage"] = json!({"input_tokens":13,"output_tokens":4,"cache_creation_input_tokens":15,"cache_read_input_tokens":25});
    events.extend(end);
    for (stream, response, expected) in [(false, full.to_string(), 14), (true, sse(&events), 53)] {
        let content_type = if stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        let (url, _) = fixture::server(vec![(content_type, response)])?;
        let model = AnthropicModel::new(
            transport(url)?,
            settings(stream),
            AnthropicVersion::default(),
        );
        let (_, usage, _) = collect(&model, &request()).await?;
        let usage = usage.ok_or("missing usage")?;
        assert_eq!(usage.input_tokens, Some(expected));
        assert_eq!(usage.output_tokens, Some(4));
        assert!(!usage.estimated);
    }
    Ok(())
}

#[tokio::test]
async fn malformed_stream_usage_is_rejected_instead_of_becoming_unknown()
-> Result<(), Box<dyn std::error::Error>> {
    for usage in [
        json!({"input_tokens":"12","output_tokens":0}),
        json!({"input_tokens":12,"output_tokens":-1}),
        json!({"input_tokens":12,"output_tokens":0,"cache_read_input_tokens":"many"}),
        json!({"input_tokens":u64::MAX,"output_tokens":0,"cache_creation_input_tokens":1}),
    ] {
        for initial in [true, false] {
            let mut events = vec![
                start(),
                block_start(0, json!({"type":"text","text":"ok"})),
                block_stop(0),
            ];
            let mut end = stop("end_turn");
            if initial {
                events[0]["message"]["usage"] = usage.clone();
            } else {
                end[0]["usage"] = usage.clone();
            }
            events.extend(end);
            let (url, _) = fixture::server(vec![("text/event-stream", sse(&events))])?;
            let model =
                AnthropicModel::new(transport(url)?, settings(true), AnthropicVersion::default());
            assert!(
                collect(&model, &request()).await.is_err(),
                "invalid usage accepted: {usage}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn concurrent_cold_prefix_waits_for_first_stream_event()
-> Result<(), Box<dyn std::error::Error>> {
    let mut events = vec![
        start(),
        block_start(0, json!({"type":"text","text":"ok"})),
        block_stop(0),
    ];
    events.extend(stop("end_turn"));
    let body = sse(&events);
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", body.clone()),
        ("text/event-stream", body),
    ])?;
    let model = Arc::new(
        AnthropicModel::new(transport(url)?, settings(true), AnthropicVersion::default())
            .with_prompt_cache(true),
    );
    let mut req = request();
    req.dynamic_instructions = Some("time one".into());
    let mut first = model.start(&req, &Never).await?;
    let _ = requests.recv_timeout(Duration::from_secs(1))?;
    req.dynamic_instructions = Some("time two".into());
    let other = model.clone();
    let mut second = tokio::spawn(async move { other.start(&req, &Never).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err()
    );
    assert!(requests.try_recv().is_err());
    assert!(matches!(
        first.next(&Never).await?,
        Some(ModelUpdate::Progress)
    ));
    let _stream = tokio::time::timeout(Duration::from_secs(2), &mut second).await???;
    let _ = requests.recv_timeout(Duration::from_secs(1))?;
    Ok(())
}

#[tokio::test]
async fn dropping_cold_stream_releases_waiting_request() -> Result<(), Box<dyn std::error::Error>> {
    let mut events = vec![
        start(),
        block_start(0, json!({"type":"text","text":"ok"})),
        block_stop(0),
    ];
    events.extend(stop("end_turn"));
    let body = sse(&events);
    let (url, _) = fixture::server(vec![
        ("text/event-stream", body.clone()),
        ("text/event-stream", body),
    ])?;
    let model = Arc::new(
        AnthropicModel::new(transport(url)?, settings(true), AnthropicVersion::default())
            .with_prompt_cache(true),
    );
    let mut req = request();
    req.dynamic_instructions = Some("time".into());
    let first = model.start(&req, &Never).await?;
    let other = model.clone();
    let mut second = tokio::spawn(async move { other.start(&req, &Never).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err()
    );
    drop(first);
    let _stream = tokio::time::timeout(Duration::from_secs(2), &mut second).await???;
    Ok(())
}
#[tokio::test]
async fn both_versions_stream_text_without_sampling_parameters()
-> Result<(), Box<dyn std::error::Error>> {
    let mut events = vec![
        start(),
        block_start(0, json!({"type":"text","text":"hel"})),
        delta(0, json!({"type":"text_delta","text":"lo"})),
        block_stop(0),
    ];
    events.extend(stop("end_turn"));
    let body = sse(&events);
    let (url, requests) = fixture::server_with_headers(vec![
        ("text/event-stream", body.clone()),
        ("text/event-stream", body),
    ])?;
    let client = transport(url)?;
    for version in [AnthropicVersion::V20261021, AnthropicVersion::V20230601] {
        let model = AnthropicModel::new(client.clone(), settings(true), version);
        let (items, usage, text) = collect(&model, &request()).await?;
        assert_eq!(text, "hello");
        assert!(
            items
                .iter()
                .any(|i| matches!(i,HistoryItem::AssistantText(s) if s=="hello"))
        );
        let usage = usage.ok_or("missing usage")?;
        assert_eq!(
            (usage.input_tokens, usage.output_tokens),
            (Some(12), Some(4))
        );
        let (body, headers) = requests.recv_timeout(Duration::from_secs(1))?;
        let wanted = match version {
            AnthropicVersion::V20261021 => "2026-10-21",
            AnthropicVersion::V20230601 => "2023-06-01",
        };
        assert!(
            headers
                .to_ascii_lowercase()
                .contains(&format!("anthropic-version: {wanted}"))
        );
        assert_eq!(body["max_tokens"], 8192);
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("top_k").is_none());
    }
    Ok(())
}
#[tokio::test]
async fn preserves_signed_thinking_and_groups_tool_results_in_order()
-> Result<(), Box<dyn std::error::Error>> {
    let mut events = vec![
        start(),
        block_start(0, json!({"type":"thinking","thinking":""})),
        delta(0, json!({"type":"thinking_delta","thinking":"hidden"})),
        delta(
            0,
            json!({"type":"signature_delta","signature":"signature-fixture"}),
        ),
        block_stop(0),
        block_start(
            1,
            json!({"type":"tool_use","id":"call-a","name":"read_file","input":{}}),
        ),
        delta(
            1,
            json!({"type":"input_json_delta","partial_json":"{\"path\":"}),
        ),
        delta(
            1,
            json!({"type":"input_json_delta","partial_json":"\"a.txt\"}"}),
        ),
        block_stop(1),
        block_start(
            2,
            json!({"type":"redacted_thinking","data":"opaque-fixture"}),
        ),
        block_stop(2),
        block_start(
            3,
            json!({"type":"tool_use","id":"call-b","name":"list_dir","input":{"path":"."}}),
        ),
        block_stop(3),
        block_start(4, json!({"type":"text","text":""})),
        block_stop(4),
    ];
    events.extend(stop("tool_use"));
    let mut second = vec![
        start(),
        block_start(0, json!({"type":"text","text":""})),
        delta(0, json!({"type":"text_delta","text":"done"})),
        block_stop(0),
    ];
    second.extend(stop("end_turn"));
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", sse(&events)),
        ("text/event-stream", sse(&second)),
    ])?;
    let model = AnthropicModel::new(transport(url)?, settings(true), AnthropicVersion::default());
    let mut req = request();
    req.tools.push(ToolDefinition {
        name: "read_file".into(),
        description: "read".into(),
        input_schema: json!({"type":"object"}),
    });
    let (items, _, text) = collect(&model, &req).await?;
    assert!(text.is_empty());
    let ids: Vec<_> = items
        .iter()
        .filter_map(|i| {
            if let HistoryItem::ToolCall(c) = i {
                Some(c.call_id.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ids, ["call-a", "call-b"]);
    assert!(!items.iter().any(|item| matches!(item,
        HistoryItem::ProviderItem { value, .. } if value["type"] == "text" && value["text"] == ""
    )));
    req.history.push(HistoryItem::ProviderItem {
        protocol: ModelProtocol::AnthropicMessages,
        value: json!({"type":"text","text":""}),
    });
    req.history.push(HistoryItem::AssistantText(String::new()));
    req.history.extend(items);
    for (i, id) in ids.iter().enumerate() {
        req.history.push(HistoryItem::ToolResult {
            call_id: id.clone(),
            result: ToolResult {
                ok: i == 0,
                data: None,
                error: None,
                truncated: false,
            },
        });
    }
    assert_eq!(collect(&model, &req).await?.2, "done");
    let _ = requests.recv_timeout(Duration::from_secs(1))?;
    let second = requests.recv_timeout(Duration::from_secs(1))?;
    let messages = second["messages"].as_array().ok_or("missing messages")?;
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .ok_or("missing assistant")?;
    let blocks = assistant["content"].as_array().ok_or("missing blocks")?;
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["signature"], "signature-fixture");
    assert_eq!(blocks[1]["id"], "call-a");
    assert_eq!(blocks[2]["type"], "redacted_thinking");
    assert_eq!(blocks[3]["id"], "call-b");
    let last = messages.last().ok_or("missing result")?;
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"][0]["tool_use_id"], "call-a");
    assert_eq!(last["content"][1]["tool_use_id"], "call-b");
    assert_eq!(last["content"][1]["is_error"], true);
    Ok(())
}
#[tokio::test]
async fn complete_json_maps_multiple_tools_and_usage() -> Result<(), Box<dyn std::error::Error>> {
    let response = json!({"role":"assistant","stop_reason":"tool_use","content":[{"type":"text","text":""},{"type":"text","text":"checking"},{"type":"tool_use","id":"one","name":"read_file","input":{"path":"a"}},{"type":"tool_use","id":"two","name":"list_dir","input":{"path":"."}}],"usage":{"input_tokens":9,"output_tokens":4}});
    let (url, _) = fixture::server(vec![("application/json", response.to_string())])?;
    let model = AnthropicModel::new(
        transport(url)?,
        settings(false),
        AnthropicVersion::default(),
    );
    let (items, usage, _) = collect(&model, &request()).await?;
    assert_eq!(usage.ok_or("missing usage")?.output_tokens, Some(4));
    assert!(
        !items
            .iter()
            .any(|item| matches!(item, HistoryItem::AssistantText(text) if text.is_empty()))
    );
    assert_eq!(
        items
            .iter()
            .filter(|i| matches!(i, HistoryItem::ToolCall(_)))
            .count(),
        2
    );
    Ok(())
}
#[tokio::test]
async fn rejects_incomplete_streams_and_maps_limits() -> Result<(), Box<dyn std::error::Error>> {
    let truncated = vec![
        start(),
        block_start(
            0,
            json!({"type":"tool_use","id":"a","name":"read_file","input":{}}),
        ),
        delta(0, json!({"type":"input_json_delta","partial_json":"{"})),
        block_stop(0),
    ];
    let mut limited = truncated.clone();
    limited.extend(stop("max_tokens"));
    let mut invalid = truncated.clone();
    invalid.extend(stop("tool_use"));
    let scenarios = [
        (sse(&truncated), "incomplete_model_response"),
        (sse(&limited), "output_limit"),
        (sse(&invalid), "invalid_model_response"),
        (
            sse(&[
                start(),
                event(
                    "message_delta",
                    json!({"delta":{"stop_reason":"model_context_window_exceeded"}}),
                ),
                event("message_stop", json!({})),
            ]),
            "context_budget",
        ),
        (
            sse(&[
                start(),
                event("message_delta", json!({"delta":{"stop_reason":"refusal"}})),
                event("message_stop", json!({})),
            ]),
            "provider_refusal",
        ),
    ];
    for (body, expected) in scenarios {
        let (url, _) = fixture::server(vec![("text/event-stream", body)])?;
        let model =
            AnthropicModel::new(transport(url)?, settings(true), AnthropicVersion::default());
        let mut stream = model.start(&request(), &Never).await?;
        loop {
            match stream.next(&Never).await {
                Ok(Some(ModelUpdate::Finished { .. })) => return Err("unexpected success".into()),
                Ok(Some(_)) => {}
                Ok(None) => return Err("unexpected end".into()),
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
async fn rejects_missing_signature_and_mismatched_replay() -> Result<(), Box<dyn std::error::Error>>
{
    let response = json!({"role":"assistant","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"ok"}]});
    let (url, _) = fixture::server(vec![("application/json", response.to_string())])?;
    let model = AnthropicModel::new(
        transport(url)?,
        settings(false),
        AnthropicVersion::default(),
    );
    let err = match model.start(&request(), &Never).await {
        Ok(_) => return Err("missing signature accepted".into()),
        Err(e) => e,
    };
    assert_eq!(err.0.code, "invalid_model_response");
    let model = AnthropicModel::new(
        transport("http://127.0.0.1:1/v1".into())?,
        settings(true),
        AnthropicVersion::default(),
    );
    let mut req = request();
    req.history.push(HistoryItem::ProviderItem {
        protocol: ModelProtocol::AnthropicMessages,
        value: json!({"type":"tool_use","id":"real","name":"read_file","input":{}}),
    });
    req.history
        .push(HistoryItem::ToolCall(saya_core::contract::ToolCall {
            call_id: "fake".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }));
    let err = match model.start(&req, &Never).await {
        Ok(_) => return Err("mismatched replay accepted".into()),
        Err(e) => e,
    };
    assert_eq!(err.0.code, "invalid_history");
    Ok(())
}
#[tokio::test]
async fn anthropic_adapter_drives_core_tool_loop() -> Result<(), Box<dyn std::error::Error>> {
    let mut first = vec![
        start(),
        block_start(
            0,
            json!({"type":"tool_use","id":"call-write","name":"write_file","input":{}}),
        ),
        delta(
            0,
            json!({"type":"input_json_delta","partial_json":"{\"path\":\"out.txt\",\"content\":\"ok\",\"expected_version\":null}"}),
        ),
        block_stop(0),
    ];
    first.extend(stop("tool_use"));
    let mut second = vec![
        start(),
        block_start(0, json!({"type":"text","text":""})),
        delta(0, json!({"type":"text_delta","text":"done"})),
        block_stop(0),
    ];
    second.extend(stop("end_turn"));
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", sse(&first)),
        ("text/event-stream", sse(&second)),
    ])?;
    let model = Arc::new(AnthropicModel::new(
        transport(url)?,
        settings(true),
        AnthropicVersion::default(),
    ));
    let temp = support::temp_workspace()?;
    let mut session = Session {
        id: SessionId::new(),
        workspace: temp.path().into(),
        work_mode: WorkMode::Work,
        profile: "test".into(),
        protocol: ModelProtocol::AnthropicMessages,
        model: "fixture-model".into(),
        history: vec![HistoryItem::UserText("create file".into())],
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
    let _ = requests.recv_timeout(Duration::from_secs(1))?;
    let next = requests.recv_timeout(Duration::from_secs(1))?;
    let messages = next["messages"].as_array().ok_or("messages missing")?;
    assert!(messages.iter().any(|m| m["role"] == "user"
        && m["content"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|i| i["type"] == "tool_result" && i["tool_use_id"] == "call-write")
        })));
    Ok(())
}
