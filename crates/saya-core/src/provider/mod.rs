//! HTTP transport and provider adapters for the supported model protocols.

pub mod chat;
pub mod transport;

use crate::{
    boundary::ModelRequest,
    contract::{BoundaryError, ErrorInfo, ModelPurpose},
};
use serde_json::{Map, Value};

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
    pub max_output_tokens: u32,
    pub strict_tools: bool,
    pub stream: bool,
}
impl ModelSettings {
    fn validate(&self, request: &ModelRequest) -> Result<u32, BoundaryError> {
        if request.purpose == ModelPurpose::Review && !request.tools.is_empty() {
            return Err(failure(
                "invalid_config",
                "review requests cannot contain tools",
            ));
        }
        if self.profile.is_empty()
            || self.model.is_empty()
            || self.context_window_tokens == 0
            || self.max_output_tokens == 0
            || request.profile != self.profile
        {
            return Err(failure(
                "invalid_config",
                "model profile is invalid or does not match the request",
            ));
        }
        let output = request
            .max_output_tokens
            .unwrap_or(self.max_output_tokens)
            .min(self.max_output_tokens);
        if output == 0 {
            return Err(failure(
                "invalid_config",
                "output token limit must be positive",
            ));
        }
        Ok(output)
    }

    // Conservative, model-independent heuristic; it is not provider usage or
    // an exact tokenizer. It is deliberately separate from billed token counts.
    fn check_context(&self, request_body: &Value, output: u32) -> Result<(), BoundaryError> {
        let text = request_body.to_string();
        let mut ascii = 0u64;
        let mut other = 0u64;
        for c in text.chars() {
            if c.is_ascii() {
                ascii = ascii.saturating_add(1);
            } else {
                other = other.saturating_add(1);
            }
        }
        let estimate = ascii
            .div_ceil(4)
            .saturating_add(other)
            .saturating_add(u64::from(output));
        if estimate.saturating_mul(10) >= self.context_window_tokens.saturating_mul(9) {
            return Err(failure(
                "context_budget",
                "estimated input and output reserve reach the context limit",
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
