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
        responses::{ResponsesCompatibility, ResponsesModel},
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
        max_output_tokens: None,
        history: vec![HistoryItem::UserText("hi".into())],
        tools: vec![],
    }
}
fn event(kind: &str, fields: Value) -> Value {
    let mut value = fields;
    value["type"] = json!(kind);
    value
}
fn done(output: Vec<Value>) -> Value {
    event(
        "response.completed",
        json!({"response":{"status":"completed","output":output,"usage":{"input_tokens":12,"output_tokens":4}}}),
    )
}
fn message(text: &str) -> Value {
    json!({"type":"message","id":"msg_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]})
}
fn call_item(id: &str, name: &str, args: &str) -> Value {
    json!({"type":"function_call","id":format!("fc_{id}"),"status":"completed","call_id":id,"name":name,"arguments":args})
}
async fn collect(
    model: &dyn Model,
    req: &ModelRequest,
) -> Result<
    (Vec<HistoryItem>, Option<saya_core::contract::Usage>, String),
    Box<dyn std::error::Error>,
> {
    let mut stream = model.start(req, &Never).await?;
    let mut text = String::new();
    loop {
        match stream.next(&Never).await? {
            Some(ModelUpdate::TextDelta(s)) => text.push_str(&s),
            Some(ModelUpdate::Progress) => {}
            Some(ModelUpdate::Finished { items, usage }) => return Ok((items, usage, text)),
            None => return Err("missing completion".into()),
        }
    }
}
#[tokio::test]
async fn preserves_reasoning_and_tool_items_in_original_order_on_replay()
-> Result<(), Box<dyn std::error::Error>> {
    let reasoning =
        json!({"id":"rs_1","type":"reasoning","summary":[],"encrypted_content":"opaque-fixture"});
    let call = call_item("call_1", "read_file", "{\"path\":\"a.txt\"}");
    let first = fixture::sse(&[
        event("response.created", json!({})),
        event(
            "response.output_item.done",
            json!({"output_index":0,"item":reasoning}),
        ),
        event(
            "response.function_call_arguments.delta",
            json!({"output_index":1,"item_id":"fc_call_1","delta":"{\"path\":"}),
        ),
        event(
            "response.function_call_arguments.delta",
            json!({"output_index":1,"item_id":"fc_call_1","delta":"\"a.txt\"}"}),
        ),
        event(
            "response.function_call_arguments.done",
            json!({"output_index":1,"item_id":"fc_call_1","arguments":"{\"path\":\"a.txt\"}"}),
        ),
        event(
            "response.output_item.done",
            json!({"output_index":1,"item":call}),
        ),
        done(vec![reasoning.clone(), call.clone()]),
    ]);
    let msg = message("done");
    let second = fixture::sse(&[
        event(
            "response.output_text.delta",
            json!({"output_index":0,"content_index":0,"delta":"done"}),
        ),
        event(
            "response.output_item.done",
            json!({"output_index":0,"item":msg}),
        ),
        done(vec![msg]),
    ]);
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", first),
        ("text/event-stream", second),
    ])?;
    let model = ResponsesModel::new(
        transport(url)?,
        settings(true),
        ResponsesCompatibility::default(),
    );
    let mut req = request();
    req.tools.push(ToolDefinition {name:"read_file".into(),description:"read".into(),input_schema:json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false})});
    let (items, usage, _) = collect(&model, &req).await?;
    assert_eq!(usage.ok_or("missing usage")?.input_tokens, Some(12));
    assert!(
        matches!(&items[..],[HistoryItem::ProviderItem {value:a,..},HistoryItem::ProviderItem {value:b,..},HistoryItem::ToolCall(c)] if *a==reasoning && *b==call && c.call_id=="call_1")
    );
    req.history.extend(items);
    req.history.push(HistoryItem::ToolResult {
        call_id: "call_1".into(),
        result: ToolResult {
            ok: true,
            data: Some(json!({"text":"hello"})),
            error: None,
            truncated: false,
        },
    });
    assert_eq!(collect(&model, &req).await?.2, "done");
    let first = requests.recv_timeout(Duration::from_secs(1))?;
    assert_eq!(first["store"], false);
    assert!(first.get("previous_response_id").is_none());
    assert_eq!(first["include"][0], "reasoning.encrypted_content");
    assert_eq!(first["tools"][0]["type"], "function");
    let second = requests.recv_timeout(Duration::from_secs(1))?;
    let input = second["input"].as_array().ok_or("missing input")?;
    assert_eq!(input[1], reasoning);
    assert_eq!(input[2], call);
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_1");
    assert!(second.get("previous_response_id").is_none());
    Ok(())
}
#[tokio::test]
async fn stream_text_delta_and_complete_json_preserve_raw_message()
-> Result<(), Box<dyn std::error::Error>> {
    let msg = message("hello");
    let stream = fixture::sse(&[
        event(
            "response.output_text.delta",
            json!({"output_index":0,"content_index":0,"delta":"hel"}),
        ),
        event(
            "response.output_text.delta",
            json!({"output_index":0,"content_index":0,"delta":"lo"}),
        ),
        event(
            "response.output_item.done",
            json!({"output_index":0,"item":msg}),
        ),
        done(vec![msg.clone()]),
    ]);
    let full =
        json!({"status":"completed","output":[msg],"usage":{"input_tokens":5,"output_tokens":2}})
            .to_string();
    let (url, _) = fixture::server(vec![
        ("text/event-stream", stream),
        ("application/json", full),
    ])?;
    let client = transport(url)?;
    let streaming = ResponsesModel::new(
        client.clone(),
        settings(true),
        ResponsesCompatibility::default(),
    );
    let (items, _, text) = collect(&streaming, &request()).await?;
    assert_eq!(text, "hello");
    assert!(
        matches!(&items[..],[HistoryItem::ProviderItem {protocol:ModelProtocol::Responses,..},HistoryItem::AssistantText(text)] if text=="hello")
    );
    let complete = ResponsesModel::new(client, settings(false), ResponsesCompatibility::default());
    let (items, usage, _) = collect(&complete, &request()).await?;
    assert_eq!(usage.ok_or("missing usage")?.output_tokens, Some(2));
    assert!(
        matches!(&items[..],[HistoryItem::ProviderItem {..},HistoryItem::AssistantText(text)] if text=="hello")
    );
    Ok(())
}
#[tokio::test]
async fn rejects_incomplete_conflicting_and_untrusted_output()
-> Result<(), Box<dyn std::error::Error>> {
    let tool = call_item("x", "read_file", "{\"path\":\"a\"}");
    let scenarios = [
        (
            "data: {\"type\":\"response.created\"}\n\n".to_owned(),
            "incomplete_model_response",
        ),
        (
            fixture::sse(&[event(
                "response.incomplete",
                json!({"response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}),
            )]),
            "output_limit",
        ),
        (
            fixture::sse(&[event("response.refusal.delta", json!({"delta":"no"}))]),
            "provider_refusal",
        ),
        (
            fixture::sse(&[
                event(
                    "response.function_call_arguments.delta",
                    json!({"output_index":0,"delta":"{}"}),
                ),
                done(vec![tool.clone()]),
            ]),
            "invalid_model_response",
        ),
        (
            fixture::sse(&[
                event(
                    "response.function_call_arguments.done",
                    json!({"output_index":0,"item_id":"fc_x","arguments":"{}"}),
                ),
                done(vec![tool.clone()]),
            ]),
            "invalid_model_response",
        ),
        (
            fixture::sse(&[
                event(
                    "response.output_item.done",
                    json!({"output_index":0,"item":tool.clone()}),
                ),
                done(vec![call_item("different", "read_file", "{}")]),
            ]),
            "invalid_model_response",
        ),
        (
            fixture::sse(&[done(vec![json!({"type":"web_search_call","id":"unsafe"})])]),
            "invalid_model_response",
        ),
        (
            fixture::sse(&[done(vec![call_item("x", "read_file", "{")])]),
            "invalid_model_response",
        ),
    ];
    for (body, expected) in scenarios {
        let (url, _) = fixture::server(vec![("text/event-stream", body)])?;
        let model = ResponsesModel::new(
            transport(url)?,
            settings(true),
            ResponsesCompatibility::default(),
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
async fn does_not_replay_mismatched_opaque_items() -> Result<(), Box<dyn std::error::Error>> {
    let mut req = request();
    req.history.push(HistoryItem::ProviderItem {
        protocol: ModelProtocol::Responses,
        value: call_item("real", "read_file", "{}"),
    });
    req.history
        .push(HistoryItem::ToolCall(saya_core::contract::ToolCall {
            call_id: "forged".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        }));
    let model = ResponsesModel::new(
        transport("http://127.0.0.1:1/v1".into())?,
        settings(true),
        ResponsesCompatibility::default(),
    );
    let error = match model.start(&req, &Never).await {
        Ok(_) => return Err("mismatched item accepted".into()),
        Err(e) => e,
    };
    assert_eq!(error.0.code, "invalid_history");
    Ok(())
}
#[tokio::test]
async fn responses_adapter_drives_runtime_and_replays_tool_result()
-> Result<(), Box<dyn std::error::Error>> {
    let raw = call_item(
        "call-write",
        "write_file",
        "{\"path\":\"out.txt\",\"content\":\"ok\",\"expected_version\":null}",
    );
    let first = fixture::sse(&[
        event(
            "response.output_item.done",
            json!({"output_index":0,"item":raw}),
        ),
        done(vec![raw.clone()]),
    ]);
    let msg = message("done");
    let second = fixture::sse(&[
        event(
            "response.output_text.delta",
            json!({"output_index":0,"content_index":0,"delta":"done"}),
        ),
        done(vec![msg]),
    ]);
    let (url, requests) = fixture::server(vec![
        ("text/event-stream", first),
        ("text/event-stream", second),
    ])?;
    let model = Arc::new(ResponsesModel::new(
        transport(url)?,
        settings(true),
        ResponsesCompatibility::default(),
    ));
    let temp = support::temp_workspace()?;
    let mut session = Session {
        id: SessionId::new(),
        workspace: temp.path().into(),
        work_mode: WorkMode::Work,
        profile: "test".into(),
        protocol: ModelProtocol::Responses,
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
    let _ = requests.recv_timeout(Duration::from_secs(1))?;
    let next = requests.recv_timeout(Duration::from_secs(1))?;
    let input = next["input"].as_array().ok_or("missing input")?;
    assert!(input.contains(&raw));
    assert!(
        input
            .iter()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == "call-write")
    );
    Ok(())
}

#[tokio::test]
async fn completed_event_followed_by_eof_is_a_valid_stream_end()
-> Result<(), Box<dyn std::error::Error>> {
    let event = done(vec![message("ok")]);
    let body = format!("event: response.completed\ndata: {event}\n\n");
    let (url, _) = fixture::server(vec![("text/event-stream", body)])?;
    let model = ResponsesModel::new(
        transport(url)?,
        settings(true),
        ResponsesCompatibility::default(),
    );
    let (items, _, _) = collect(&model, &request()).await?;
    assert!(
        items
            .iter()
            .any(|item| matches!(item,HistoryItem::AssistantText(text) if text=="ok"))
    );
    Ok(())
}
