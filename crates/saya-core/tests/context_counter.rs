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
    configured_adapter(
        protocol,
        url,
        ModelSettings {
            profile: profile.into(),
            model: "fixture-model".into(),
            context_window_tokens: 128_000,
            extra_safety_tokens: 0,
            max_output_tokens: 8192,
            thinking_effort: effort,
            strict_tools: false,
            stream: false,
        },
        ChatCompatibility::default(),
    )
}
fn configured_adapter(
    protocol: ModelProtocol,
    url: String,
    settings: ModelSettings,
    chat_compatibility: ChatCompatibility,
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
    Ok(match protocol {
        ModelProtocol::ChatCompletions => {
            Arc::new(ChatModel::new(transport, settings, chat_compatibility))
        }
        ModelProtocol::Responses => Arc::new(ResponsesModel::new(
            transport,
            settings,
            ResponsesCompatibility::default(),
        )),
        ModelProtocol::AnthropicMessages => Arc::new(
            AnthropicModel::new(transport, settings, AnthropicVersion::default())
                .with_prompt_cache(true),
        ),
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
        for purpose in [ModelPurpose::Execution, ModelPurpose::Compaction] {
            let (url, received) = fixture::server(vec![("application/json", response(protocol))])?;
            let model = adapter(protocol, url, "fixture", Some(ThinkingEffort::High))?;
            let mut request = request("fixture", purpose);
            request.instructions = "Static rules. 静态指令。".into();
            if purpose == ModelPurpose::Execution {
                request.dynamic_instructions = Some("Current host data. 动态上下文。".into());
                request.tools.push(ToolDefinition {name:"read_file".into(),description:"Bounded source evidence".into(),input_schema:json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false})});
            }
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
            let received_body = received.recv_timeout(Duration::from_secs(1))?;
            if protocol == ModelProtocol::AnthropicMessages {
                assert_eq!(
                    received_body["system"][0]["cache_control"]["type"],
                    "ephemeral"
                );
                assert_eq!(received_body["system"][0]["text"], request.instructions);
            }
            if purpose == ModelPurpose::Compaction {
                assert!(request.tools.is_empty());
            }
            let body = received_body.to_string();
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
    }
    Ok(())
}

#[tokio::test]
async fn required_input_near_provider_guard_is_rejected_before_network_dispatch()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::context_builder::{
        self, BuildInput, ContextBudget, ContextDecision, CountingIdentity,
    };
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        let (url, received) = fixture::server(vec![("application/json", response(protocol))])?;
        let model = adapter(protocol, url, "fixture", None)?;
        let mut request = request("fixture", ModelPurpose::Execution);
        request.history = vec![HistoryItem::UserText("x".repeat(500_000))];
        let identity = CountingIdentity {
            protocol,
            model: "fixture-model".into(),
            adapter_version: saya_core::provider::COUNTING_ADAPTER_VERSION.into(),
        };
        let built = context_builder::build(
            BuildInput {
                request: &request,
                identity: &identity,
                budget: ContextBudget::new(128_000, 8192, 12_801),
                source_history: &request.history,
                history_offset: 0,
                revision: 1,
                committed_view: None,
                protected_ranges: &[],
                output_projections: &[],
                group_retirements: &[],
            },
            model.request_counter(),
        )?;
        assert!(matches!(
            built.decision,
            ContextDecision::ContextLimit { .. }
        ));
        match model.start(&request, &Never).await {
            Err(error) => assert_eq!(error.0.code, "context_budget"),
            Ok(_) => panic!("oversized request reached provider transport"),
        }
        assert!(received.try_recv().is_err());
    }
    Ok(())
}

#[tokio::test]
async fn dynamic_output_limits_match_counted_wire_and_keep_all_input()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::{
        boundary::ToolDefinition,
        context_builder::{self, CountingIdentity},
    };
    for (protocol, legacy) in [
        (ModelProtocol::ChatCompletions, false),
        (ModelProtocol::ChatCompletions, true),
        (ModelProtocol::Responses, false),
        (ModelProtocol::AnthropicMessages, false),
    ] {
        for (window, default_output, override_output) in [
            (2048, 32_000, None),
            (128_000, 32_000, None),
            (128_000, 32_000, Some(16_384)),
            (128_000, 1024, Some(16_000)),
            (2048, 32_000, Some(512)),
            (2048, 32_000, Some(32_000)),
        ] {
            let (url, received) = fixture::server(vec![("application/json", response(protocol))])?;
            let model = configured_adapter(
                protocol,
                url,
                ModelSettings {
                    profile: "fixture".into(),
                    model: "fixture-model".into(),
                    context_window_tokens: window,
                    extra_safety_tokens: 0,
                    max_output_tokens: default_output,
                    thinking_effort: None,
                    strict_tools: false,
                    stream: false,
                },
                ChatCompatibility {
                    legacy_max_tokens: legacy,
                    ..ChatCompatibility::default()
                },
            )?;
            let mut request = request("fixture", ModelPurpose::Execution);
            request.instructions = "Keep the complete input. 保留完整输入。".into();
            request.dynamic_instructions = Some("Current host context. 宿主信息。".into());
            request.max_output_tokens = override_output;
            request.tools.push(ToolDefinition {
                name: "read_file".into(),
                description: "Complete tool schema remains available".into(),
                input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
            });
            let identity = CountingIdentity {
                protocol,
                model: "fixture-model".into(),
                adapter_version: saya_core::provider::COUNTING_ADAPTER_VERSION.into(),
            };
            let fingerprint = context_builder::request_fingerprint(&request, &identity)?;
            let evidence =
                model
                    .request_counter()
                    .unwrap()
                    .count(&request, &identity, &fingerprint)?;
            assert!(received.try_recv().is_err());
            let mut stream = model.start(&request, &Never).await?;
            assert!(stream.next(&Never).await?.is_some());
            let body = received.recv_timeout(Duration::from_secs(1))?;
            let field = match protocol {
                ModelProtocol::ChatCompletions if !legacy => "max_completion_tokens",
                ModelProtocol::Responses => "max_output_tokens",
                _ => "max_tokens",
            };
            let output = body[field].as_u64().ok_or("missing output limit")?;
            let desired = override_output.unwrap_or(default_output);
            assert!(output > 0 && output <= u64::from(desired));
            if window == 2048 && desired > 2048 {
                assert!(output < u64::from(desired));
            } else {
                assert_eq!(output, u64::from(desired));
            }
            for other_field in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
                if other_field != field {
                    assert!(body.get(other_field).is_none());
                }
            }
            let serialized = body.to_string();
            let ascii = serialized.chars().filter(char::is_ascii).count() as u64;
            let other = serialized.chars().filter(|ch| !ch.is_ascii()).count() as u64;
            assert_eq!(evidence.input_tokens, ascii.div_ceil(4) + other);
            let usable = window - window.div_ceil(10) - 1;
            assert!(evidence.input_tokens + output <= usable);
            assert!(serialized.contains("Keep the complete input."));
            assert!(serialized.contains("宿主信息。"));
            assert!(serialized.contains("hello"));
            let tool = &body["tools"][0];
            let schema = match protocol {
                ModelProtocol::ChatCompletions => &tool["function"]["parameters"],
                ModelProtocol::Responses => &tool["parameters"],
                ModelProtocol::AnthropicMessages => &tool["input_schema"],
            };
            assert_eq!(schema, &request.tools[0].input_schema);
        }
    }
    Ok(())
}

#[tokio::test]
async fn zero_output_is_invalid_and_full_input_still_has_counting_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::context_builder::{self, CountingIdentity};
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        for (window, extra_safety, override_output, text, expected_code) in [
            (2048, 0, Some(0), "hello".into(), "invalid_config"),
            (512, 0, None, "x".repeat(3000), "context_budget"),
            (1, 0, None, "hello".into(), "context_budget"),
            (2048, 2000, None, "hello".into(), "context_budget"),
            (2048, u64::MAX, None, "hello".into(), "context_budget"),
        ] {
            let (url, received) = fixture::server(vec![("application/json", response(protocol))])?;
            let model = configured_adapter(
                protocol,
                url,
                ModelSettings {
                    profile: "fixture".into(),
                    model: "fixture-model".into(),
                    context_window_tokens: window,
                    extra_safety_tokens: extra_safety,
                    max_output_tokens: 32_000,
                    thinking_effort: None,
                    strict_tools: false,
                    stream: false,
                },
                ChatCompatibility::default(),
            )?;
            let mut request = request("fixture", ModelPurpose::Execution);
            request.max_output_tokens = override_output;
            request.history = vec![HistoryItem::UserText(text)];
            let identity = CountingIdentity {
                protocol,
                model: "fixture-model".into(),
                adapter_version: saya_core::provider::COUNTING_ADAPTER_VERSION.into(),
            };
            let fingerprint = context_builder::request_fingerprint(&request, &identity)?;
            let evidence =
                model
                    .request_counter()
                    .unwrap()
                    .count(&request, &identity, &fingerprint);
            if override_output == Some(0) {
                assert_eq!(evidence.unwrap_err().0.code, "invalid_config");
            } else {
                let safety = window
                    .div_ceil(10)
                    .saturating_add(1)
                    .saturating_add(extra_safety);
                assert!(evidence?.input_tokens >= window.saturating_sub(safety));
            }
            let error = match model.start(&request, &Never).await {
                Err(error) => error,
                Ok(_) => return Err("request without output space reached transport".into()),
            };
            assert_eq!(error.0.code, expected_code);
            assert!(received.try_recv().is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn extra_safety_reserve_is_enforced_on_actual_wire_for_every_purpose()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::context_builder::{self, CountingIdentity};
    let window = 2048u64;
    let extra_safety = 700u64;
    for protocol in [
        ModelProtocol::ChatCompletions,
        ModelProtocol::Responses,
        ModelProtocol::AnthropicMessages,
    ] {
        for purpose in [
            ModelPurpose::Execution,
            ModelPurpose::Review,
            ModelPurpose::Compaction,
        ] {
            let (url, received) = fixture::server(vec![("application/json", response(protocol))])?;
            let model = configured_adapter(
                protocol,
                url,
                ModelSettings {
                    profile: "fixture".into(),
                    model: "fixture-model".into(),
                    context_window_tokens: window,
                    extra_safety_tokens: extra_safety,
                    max_output_tokens: 32_000,
                    thinking_effort: None,
                    strict_tools: false,
                    stream: false,
                },
                ChatCompatibility::default(),
            )?;
            let mut request = request("fixture", purpose);
            request.max_output_tokens = Some(64_000);
            request.instructions = "Keep mandatory facts. 保留必要事实。".into();
            let identity = CountingIdentity {
                protocol,
                model: "fixture-model".into(),
                adapter_version: saya_core::provider::COUNTING_ADAPTER_VERSION.into(),
            };
            let fingerprint = context_builder::request_fingerprint(&request, &identity)?;
            let evidence =
                model
                    .request_counter()
                    .unwrap()
                    .count(&request, &identity, &fingerprint)?;
            assert!(received.try_recv().is_err());
            let mut stream = model.start(&request, &Never).await?;
            assert!(stream.next(&Never).await?.is_some());
            let body = received.recv_timeout(Duration::from_secs(1))?;
            let field = match protocol {
                ModelProtocol::ChatCompletions => "max_completion_tokens",
                ModelProtocol::Responses => "max_output_tokens",
                ModelProtocol::AnthropicMessages => "max_tokens",
            };
            let output = body[field].as_u64().ok_or("missing output limit")?;
            assert!(output > 0 && output < 64_000);
            let text = body.to_string();
            let ascii = text.chars().filter(char::is_ascii).count() as u64;
            let other = text.chars().filter(|ch| !ch.is_ascii()).count() as u64;
            assert_eq!(evidence.input_tokens, ascii.div_ceil(4) + other);
            let base_safety = window.div_ceil(10) + 1;
            assert!(evidence.input_tokens + output + base_safety + extra_safety <= window);
            // This cap is limited by the configured extra reserve, even though
            // the provider's base reserve alone would have allowed more output.
            assert!(evidence.input_tokens + output + base_safety > window - extra_safety - 3);
            assert!(text.contains("保留必要事实。"));
            assert!(text.contains("hello"));
        }
    }
    Ok(())
}
