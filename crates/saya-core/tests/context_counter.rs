//! Count the exact wire body locally before starting any provider request.
#[path = "fixtures/http_support/mod.rs"]
mod fixture;
use saya_core::{
    boundary::{Cancellation, Model, ModelRequest},
    config::ThinkingEffort,
    contract::{HistoryItem, ModelProtocol, ModelPurpose},
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
fn request(profile: &str, purpose: ModelPurpose) -> ModelRequest {
    ModelRequest {
        request_id: "effort-test".into(),
        purpose,
        profile: profile.into(),
        instructions: "fixture".into(),
        dynamic_instructions: None,
        max_output_tokens: None,
        history: vec![HistoryItem::UserText("hello".into())],
        tools: vec![],
    }
}
fn response(protocol: ModelProtocol) -> String {
    match protocol {
        ModelProtocol::ChatCompletions => json!({
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "ok"}
            }]
        }),
        ModelProtocol::Responses => json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "ok"}]
            }]
        }),
        ModelProtocol::AnthropicMessages => json!({
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "ok"}]
        }),
    }
    .to_string()
}
fn adapter(
    protocol: ModelProtocol,
    url: String,
    profile: &str,
    effort: Option<ThinkingEffort>,
) -> Result<Arc<dyn Model>, Box<dyn std::error::Error>> {
    let transport = Arc::new(HttpTransport::new(HttpConfig {
        base_url: url,
        allow_remote_http: false,
        authentication: Authentication::None,
        headers_env: BTreeMap::new(),
        connect_timeout: Duration::from_secs(1),
        idle_timeout: Duration::from_secs(2),
        total_timeout: Duration::from_secs(5),
    })?);
    let settings = ModelSettings {
        profile: profile.into(),
        model: "fixture-model".into(),
        context_window_tokens: 128_000,
        max_output_tokens: 8192,
        thinking_effort: effort,
        strict_tools: false,
        stream: false,
    };
    Ok(match protocol {
        ModelProtocol::ChatCompletions => Arc::new(ChatModel::new(
            transport,
            settings,
            ChatCompatibility::default(),
        )),
        ModelProtocol::Responses => Arc::new(ResponsesModel::new(
            transport,
            settings,
            ResponsesCompatibility::default(),
        )),
        ModelProtocol::AnthropicMessages => Arc::new(AnthropicModel::new(
            transport,
            settings,
            AnthropicVersion::default(),
        )),
    })
}

#[tokio::test]
async fn local_counter_matches_actual_wire_body_for_all_three_protocols()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        boundary::ToolDefinition,
        context_builder::{self, CountingIdentity},
    };
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let (url, received) = fixture::server(vec![("application/json", response(protocol))])?;
        let model = adapter(protocol, url, "fixture", Some(ThinkingEffort::High))?;
        let mut request = request("fixture", ModelPurpose::Execution);
        request.instructions = "Static rules. 静态指令。".into();
        request.dynamic_instructions = Some("Current host data. 动态上下文。".into());
        request.tools.push(ToolDefinition {name:"read_file".into(),description:"Bounded source evidence".into(),input_schema:json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false})});
        let identity = CountingIdentity {
            protocol,
            model: "fixture-model".into(),
            adapter_version: saya_core::provider::COUNTING_ADAPTER_VERSION.into(),
        };
        let fingerprint = context_builder::request_fingerprint(&request, &identity)?;
        let evidence = model
            .request_counter()
            .expect("local adapter counter")
            .count(&request, &identity, &fingerprint)?;
        assert!(
            received.try_recv().is_err(),
            "counting must not perform HTTP"
        );
        let mut stream = model.start(&request, &Never).await?;
        assert!(stream.next(&Never).await?.is_some());
        let body = received.recv_timeout(Duration::from_secs(1))?.to_string();
        let ascii = body.chars().filter(char::is_ascii).count() as u64;
        let other = body
            .chars()
            .filter(|character| !character.is_ascii())
            .count() as u64;
        assert_eq!(evidence.input_tokens, ascii.div_ceil(4) + other);
        assert_eq!(evidence.request_fingerprint, fingerprint);
        let wrong = CountingIdentity {
            model: "wrong-model".into(),
            ..identity
        };
        assert!(
            model
                .request_counter()
                .unwrap()
                .count(&request, &wrong, &fingerprint)
                .is_err()
        );
    }
    Ok(())
}
