//! OpenAI Chat Completions adapter. Tool deltas are assembled by index and
//! dispatched only after a complete terminal response.

use super::{
    ModelSettings, failure, object, strict_schema,
    transport::{EventStream, HttpTransport},
};
use crate::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
    contract::{BoundaryError, HistoryItem, ModelProtocol, Session, ToolCall, Usage},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::Arc,
};

const MAX_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_CALLS: usize = 256;

/// Chat-specific compatibility options. Defaults enable streaming and request
/// usage; the profile's `stream` flag can select a complete JSON response.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatCompatibility {
    /// Use `max_tokens` for gateways that do not accept `max_completion_tokens`.
    pub legacy_max_tokens: bool,
    /// Suppress `stream_options` for gateways that do not support usage chunks.
    pub omit_stream_usage: bool,
}

pub struct ChatModel {
    transport: Arc<HttpTransport>,
    settings: ModelSettings,
    compatibility: ChatCompatibility,
}
impl ChatModel {
    /// Construct a Chat Completions model bound to one configured profile.
    pub fn new(
        transport: Arc<HttpTransport>,
        settings: ModelSettings,
        compatibility: ChatCompatibility,
    ) -> Self {
        Self {
            transport,
            settings,
            compatibility,
        }
    }

    fn request_body(&self, request: &ModelRequest) -> Result<Value, BoundaryError> {
        self.serialized_request(request, true)
    }
    fn serialized_request(
        &self,
        request: &ModelRequest,
        check_budget: bool,
    ) -> Result<Value, BoundaryError> {
        let output = self.settings.validate(request)?;
        let messages = chat_history(&request.combined_instructions(), &request.history)?;
        let mut body = json!({"model":self.settings.model, "messages":messages, "store":false, "stream":self.settings.stream});
        let map = body
            .as_object_mut()
            .ok_or_else(|| failure("invalid_config", "invalid request body"))?;
        if let Some(effort) = self.settings.thinking_effort {
            map.insert("reasoning_effort".into(), json!(effort));
        }
        let output_field = if self.compatibility.legacy_max_tokens {
            "max_tokens"
        } else {
            "max_completion_tokens"
        };
        map.insert(output_field.into(), json!(output));
        if self.settings.stream && !self.compatibility.omit_stream_usage {
            map.insert("stream_options".into(), json!({"include_usage":true}));
        }
        if !request.tools.is_empty() {
            let mut tools = Vec::new();
            for tool in &request.tools {
                let parameters = if self.settings.strict_tools {
                    strict_schema(&tool.input_schema)?
                } else {
                    tool.input_schema.clone()
                };
                tools.push(json!({"type":"function", "function":{"name":tool.name,"description":tool.description,"parameters":parameters,"strict":self.settings.strict_tools}}));
            }
            map.insert("tools".into(), Value::Array(tools));
        }
        self.settings
            .fit_output(&mut body, output_field, output, check_budget)?;
        Ok(body)
    }
}

fn chat_history(instructions: &str, history: &[HistoryItem]) -> Result<Vec<Value>, BoundaryError> {
    let mut messages = vec![json!({"role":"system","content":instructions})];
    let mut text: Option<String> = None;
    let mut calls = Vec::new();
    let mut reasoning: Option<String> = None;
    let mut pending = HashSet::new();
    fn flush(
        messages: &mut Vec<Value>,
        text: &mut Option<String>,
        calls: &mut Vec<Value>,
        reasoning: &mut Option<String>,
    ) -> Result<(), BoundaryError> {
        if text.is_some() || !calls.is_empty() {
            let mut message = json!({"role":"assistant","content":text.take()});
            if let Some(reasoning) = reasoning.take() {
                let retained = calls.iter().fold(
                    reasoning
                        .len()
                        .saturating_add(message["content"].as_str().map_or(0, str::len)),
                    |sum, call| {
                        sum.saturating_add(call["id"].as_str().map_or(0, str::len))
                            .saturating_add(call["function"]["name"].as_str().map_or(0, str::len))
                            .saturating_add(
                                call["function"]["arguments"].as_str().map_or(0, str::len),
                            )
                    },
                );
                if retained > MAX_CONTENT_BYTES {
                    return Err(failure(
                        "invalid_history",
                        "assistant replay exceeds size limit",
                    ));
                }
                message["reasoning_content"] = Value::String(reasoning);
            }
            if !calls.is_empty() {
                message["tool_calls"] = Value::Array(std::mem::take(calls));
            }
            messages.push(message);
        } else if reasoning.is_some() {
            return Err(failure(
                "invalid_history",
                "reasoning has no assistant message",
            ));
        }
        Ok(())
    }
    for (index, item) in history.iter().enumerate() {
        match item {
            HistoryItem::UserText(value) => {
                flush(&mut messages, &mut text, &mut calls, &mut reasoning)?;
                if !pending.is_empty() {
                    return Err(failure("invalid_history", "tool results are incomplete"));
                }
                messages.push(json!({"role":"user","content":value}));
            }
            HistoryItem::AssistantPartial(value) if value.is_empty() => {}
            HistoryItem::AssistantText(value) | HistoryItem::AssistantPartial(value) => {
                if text.is_some() || !calls.is_empty() {
                    flush(&mut messages, &mut text, &mut calls, &mut reasoning)?;
                }
                if !pending.is_empty() {
                    return Err(failure("invalid_history", "tool results are incomplete"));
                }
                text = Some(value.clone());
            }
            HistoryItem::ToolCall(call) => {
                if call.call_id.is_empty()
                    || call.name.is_empty()
                    || !pending.insert(call.call_id.clone())
                {
                    return Err(failure("invalid_history", "invalid or duplicate tool call"));
                }
                calls.push(json!({"id":call.call_id,"type":"function","function":{"name":call.name,"arguments":call.arguments}}));
            }
            HistoryItem::ToolResult { call_id, result } => {
                flush(&mut messages, &mut text, &mut calls, &mut reasoning)?;
                if !pending.remove(call_id) {
                    return Err(failure(
                        "invalid_history",
                        "tool result has no matching call",
                    ));
                }
                let content = serde_json::to_string(result)
                    .map_err(|_| failure("invalid_history", "tool result cannot be serialized"))?;
                messages.push(json!({"role":"tool","tool_call_id":call_id,"content":content}));
            }
            HistoryItem::ProviderItem { protocol, value } => {
                flush(&mut messages, &mut text, &mut calls, &mut reasoning)?;
                if *protocol != ModelProtocol::ChatCompletions || !pending.is_empty() {
                    return Err(failure("invalid_history", "incompatible provider history"));
                }
                let valid_assistant = match history.get(index + 1) {
                    Some(HistoryItem::AssistantText(text)) => !text.is_empty(),
                    Some(HistoryItem::ToolCall(_)) => true,
                    _ => false,
                };
                let fields = value.as_object().filter(|fields| fields.len() == 1);
                let retained = fields
                    .and_then(|fields| fields.get("reasoning_content"))
                    .and_then(Value::as_str);
                let retained = retained
                    .filter(|text| text.len() <= MAX_CONTENT_BYTES)
                    .ok_or_else(|| {
                        failure("invalid_history", "invalid assistant reasoning replay")
                    })?;
                if !valid_assistant {
                    return Err(failure(
                        "invalid_history",
                        "reasoning has no complete assistant message",
                    ));
                }
                reasoning = Some(retained.to_owned());
            }
        }
    }
    flush(&mut messages, &mut text, &mut calls, &mut reasoning)?;
    if !pending.is_empty() {
        return Err(failure("invalid_history", "tool results are incomplete"));
    }
    Ok(messages)
}

impl crate::context_builder::RequestCounter for ChatModel {
    fn count(
        &self,
        request: &ModelRequest,
        identity: &crate::context_builder::CountingIdentity,
        fingerprint: &str,
    ) -> Result<crate::context_builder::CountEvidence, BoundaryError> {
        let body = self.serialized_request(request, false)?;
        super::count_wire(
            request,
            identity,
            fingerprint,
            &self.settings,
            crate::contract::ModelProtocol::ChatCompletions,
            &body,
        )
    }
}
impl Model for ChatModel {
    fn request_counter(&self) -> Option<&dyn crate::context_builder::RequestCounter> {
        Some(self)
    }

    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        if session.protocol != ModelProtocol::ChatCompletions
            || session.profile != self.settings.profile
            || session.model != self.settings.model
        {
            return Err(failure(
                "invalid_config",
                "session model or protocol differs from the configured profile",
            ));
        }
        Ok(())
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let body = self.request_body(request)?;
            let response = self
                .transport
                .post("chat/completions", &body, None, cancel)
                .await?;
            if self.settings.stream {
                Ok(Box::new(ChatStream {
                    events: response.events()?,
                    state: ChatState::default(),
                    ended: false,
                    pending: VecDeque::new(),
                }) as Box<dyn ModelStream>)
            } else {
                let value = response.json(cancel).await?;
                let (items, usage) = complete_json(&value)?;
                Ok(
                    Box::new(ReadyStream(Some(ModelUpdate::Finished { items, usage })))
                        as Box<dyn ModelStream>,
                )
            }
        })
    }
}
struct ReadyStream(Option<ModelUpdate>);
impl ModelStream for ReadyStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(failure("cancelled", "request cancelled"));
            }
            Ok(self.0.take())
        })
    }
}

#[derive(Default)]
struct CallParts {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}
#[derive(Default)]
struct ChatState {
    text: String,
    reasoning: Option<String>,
    calls: BTreeMap<usize, CallParts>,
    finish: Option<String>,
    usage: Option<Usage>,
}
struct ChatStream {
    events: EventStream,
    state: ChatState,
    ended: bool,
    pending: VecDeque<ModelUpdate>,
}
impl ModelStream for ChatStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if self.ended {
                return Ok(None);
            }
            if cancel.is_cancelled() {
                return Err(failure("cancelled", "request cancelled"));
            }
            if let Some(update) = self.pending.pop_front() {
                return Ok(Some(update));
            }
            let event = self.events.next(cancel).await?.ok_or_else(|| {
                failure(
                    "incomplete_model_response",
                    "provider stream ended without completion",
                )
            })?;
            if !event.event.is_empty() && event.event != "message" {
                return Err(failure(
                    "invalid_model_response",
                    "unexpected provider event",
                ));
            }
            if event.data == "[DONE]" {
                self.ended = true;
                let (items, usage) = std::mem::take(&mut self.state).finish()?;
                return Ok(Some(ModelUpdate::Finished { items, usage }));
            }
            let value: Value = serde_json::from_str(&event.data).map_err(|_| {
                failure(
                    "invalid_model_response",
                    "provider returned malformed event JSON",
                )
            })?;
            let delta = self.state.update(&value)?;
            if let Some(text) = value["choices"][0]["delta"]["reasoning_content"]
                .as_str()
                .filter(|text| !text.is_empty())
            {
                self.pending
                    .push_back(ModelUpdate::ThinkingDelta { text: text.into() });
            }
            if let Some(text) = delta {
                self.pending.push_back(ModelUpdate::TextDelta(text));
            }
            Ok(Some(
                self.pending.pop_front().unwrap_or(ModelUpdate::Progress),
            ))
        })
    }
}
fn optional_string(value: Option<&Value>) -> Result<Option<&str>, BoundaryError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => Err(failure(
            "invalid_model_response",
            "provider returned an invalid text field",
        )),
    }
}
fn set_once(target: &mut Option<String>, value: Option<&str>) -> Result<(), BoundaryError> {
    if let Some(value) = value {
        if target.as_deref().is_some_and(|old| old != value) {
            return Err(failure(
                "invalid_model_response",
                "provider changed a tool call identity",
            ));
        }
        if target.is_none() {
            *target = Some(value.to_owned());
        }
    }
    Ok(())
}
fn bounded_push(target: &mut String, value: &str) -> Result<(), BoundaryError> {
    if target.len().saturating_add(value.len()) > MAX_CONTENT_BYTES {
        return Err(failure(
            "invalid_model_response",
            "provider content exceeds size limit",
        ));
    }
    target.push_str(value);
    Ok(())
}
fn parse_usage(value: Option<&Value>) -> Result<Option<Usage>, BoundaryError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let usage = object(value)?;
    let input = usage.get("prompt_tokens").and_then(Value::as_u64);
    let output = usage.get("completion_tokens").and_then(Value::as_u64);
    if input.is_none() || output.is_none() {
        return Err(failure(
            "invalid_model_response",
            "provider usage is incomplete",
        ));
    }
    Ok(Some(Usage {
        input_tokens: input,
        output_tokens: output,
        estimated: false,
    }))
}
impl ChatState {
    fn update(&mut self, value: &Value) -> Result<Option<String>, BoundaryError> {
        let obj = object(value)?;
        if obj.contains_key("error") {
            return Err(failure(
                "provider_error",
                "provider reported a stream error",
            ));
        }
        if let Some(usage) = parse_usage(obj.get("usage"))? {
            self.usage = Some(usage);
        }
        let choices = obj
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| failure("invalid_model_response", "provider event has no choices"))?;
        if choices.is_empty() {
            return Ok(None);
        }
        if choices.len() != 1 {
            return Err(failure(
                "invalid_model_response",
                "multiple completion choices are unsupported",
            ));
        }
        let choice = object(&choices[0])?;
        if choice.get("index").and_then(Value::as_u64) != Some(0) {
            return Err(failure(
                "invalid_model_response",
                "invalid completion choice index",
            ));
        }
        let delta = object(
            choice
                .get("delta")
                .ok_or_else(|| failure("invalid_model_response", "missing completion delta"))?,
        )?;
        if self.finish.is_some() && !delta.is_empty() {
            return Err(failure(
                "invalid_model_response",
                "provider sent content after finish",
            ));
        }
        if optional_string(delta.get("role"))?.is_some_and(|role| role != "assistant") {
            return Err(failure("invalid_model_response", "invalid completion role"));
        }
        if delta.get("refusal").is_some_and(|v| !v.is_null()) {
            return Err(failure("provider_refusal", "provider refused the request"));
        }
        if delta.get("function_call").is_some_and(|v| !v.is_null())
            || delta.get("audio").is_some_and(|v| !v.is_null())
        {
            return Err(failure(
                "invalid_model_response",
                "unsupported completion content",
            ));
        }
        if let Some(reasoning) = optional_string(delta.get("reasoning_content"))? {
            bounded_push(self.reasoning.get_or_insert_with(String::new), reasoning)?;
        }
        let text = optional_string(delta.get("content"))?.map(str::to_owned);
        if let Some(value) = &text {
            bounded_push(&mut self.text, value)?;
        }
        if let Some(value) = delta.get("tool_calls").filter(|v| !v.is_null()) {
            let list = value
                .as_array()
                .ok_or_else(|| failure("invalid_model_response", "invalid tool call deltas"))?;
            for call in list {
                let call = object(call)?;
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|v| usize::try_from(v).ok())
                    .filter(|v| *v < MAX_CALLS)
                    .ok_or_else(|| failure("invalid_model_response", "invalid tool call index"))?;
                let part = self.calls.entry(index).or_default();
                if optional_string(call.get("type"))?.is_some_and(|kind| kind != "function") {
                    return Err(failure(
                        "invalid_model_response",
                        "unsupported tool call type",
                    ));
                }
                set_once(&mut part.id, optional_string(call.get("id"))?)?;
                if let Some(function) = call.get("function").filter(|v| !v.is_null()) {
                    let function = object(function)?;
                    set_once(&mut part.name, optional_string(function.get("name"))?)?;
                    if let Some(arguments) = optional_string(function.get("arguments"))? {
                        bounded_push(&mut part.arguments, arguments)?;
                    }
                }
            }
        }
        if let Some(reason) = optional_string(choice.get("finish_reason"))?
            && self.finish.replace(reason.to_owned()).is_some()
        {
            return Err(failure(
                "invalid_model_response",
                "duplicate completion finish",
            ));
        }
        self.check_size()?;
        Ok(text)
    }
    fn check_size(&self) -> Result<(), BoundaryError> {
        let retained = self.calls.values().fold(
            self.text
                .len()
                .saturating_add(self.reasoning.as_ref().map_or(0, String::len)),
            |sum, call| {
                sum.saturating_add(call.arguments.len())
                    .saturating_add(call.id.as_ref().map_or(0, String::len))
                    .saturating_add(call.name.as_ref().map_or(0, String::len))
            },
        );
        if retained > MAX_CONTENT_BYTES {
            return Err(failure(
                "invalid_model_response",
                "provider content exceeds size limit",
            ));
        }
        Ok(())
    }
    fn finish(self) -> Result<(Vec<HistoryItem>, Option<Usage>), BoundaryError> {
        self.check_size()?;
        let reason = self.finish.as_deref().ok_or_else(|| {
            failure(
                "incomplete_model_response",
                "provider omitted finish reason",
            )
        })?;
        match reason {
            "stop" if self.calls.is_empty() => {}
            "tool_calls" if !self.calls.is_empty() => {}
            "length" => return Err(failure("output_limit", "provider output limit reached")),
            "content_filter" => {
                return Err(failure(
                    "provider_refusal",
                    "provider filtered the response",
                ));
            }
            _ => {
                return Err(failure(
                    "invalid_model_response",
                    "provider returned an unsupported finish reason",
                ));
            }
        }
        let mut items = Vec::new();
        if !self.text.is_empty() {
            items.push(HistoryItem::AssistantText(self.text));
        }
        let mut ids = HashSet::new();
        for (expected, (index, call)) in self.calls.into_iter().enumerate() {
            if expected != index {
                return Err(failure(
                    "invalid_model_response",
                    "tool call indices are incomplete",
                ));
            }
            let id = call
                .id
                .filter(|v| !v.is_empty())
                .ok_or_else(|| failure("invalid_model_response", "tool call ID missing"))?;
            let name = call
                .name
                .filter(|v| !v.is_empty())
                .ok_or_else(|| failure("invalid_model_response", "tool call name missing"))?;
            if !ids.insert(id.clone()) {
                return Err(failure("invalid_model_response", "duplicate tool call ID"));
            }
            if !serde_json::from_str::<Value>(&call.arguments).is_ok_and(|v| v.is_object()) {
                return Err(failure(
                    "invalid_model_response",
                    "tool arguments are incomplete or invalid",
                ));
            }
            items.push(HistoryItem::ToolCall(ToolCall {
                call_id: id,
                name,
                arguments: call.arguments,
            }));
        }
        if items.is_empty() {
            return Err(failure(
                "invalid_model_response",
                "provider returned an empty response",
            ));
        }
        if let Some(reasoning) = self.reasoning {
            items.insert(
                0,
                HistoryItem::ProviderItem {
                    protocol: ModelProtocol::ChatCompletions,
                    value: json!({"reasoning_content":reasoning}),
                },
            );
        }
        Ok((items, self.usage))
    }
}

fn complete_json(value: &Value) -> Result<(Vec<HistoryItem>, Option<Usage>), BoundaryError> {
    let obj = object(value)?;
    let choices = obj
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("invalid_model_response", "provider response has no choices"))?;
    if choices.len() != 1 {
        return Err(failure(
            "invalid_model_response",
            "multiple or missing completion choices",
        ));
    }
    let choice = object(&choices[0])?;
    if choice.get("index").and_then(Value::as_u64) != Some(0) {
        return Err(failure(
            "invalid_model_response",
            "invalid completion choice index",
        ));
    }
    let message =
        object(choice.get("message").ok_or_else(|| {
            failure("invalid_model_response", "provider response has no message")
        })?)?;
    if optional_string(message.get("role"))? != Some("assistant") {
        return Err(failure("invalid_model_response", "invalid completion role"));
    }
    if message.get("refusal").is_some_and(|v| !v.is_null()) {
        return Err(failure("provider_refusal", "provider refused the request"));
    }
    let mut state = ChatState::default();
    if let Some(reasoning) = optional_string(message.get("reasoning_content"))? {
        bounded_push(state.reasoning.get_or_insert_with(String::new), reasoning)?;
    }
    if let Some(text) = optional_string(message.get("content"))? {
        bounded_push(&mut state.text, text)?;
    }
    if let Some(calls) = message.get("tool_calls").filter(|v| !v.is_null()) {
        for (index, value) in calls
            .as_array()
            .ok_or_else(|| failure("invalid_model_response", "invalid tool calls"))?
            .iter()
            .enumerate()
        {
            if index >= MAX_CALLS {
                return Err(failure("invalid_model_response", "too many tool calls"));
            }
            let call = object(value)?;
            if optional_string(call.get("type"))? != Some("function") {
                return Err(failure(
                    "invalid_model_response",
                    "unsupported tool call type",
                ));
            }
            let function = object(
                call.get("function")
                    .ok_or_else(|| failure("invalid_model_response", "missing tool function"))?,
            )?;
            let mut part = CallParts::default();
            set_once(&mut part.id, optional_string(call.get("id"))?)?;
            set_once(&mut part.name, optional_string(function.get("name"))?)?;
            if let Some(args) = optional_string(function.get("arguments"))? {
                bounded_push(&mut part.arguments, args)?;
            }
            state.calls.insert(index, part);
        }
    }
    state.finish = optional_string(choice.get("finish_reason"))?.map(str::to_owned);
    state.usage = parse_usage(obj.get("usage"))?;
    state.finish()
}

#[cfg(test)]
mod reasoning_tests {
    use super::*;

    #[test]
    fn reasoning_and_visible_output_share_the_aggregate_limit() {
        let half = "a".repeat(MAX_CONTENT_BYTES / 2);
        let mut state = ChatState::default();
        assert_eq!(
            state
                .update(&json!({
                    "choices": [{
                        "index": 0,
                        "delta": {"reasoning_content": half},
                        "finish_reason": null
                    }]
                }))
                .unwrap(),
            None
        );
        let error = state
            .update(&json!({
                "choices": [{
                    "index": 0,
                    "delta": {"content": format!("{half}b")},
                    "finish_reason": null
                }]
            }))
            .unwrap_err();
        assert_eq!(error.0.code, "invalid_model_response");
        let error = complete_json(&json!({
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "reasoning_content": half,
                    "content": format!("{half}b")
                }
            }]
        }))
        .unwrap_err();
        assert_eq!(error.0.code, "invalid_model_response");
    }

    #[test]
    fn explicitly_empty_reasoning_is_preserved_without_becoming_text() {
        let (items, _) = complete_json(&json!({
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "reasoning_content": "",
                    "content": "visible"
                }
            }]
        }))
        .unwrap();
        assert_eq!(
            items,
            vec![
                HistoryItem::ProviderItem {
                    protocol: ModelProtocol::ChatCompletions,
                    value: json!({"reasoning_content":""})
                },
                HistoryItem::AssistantText("visible".into())
            ]
        );
    }
}
