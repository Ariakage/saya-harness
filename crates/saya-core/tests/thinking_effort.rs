//! Capture the real HTTP boundary for explicit effort and profile isolation.
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
        router::ProfileRouter,
        transport::{Authentication, HttpConfig, HttpTransport},
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
fn assert_effort(body: &Value, protocol: ModelProtocol, effort: Option<ThinkingEffort>) {
    let encoded = effort.map(|effort| json!(effort));
    match protocol {
        ModelProtocol::ChatCompletions => {
            assert_eq!(body.get("reasoning_effort"), encoded.as_ref());
            assert!(body.get("reasoning").is_none() && body.get("output_config").is_none());
        }
        ModelProtocol::Responses => {
            assert_eq!(
                body.get("reasoning"),
                effort.map(|effort| json!({"effort":effort})).as_ref()
            );
            assert!(body.get("reasoning_effort").is_none() && body.get("output_config").is_none());
        }
        ModelProtocol::AnthropicMessages => {
            assert_eq!(
                body.get("output_config"),
                effort.map(|effort| json!({"effort":effort})).as_ref()
            );
            assert!(body.get("reasoning_effort").is_none() && body.get("reasoning").is_none());
        }
    }
    assert!(body.get("thinking").is_none());
}

#[tokio::test]
async fn protocol_bodies_omit_unconfigured_effort_and_preserve_every_explicit_level()
-> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        for effort in [
            None,
            Some(ThinkingEffort::Low),
            Some(ThinkingEffort::Medium),
            Some(ThinkingEffort::High),
            Some(ThinkingEffort::XHigh),
            Some(ThinkingEffort::Max),
        ] {
            let (url, captured) = fixture::server(vec![("application/json", response(protocol))])?;
            let model = adapter(protocol, url, "execution", effort)?;
            let request = request("execution", ModelPurpose::Execution);
            let mut stream = model.start(&request, &Never).await?;
            assert!(stream.next(&Never).await?.is_some());
            let body = captured.recv_timeout(Duration::from_secs(1))?;
            assert_effort(&body, protocol, effort);
        }
    }
    Ok(())
}

#[tokio::test]
async fn review_effort_comes_only_from_the_selected_review_profile()
-> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        for review_effort in [None, Some(ThinkingEffort::Low)] {
            let (execution_url, execution_bodies) =
                fixture::server(vec![("application/json", response(protocol))])?;
            let (review_url, review_bodies) =
                fixture::server(vec![("application/json", response(protocol))])?;
            let execution = adapter(
                protocol,
                execution_url,
                "execution",
                Some(ThinkingEffort::Max),
            )?;
            let review = adapter(protocol, review_url, "review", review_effort)?;
            let router = ProfileRouter::new(
                "execution".into(),
                BTreeMap::from([("execution".into(), execution), ("review".into(), review)]),
            )?;
            for (profile, purpose) in [
                ("execution", ModelPurpose::Execution),
                ("review", ModelPurpose::Review),
            ] {
                let request = request(profile, purpose);
                let mut stream = router.start(&request, &Never).await?;
                assert!(stream.next(&Never).await?.is_some());
            }
            assert_effort(
                &execution_bodies.recv_timeout(Duration::from_secs(1))?,
                protocol,
                Some(ThinkingEffort::Max),
            );
            assert_effort(
                &review_bodies.recv_timeout(Duration::from_secs(1))?,
                protocol,
                review_effort,
            );
        }
    }
    Ok(())
}
