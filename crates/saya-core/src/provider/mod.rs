//! HTTP transport and provider adapters for the supported model protocols.

pub mod anthropic;
pub mod chat;
pub mod responses;
pub mod router;
pub mod transport;

use crate::{
    boundary::ModelRequest,
    config::{MAX_OUTPUT_TOKENS, MediaCapabilities, ThinkingEffort},
    contract::{BoundaryError, ErrorInfo, ModelPurpose},
};
use serde_json::{Map, Value};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

/// Explicit, provider-independent profile settings. A new session is required
/// to change the execution model or protocol; review profiles can differ.
#[derive(Debug, Clone)]
pub struct ModelSettings {
    pub profile: String,
    pub model: String,
    pub context_window_tokens: u64,
    /// Additional host-configured context reserve for every request purpose.
    pub extra_safety_tokens: u64,
    pub max_output_tokens: u32,
    pub multimodal: MediaCapabilities,
    pub upstream_max_output_tokens: Arc<AtomicU32>,
    pub thinking_effort: Option<ThinkingEffort>,
    pub strict_tools: bool,
    pub stream: bool,
}
impl ModelSettings {
    fn validate(&self, request: &ModelRequest) -> Result<u32, BoundaryError> {
        if request.purpose != ModelPurpose::Execution && !request.tools.is_empty() {
            return Err(failure(
                "invalid_config",
                "review and compaction requests cannot contain tools",
            ));
        }
        if self.profile.is_empty()
            || self.model.is_empty()
            || self.context_window_tokens == 0
            || self.max_output_tokens == 0
            || self.max_output_tokens > MAX_OUTPUT_TOKENS
            || request.profile != self.profile
        {
            return Err(failure(
                "invalid_config",
                "model profile is invalid or does not match the request",
            ));
        }
        let requested = request.max_output_tokens.unwrap_or(self.max_output_tokens);
        if requested == 0 || requested > MAX_OUTPUT_TOKENS {
            return Err(failure(
                "invalid_config",
                "output token limit must be positive and at most one million",
            ));
        }
        let observed = self
            .upstream_max_output_tokens
            .load(Ordering::Acquire)
            .min(MAX_OUTPUT_TOKENS);
        let output = if observed == 0 {
            requested
        } else {
            requested.min(observed)
        };
        Ok(output)
    }

    pub(crate) fn observe_output_limit(&self, value: &Value) {
        for key in ["max_output_limit", "output_limit"] {
            if let Some(limit) = value.get(key).and_then(Value::as_u64)
                && let Ok(limit) = u32::try_from(limit)
                && limit > 0
            {
                let limit = limit.min(MAX_OUTPUT_TOKENS);
                let observed = self.upstream_max_output_tokens.load(Ordering::Acquire);
                if observed == 0 || limit < observed {
                    self.upstream_max_output_tokens
                        .store(limit, Ordering::Release);
                }
            }
        }
        for key in ["response", "model", "capabilities", "limits"] {
            if let Some(value) = value.get(key) {
                self.observe_output_limit(value);
            }
        }
    }

    /// Fit the requested output ceiling after the complete wire input is known.
    /// Counting also uses this projection, but can count an input that needs
    /// compaction even when no output token can fit yet.
    fn fit_output(
        &self,
        request_body: &mut Value,
        output_field: &str,
        output: u32,
        check_budget: bool,
    ) -> Result<(), BoundaryError> {
        let input = wire_input_tokens(request_body)?;
        let safety = self
            .context_window_tokens
            .div_ceil(10)
            .saturating_add(1)
            .saturating_add(self.extra_safety_tokens);
        let usable = self.context_window_tokens.saturating_sub(safety);
        let remaining = usable.saturating_sub(input);
        let adjusted = u64::from(output).min(remaining).max(1) as u32;
        let field = request_body
            .get_mut(output_field)
            .ok_or_else(|| failure("invalid_config", "output token field is missing"))?;
        *field = Value::from(adjusted);
        // Shrinking the number can only shorten its serialized representation.
        // Validate the final body, which is also the body counted and sent.
        if check_budget
            && wire_input_tokens(request_body)?.saturating_add(u64::from(adjusted)) > usable
        {
            return Err(failure(
                "context_budget",
                "estimated input leaves no output token within the context limit",
            ));
        }
        Ok(())
    }
}

/// Convert an object schema to OpenAI strict form when it is possible without
/// changing the meanings of present fields. Schemas with open-ended maps cannot
/// be represented in strict mode and require the caller to disable it explicitly.
fn strict_schema(schema: &Value) -> Result<Value, BoundaryError> {
    fn visit(value: &mut Value) -> Result<(), BoundaryError> {
        match value {
            Value::Object(map) => {
                let object_type = map.get("type").is_some_and(|kind| {
                    kind.as_str() == Some("object")
                        || kind
                            .as_array()
                            .is_some_and(|types| types.iter().any(|item| item == "object"))
                });
                if object_type {
                    if map.get("additionalProperties") != Some(&Value::Bool(false)) {
                        return Err(failure(
                            "invalid_config",
                            "strict mode cannot represent an open object schema",
                        ));
                    }
                    let mut required: Vec<String> = map
                        .get("required")
                        .and_then(Value::as_array)
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(Value::Object(properties)) = map.get_mut("properties") {
                        for (key, property) in properties {
                            if !required.contains(key) {
                                let object = property.as_object_mut().ok_or_else(|| {
                                    failure("invalid_config", "strict tool schema is invalid")
                                })?;
                                let accepts_null =
                                    object.get("type").and_then(Value::as_array).is_some_and(
                                        |types| types.iter().any(|value| value == "null"),
                                    ) && object
                                        .get("enum")
                                        .and_then(Value::as_array)
                                        .is_none_or(|values| values.iter().any(Value::is_null));
                                if !accepts_null {
                                    return Err(failure(
                                        "invalid_config",
                                        "strict mode cannot preserve an optional non-nullable property",
                                    ));
                                }
                                required.push(key.clone());
                            }
                            visit(property)?;
                        }
                    }
                    map.insert(
                        "required".into(),
                        Value::Array(required.into_iter().map(Value::String).collect()),
                    );
                }
                if let Some(items) = map.get_mut("items") {
                    visit(items)?;
                }
                for name in ["anyOf", "oneOf", "allOf"] {
                    if let Some(Value::Array(items)) = map.get_mut(name) {
                        for item in items {
                            visit(item)?;
                        }
                    }
                }
            }
            _ => return Err(failure("invalid_config", "tool schema must be an object")),
        }
        Ok(())
    }
    let mut schema = schema.clone();
    visit(&mut schema)?;
    Ok(schema)
}

fn object(value: &Value) -> Result<&Map<String, Value>, BoundaryError> {
    value.as_object().ok_or_else(|| {
        failure(
            "invalid_model_response",
            "provider response has invalid structure",
        )
    })
}

/// Version for the provider serializers used by local context accounting.
pub const COUNTING_ADAPTER_VERSION: &str = "saya-provider-wire-v3";

// Model-independent heuristic, separate from provider usage and billed counts.
// It is neither an exact tokenizer nor a guaranteed upper bound.
fn wire_input_tokens(body: &Value) -> Result<u64, BoundaryError> {
    let mut media_tokens = 0u64;
    let projected = project_media_for_counting(body, false, &mut media_tokens);
    let text = serde_json::to_string(&projected)
        .map_err(|_| failure("context_counter", "Wire request cannot be counted"))?;
    if text.len() > 32 * 1024 * 1024 {
        return Err(failure(
            "context_limit",
            "Wire request exceeds the counting limit",
        ));
    }
    let mut ascii = 0u64;
    let mut other = 0u64;
    for character in text.chars() {
        if character.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
    }
    Ok(ascii
        .div_ceil(4)
        .saturating_add(other)
        .saturating_add(media_tokens))
}

fn project_media_for_counting(value: &Value, media: bool, tokens: &mut u64) -> Value {
    match value {
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str);
            let is_media = matches!(
                kind,
                Some("image_url" | "input_image" | "image" | "input_audio" | "file" | "input_file")
            );
            if is_media {
                *tokens = tokens.saturating_add(match kind {
                    Some("input_audio") => 4096,
                    Some("file" | "input_file") => 8192,
                    _ => 2048,
                });
            }
            Value::Object(
                object
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            project_media_for_counting(value, media || is_media, tokens),
                        )
                    })
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| project_media_for_counting(value, media, tokens))
                .collect(),
        ),
        Value::String(text) if media && text.len() > 1024 => Value::String("[media]".into()),
        value => value.clone(),
    }
}

fn count_wire(
    request: &crate::boundary::ModelRequest,
    identity: &crate::context_builder::CountingIdentity,
    fingerprint: &str,
    settings: &ModelSettings,
    protocol: crate::contract::ModelProtocol,
    body: &Value,
) -> Result<crate::context_builder::CountEvidence, BoundaryError> {
    if identity.model != settings.model
        || identity.protocol != protocol
        || identity.adapter_version != COUNTING_ADAPTER_VERSION
        || request.profile != settings.profile
    {
        return Err(failure(
            "context_counter",
            "Wire counter identity differs from the selected serializer",
        ));
    }
    Ok(crate::context_builder::CountEvidence {
        request_fingerprint: fingerprint.into(),
        identity: identity.clone(),
        input_tokens: wire_input_tokens(body)?,
        method: crate::context_builder::CountMethod::SerializedHeuristic {
            name: "saya-wire-characters".into(),
            version: "1".into(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(max_output_tokens: u32) -> ModelSettings {
        ModelSettings {
            profile: "fixture".into(),
            model: "fixture".into(),
            context_window_tokens: 2_000_000,
            extra_safety_tokens: 0,
            max_output_tokens,
            multimodal: MediaCapabilities::default(),
            upstream_max_output_tokens: Arc::new(AtomicU32::new(0)),
            thinking_effort: None,
            strict_tools: false,
            stream: true,
        }
    }

    fn request(max_output_tokens: Option<u32>) -> ModelRequest {
        ModelRequest {
            request_id: "request".into(),
            purpose: ModelPurpose::Execution,
            profile: "fixture".into(),
            instructions: "fixture".into(),
            dynamic_instructions: None,
            max_output_tokens,
            history: vec![],
            tools: vec![],
        }
    }

    #[test]
    fn media_payloads_use_bounded_count_estimates() {
        let body = json!({
            "input":[{
                "role":"user",
                "content":[
                    {"type":"input_text","text":"inspect"},
                    {
                        "type":"input_image",
                        "image_url":format!("data:image/png;base64,{}", "A".repeat(2_000_000))
                    }
                ]
            }]
        });
        assert!(wire_input_tokens(&body).unwrap() < 10_000);
    }

    #[test]
    fn hard_limit_is_one_million_and_upstream_caps_future_output_targets() {
        let model = settings(MAX_OUTPUT_TOKENS);
        assert_eq!(model.validate(&request(None)).unwrap(), MAX_OUTPUT_TOKENS);
        model.observe_output_limit(&serde_json::json!({
            "limits": {"max_output_limit": 100_000}
        }));
        assert_eq!(model.validate(&request(None)).unwrap(), 100_000);
        assert_eq!(model.validate(&request(Some(50_000))).unwrap(), 50_000);
        assert_eq!(
            model
                .validate(&request(Some(MAX_OUTPUT_TOKENS + 1)))
                .unwrap_err()
                .0
                .code,
            "invalid_config"
        );
        assert_eq!(
            settings(MAX_OUTPUT_TOKENS + 1)
                .validate(&request(None))
                .unwrap_err()
                .0
                .code,
            "invalid_config"
        );
    }
}
