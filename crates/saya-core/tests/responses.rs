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
fn event(kind: &str, fields: Value) -> Value {
    let mut value = fields;
    value["type"] = json!(kind);
    value
}
fn done(output: Vec<Value>) -> Value {
    event(
        "response.completed",
        json!({"response":{"status":"completed","output":output,"usage":{"input_tokens":12,"output_tokens":4,"input_tokens_details":{"cached_tokens":8}}}}),
    )
}
fn message(text: &str) -> Value {
    json!({"type":"message","id":"msg_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]})
}
fn call_item(id: &str, name: &str, args: &str) -> Value {
    json!({"type":"function_call","id":format!("fc_{id}"),"status":"completed","call_id":id,"name":name,"arguments":args})
}
#[tokio::test]
async fn public_reasoning_deltas_exclude_encrypted_native_content()
-> Result<(), Box<dyn std::error::Error>> {
    let reasoning = json!({"type":"reasoning","id":"rs_public","summary":[{"type":"summary_text","text":"summary"}],
        "content":[{"type":"reasoning_text","text":"public"}],"encrypted_content":"opaque-encrypted"});
    let output = message("answer");
    let body = fixture::sse(&[
        event(
            "response.reasoning_summary_text.delta",
            json!({"output_index":0,"summary_index":0,"delta":"summary"}),
        ),
        event(
            "response.reasoning_text.delta",
            json!({"output_index":0,"content_index":0,"delta":"public"}),
        ),
        event(
            "response.reasoning_summary_text.done",
            json!({"output_index":0,"summary_index":0,"item_id":"rs_public","text":"summary"}),
        ),
        event(
            "response.reasoning_text.done",
            json!({"output_index":0,"content_index":0,"item_id":"rs_public","text":"public"}),
        ),
        event(
            "response.output_text.delta",
            json!({"output_index":1,"content_index":0,"delta":"answer"}),
        ),
        event(
            "response.output_item.done",
            json!({"output_index":0,"item":reasoning}),
        ),
        event(
            "response.output_item.done",
            json!({"output_index":1,"item":output}),
        ),
        done(vec![reasoning.clone(), output]),
    ]);
    let (url, requests) = fixture::server(vec![("text/event-stream", body)])?;
    let model = ResponsesModel::new(
        transport(url)?,
        settings(true),
        ResponsesCompatibility::default(),
    );
    let request = request();
    let mut stream = model.start(&request, &Never).await?;
    let mut thinking = Vec::new();
    let mut visible = String::new();
    loop {
        match stream.next(&Never).await? {
            Some(ModelUpdate::ThinkingDelta { text }) => thinking.push(text),
            Some(ModelUpdate::TextDelta(text)) => visible.push_str(&text),
            Some(ModelUpdate::Finished { items, .. }) => {
                assert!(
                    matches!(&items[0], HistoryItem::ProviderItem { value, .. } if *value==reasoning)
                );
                break;
            }
            Some(ModelUpdate::Progress) => {}
            None => return Err("missing completion".into()),
        }
    }
    assert_eq!(thinking, ["summary", "public"]);
    assert_eq!(visible, "answer");
    assert!(
        requests
            .recv_timeout(Duration::from_secs(1))?
            .get("reasoning")
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn enabled_media_is_sent_as_responses_content_parts() -> Result<(), Box<dyn std::error::Error>>
{
    let response = json!({
        "status":"completed",
        "output":[message("ok")],
        "usage":{"input_tokens":9,"output_tokens":1}
    })
    .to_string();
    let (url, requests) = fixture::server(vec![("application/json", response)])?;
    let root = tempfile::tempdir()?;
    let image = root.path().join("image.jpg");
    let audio = root.path().join("audio.mp3");
    let video = root.path().join("video.mov");
    std::fs::write(&image, b"jpg")?;
    std::fs::write(&audio, b"mp3")?;
    std::fs::write(&video, b"mov")?;
    let mut cfg = settings(false);
    cfg.multimodal = saya_core::config::MediaCapabilities {
        image: true,
        audio: true,
        video: true,
    };
    let capabilities = cfg.multimodal;
    let model = ResponsesModel::new(transport(url)?, cfg, ResponsesCompatibility::default());
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
    let parts = body["input"][0]["content"]
        .as_array()
        .ok_or("missing multimodal content")?;
    let image = parts
        .iter()
        .find(|part| part["type"] == "input_image")
        .ok_or("missing image part")?;
    assert!(
        image["image_url"]
            .as_str()
            .is_some_and(|url| url.starts_with("data:image/jpeg;base64,"))
    );
    let audio = parts
        .iter()
        .find(|part| part["type"] == "input_audio")
        .ok_or("missing audio part")?;
    assert_eq!(audio["input_audio"]["format"], "mp3");
    let video = parts
        .iter()
        .find(|part| part["type"] == "input_file")
        .ok_or("missing video part")?;
    assert_eq!(video["filename"], "video.mov");
    assert!(
        video["file_data"]
            .as_str()
            .is_some_and(|url| url.starts_with("data:video/quicktime;base64,"))
    );
    Ok(())
}

#[tokio::test]
async fn reasoning_events_must_match_their_indices_identity_and_final_plaintext()
-> Result<(), Box<dyn std::error::Error>> {
    let reasoning = json!({"type":"reasoning","id":"rs_public",
        "summary":[{"type":"summary_text","text":"final"}],
        "content":[{"type":"reasoning_text","text":"final"}]});
    let cases = [
        vec![event(
            "response.reasoning_summary_text.delta",
            json!({"output_index":0,"summary_index":0,"delta":"conflict"}),
        )],
        vec![event(
            "response.reasoning_text.delta",
            json!({"output_index":0,"content_index":0,"delta":"conflict"}),
        )],
        vec![event(
            "response.reasoning_text.delta",
            json!({"output_index":1,"content_index":0,"delta":"final"}),
        )],
        vec![event(
            "response.reasoning_text.delta",
            json!({"output_index":0,"content_index":256,"delta":"final"}),
        )],
        vec![event(
            "response.reasoning_text.delta",
            json!({"output_index":0,"content_index":0,"item_id":"wrong","delta":"final"}),
        )],
        vec![
            event(
                "response.reasoning_text.delta",
                json!({"output_index":0,"content_index":0,"delta":"final"}),
            ),
            event(
                "response.reasoning_text.done",
                json!({"output_index":0,"content_index":0,"text":"conflict"}),
            ),
        ],
    ];
    for (case, mut events) in cases.into_iter().enumerate() {
        events.push(done(vec![reasoning.clone(), message("answer")]));
        let (url, _) = fixture::server(vec![("text/event-stream", fixture::sse(&events))])?;
        let model = ResponsesModel::new(
            transport(url)?,
            settings(true),
            ResponsesCompatibility::default(),
        );
        assert!(
            collect(&model, &request()).await.is_err(),
            "invalid reasoning case {case} succeeded"
        );
    }
    let body = fixture::sse(&[
        event(
            "response.reasoning_text.delta",
            json!({"output_index":0,"content_index":0,"delta":"discarded"}),
        ),
        done(vec![
            json!({"type":"reasoning","id":"rs_empty","summary":[]}),
            message("answer"),
        ]),
    ]);
    let (url, _) = fixture::server(vec![("text/event-stream", body)])?;
    let model = ResponsesModel::new(
        transport(url)?,
        settings(true),
        ResponsesCompatibility::default(),
    );
    assert!(
        collect(&model, &request()).await.is_err(),
        "reasoning was discarded without opaque replacement"
    );
    Ok(())
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
            Some(ModelUpdate::Progress | ModelUpdate::ThinkingDelta { .. }) => {}
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
            "response.reasoning_text.delta",
            json!({"output_index":0,"content_index":0,"delta":"not-visible"}),
        ),
        event(
            "response.reasoning_text.done",
            json!({"output_index":0,"content_index":0,"text":"not-visible"}),
        ),
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
    let (items, usage, visible) = collect(&model, &req).await?;
    assert!(visible.is_empty());
    let usage = usage.ok_or("missing usage")?;
    assert_eq!(usage.input_tokens, Some(12));
    assert_eq!(usage.cached_input_tokens, Some(8));
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
        json!({"status":"completed","output":[msg],"usage":{"input_tokens":5,"output_tokens":2,"input_tokens_details":{"cached_tokens":3}}})
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
    let usage = usage.ok_or("missing usage")?;
    assert_eq!(usage.output_tokens, Some(2));
    assert_eq!(usage.cached_input_tokens, Some(3));
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
    let first = requests.recv_timeout(Duration::from_secs(1))?;
    let next = requests.recv_timeout(Duration::from_secs(1))?;
    let first_system = first["instructions"].as_str().ok_or("missing system")?;
    let next_system = next["instructions"].as_str().ok_or("missing system")?;
    assert_eq!(first_system, next_system);
    assert!(next_system.contains("\"captured_at\""));
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

#[tokio::test]
async fn queued_streams_accept_empty_plaintext_with_opaque_reasoning_and_completed_text()
-> Result<(), Box<dyn std::error::Error>> {
    let reasoning = json!({"type":"reasoning","id":"rs_opaque","summary":[],"encrypted_content":"opaque-fixture"});
    let body = fixture::sse(&[
        event(
            "response.queued",
            json!({"response":{"status":"queued","output":[]}}),
        ),
        event(
            "response.reasoning_summary_text.delta",
            json!({"output_index":0,"summary_index":0,"item_id":"rs_opaque","delta":"public thought"}),
        ),
        event(
            "response.reasoning_summary_text.done",
            json!({"output_index":0,"summary_index":0,"item_id":"rs_opaque","text":"public thought"}),
        ),
        event(
            "response.output_text.delta",
            json!({"output_index":1,"content_index":0,"item_id":"msg_1","delta":"ok"}),
        ),
        event(
            "response.output_text.done",
            json!({"output_index":1,"content_index":0,"item_id":"msg_1","text":"ok"}),
        ),
        done(vec![reasoning.clone(), message("ok")]),
    ]);
    let (url, _) = fixture::server(vec![("text/event-stream", body)])?;
    let model = ResponsesModel::new(
        transport(url)?,
        settings(true),
        ResponsesCompatibility::default(),
    );
    let (items, _, text) = collect(&model, &request()).await?;
    assert_eq!(text, "ok");
    assert!(matches!(&items[0], HistoryItem::ProviderItem {value,..} if value == &reasoning));
    Ok(())
}

#[tokio::test]
async fn stream_identity_and_reverse_plaintext_coverage_reject_unobserved_final_parts()
-> Result<(), Box<dyn std::error::Error>> {
    let text = event(
        "response.output_text.delta",
        json!({"output_index":0,"content_index":0,"item_id":"msg_1","delta":"ok"}),
    );
    let complete = event(
        "response.output_text.done",
        json!({"output_index":0,"content_index":0,"item_id":"msg_1","text":"ok"}),
    );
    let mut extra_part = message("ok");
    extra_part["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"output_text","text":"hidden","annotations":[]}));
    let mut extra_message = message("hidden");
    extra_message["id"] = json!("msg_extra");
    let mut wrong_identity = text.clone();
    wrong_identity["item_id"] = json!("msg_wrong");
    let mut wrong_done = complete.clone();
    wrong_done["text"] = json!("different");
    let reasoning = json!({"type":"reasoning","id":"rs_extra","summary":[{"type":"summary_text","text":"hidden"}],"encrypted_content":"opaque-fixture"});
    let summary = event(
        "response.reasoning_summary_text.delta",
        json!({"output_index":1,"summary_index":0,"item_id":"rs_extra","delta":"hidden"}),
    );
    let mut extra_summary = reasoning.clone();
    extra_summary["summary"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"summary_text","text":"another hidden part"}));
    for events in [
        vec![wrong_identity, done(vec![message("ok")])],
        vec![text.clone(), done(vec![extra_part])],
        vec![text.clone(), done(vec![message("ok"), extra_message])],
        vec![text.clone(), done(vec![message("ok"), reasoning])],
        vec![
            text.clone(),
            summary,
            done(vec![message("ok"), extra_summary]),
        ],
        vec![text.clone(), wrong_done, done(vec![message("ok")])],
        vec![complete.clone(), text, done(vec![message("ok")])],
        vec![complete.clone(), complete, done(vec![message("ok")])],
    ] {
        let (url, _) = fixture::server(vec![("text/event-stream", fixture::sse(&events))])?;
        let model = ResponsesModel::new(
            transport(url)?,
            settings(true),
            ResponsesCompatibility::default(),
        );
        assert!(
            collect(&model, &request()).await.is_err(),
            "inconsistent stream accepted: {events:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn completed_text_and_item_events_are_valid_without_text_deltas()
-> Result<(), Box<dyn std::error::Error>> {
    for first in [
        event(
            "response.output_text.done",
            json!({"output_index":0,"content_index":0,"item_id":"msg_1","text":"ok"}),
        ),
        event(
            "response.output_item.done",
            json!({"output_index":0,"item":message("ok")}),
        ),
    ] {
        let (url, _) = fixture::server(vec![(
            "text/event-stream",
            fixture::sse(&[first, done(vec![message("ok")])]),
        )])?;
        let model = ResponsesModel::new(
            transport(url)?,
            settings(true),
            ResponsesCompatibility::default(),
        );
        let (items, _, text) = collect(&model, &request()).await?;
        assert!(text.is_empty());
        assert!(
            items
                .iter()
                .any(|item| matches!(item, HistoryItem::AssistantText(text) if text == "ok"))
        );
    }
    Ok(())
}

#[tokio::test]
async fn multiple_function_calls_preserve_order_and_each_call_id()
-> Result<(), Box<dyn std::error::Error>> {
    let first_call = call_item("first", "read_file", "{\"path\":\"a\"}");
    let second_call = call_item("second", "list_dir", "{\"path\":\".\"}");
    let commentary = message("I will inspect both paths.");
    let first = fixture::sse(&[
        event(
            "response.output_item.done",
            json!({"output_index":0,"item":first_call}),
        ),
        event(
            "response.output_item.done",
            json!({"output_index":1,"item":commentary}),
        ),
        event(
            "response.output_item.done",
            json!({"output_index":2,"item":second_call}),
        ),
        done(vec![first_call, commentary.clone(), second_call]),
    ]);
    let second = fixture::sse(&[done(vec![message("done")])]);
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
    let (items, _, _) = collect(&model, &req).await?;
    let ids: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let HistoryItem::ToolCall(call) = item {
                Some(call.call_id.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ids, ["first", "second"]);
    req.history.extend(items);
    for id in ids {
        req.history.push(HistoryItem::ToolResult {
            call_id: id,
            result: ToolResult {
                ok: true,
                data: None,
                error: None,
                truncated: false,
            },
        });
    }
    collect(&model, &req).await?;
    let _ = requests.recv_timeout(Duration::from_secs(1))?;
    let replay = requests.recv_timeout(Duration::from_secs(1))?;
    assert_eq!(replay["input"][2], commentary);
    let output_ids: Vec<_> = replay["input"]
        .as_array()
        .ok_or("missing input")?
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .filter_map(|item| item["call_id"].as_str())
        .collect();
    assert_eq!(output_ids, ["first", "second"]);
    Ok(())
}
