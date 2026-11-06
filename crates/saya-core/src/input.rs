//! Clarification requests are untrusted data, distinct from system instructions.
use crate::{
    boundary::{Cancellation, Operation, ToolDefinition},
    contract::{BoundaryError, ErrorInfo, ToolCall},
};
use serde::Deserialize;

/// One bounded question and optional suggestions; free-form answers remain valid.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRequest {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}
impl InputRequest {
    pub(crate) fn parse(call: &ToolCall) -> Result<Self, BoundaryError> {
        let request: Self = serde_json::from_str(&call.arguments).map_err(|_| invalid())?;
        if request.question.trim().is_empty()
            || request.question.len() > 8192
            || request.options.len() > 10
            || request
                .options
                .iter()
                .any(|v| v.trim().is_empty() || v.len() > 1024)
        {
            return Err(invalid());
        }
        Ok(request)
    }
}
fn invalid() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "invalid_args".into(),
            message: "invalid clarification question or options".into(),
        },
        None,
        None,
    )
}
/// UI-provided input, without authority to change policies or execute tools.
pub trait InputHandler: Send + Sync {
    fn ask<'a>(
        &'a self,
        request: &'a InputRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Option<String>>;
}
pub(crate) fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "ask_user".into(),
        description: "Ask the user a clarification question and wait for their answer.".into(),
        input_schema: serde_json::json!({"type":"object","properties":{"question":{"type":"string"},"options":{"type":"array","items":{"type":"string"}}},"required":["question"],"additionalProperties":false}),
    }
}
