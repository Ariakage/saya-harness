//! Explicit, opt-in provider smoke test. The API key is read from stdin and
//! response content is never printed. No local tools are executed.
use saya_core::{
    boundary::{Cancellation, Model, ModelRequest, ModelUpdate, ToolDefinition},
    contract::{HistoryItem, ModelPurpose, ToolResult},
    provider::{
        ModelSettings,
        anthropic::{AnthropicModel, AnthropicVersion},
        chat::{ChatCompatibility, ChatModel},
        responses::{ResponsesCompatibility, ResponsesModel},
        transport::{Authentication, HttpConfig, HttpTransport},
    },
};
use serde_json::json;
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
async fn turn(model: &dyn Model, request: &ModelRequest) -> Result<Vec<HistoryItem>, String> {
    let mut stream = model.start(request, &Never).await.map_err(|e| e.0.code)?;
    loop {
        match stream.next(&Never).await.map_err(|e| e.0.code)? {
            Some(ModelUpdate::Finished { items, .. }) => return Ok(items),
            Some(_) => {}
            None => return Err("incomplete_model_response".into()),
        }
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err(
            "usage: protocol_probe chat|responses|anthropic API_ROOT MODEL (API key via stdin)"
                .into(),
        );
    }
    let mut secret = String::new();
    std::io::stdin().read_line(&mut secret)?;
    let secret = secret.trim_end_matches(['\r', '\n']).to_owned();
    if secret.is_empty() {
        return Err("API key is empty".into());
    }
    let transport = Arc::new(HttpTransport::new(HttpConfig {
        base_url: args[2].clone(),
        allow_remote_http: false,
        authentication: Authentication::Bearer(secret),
        headers_env: BTreeMap::new(),
        connect_timeout: Duration::from_secs(10),
        idle_timeout: Duration::from_secs(60),
        total_timeout: Duration::from_secs(90),
    })?);
    let settings = ModelSettings {
        profile: "probe".into(),
        model: args[3].clone(),
        context_window_tokens: 128_000,
        max_output_tokens: 128,
        strict_tools: false,
        stream: true,
    };
    let model: Box<dyn Model> = match args[1].as_str() {
        "chat" => Box::new(ChatModel::new(
            transport,
            settings,
            ChatCompatibility {
                legacy_max_tokens: true,
                omit_stream_usage: false,
            },
        )),
        "responses" => Box::new(ResponsesModel::new(
            transport,
            settings,
            ResponsesCompatibility::default(),
        )),
        "anthropic" => Box::new(AnthropicModel::new(
            transport,
            settings,
            AnthropicVersion::default(),
        )),
        _ => return Err("unsupported protocol".into()),
    };
    let mut request = ModelRequest {
        request_id: "probe".into(),
        purpose: ModelPurpose::Execution,
        profile: "probe".into(),
        instructions: "Follow the user request. Use the named tool when explicitly asked.".into(),
        max_output_tokens: Some(128),
        history: vec![HistoryItem::UserText("Reply with OK.".into())],
        tools: vec![],
    };
    let direct = turn(model.as_ref(), &request).await;
    match direct {
        Ok(items)
            if items
                .iter()
                .any(|i| matches!(i,HistoryItem::AssistantText(s) if !s.is_empty())) =>
        {
            println!("direct: passed")
        }
        Ok(_) => {
            println!("direct: invalid response");
            return Err("probe failed".into());
        }
        Err(code) => {
            println!("direct: {code}");
            return Err("probe failed".into());
        }
    }
    request.history = vec![HistoryItem::UserText(
        "Call probe_echo with text set to hello, then wait for the result.".into(),
    )];
    request.tools = vec![ToolDefinition {
        name: "probe_echo".into(),
        description: "Return the supplied text.".into(),
        input_schema: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}),
    }];
    let items = match turn(model.as_ref(), &request).await {
        Ok(items) => items,
        Err(code) => {
            println!("tool_call: {code}");
            return Err("probe failed".into());
        }
    };
    let calls: Vec<_> = items
        .iter()
        .filter_map(|item| {
            if let HistoryItem::ToolCall(call) = item {
                Some(call.clone())
            } else {
                None
            }
        })
        .collect();
    if calls.len() != 1 || calls[0].name != "probe_echo" {
        println!("tool_call: no matching call");
        return Err("probe failed".into());
    }
    println!("tool_call: passed");
    request.history.extend(items);
    request.history.push(HistoryItem::ToolResult {
        call_id: calls[0].call_id.clone(),
        result: ToolResult {
            ok: true,
            data: Some(json!({"text":"hello"})),
            error: None,
            truncated: false,
        },
    });
    match turn(model.as_ref(), &request).await {
        Ok(items)
            if items
                .iter()
                .any(|i| matches!(i,HistoryItem::AssistantText(s) if !s.is_empty())) =>
        {
            println!("tool_result: passed")
        }
        Ok(_) => {
            println!("tool_result: invalid response");
            return Err("probe failed".into());
        }
        Err(code) => {
            println!("tool_result: {code}");
            return Err("probe failed".into());
        }
    }
    Ok(())
}
