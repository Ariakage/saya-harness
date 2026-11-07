//! Anthropic Messages text and local-tool subset, including signed blocks.
use super::{
    ModelSettings, failure, object,
    transport::{EventStream, HttpTransport},
};
use crate::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
    contract::{BoundaryError, HistoryItem, ModelProtocol, Session, ToolCall, Usage},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_BLOCKS: usize = 256;
/// Supported Messages API versions. No beta features are enabled.
#[derive(Debug, Clone, Copy, Default)]
pub enum AnthropicVersion {
    #[default]
    V20261021,
    V20230601,
}
impl AnthropicVersion {
    fn header(self) -> &'static str {
        match self {
            Self::V20261021 => "2026-10-21",
            Self::V20230601 => "2023-06-01",
        }
    }
}
pub struct AnthropicModel {
    transport: Arc<HttpTransport>,
    settings: ModelSettings,
    version: AnthropicVersion,
    prompt_cache: bool,
    warmups: Mutex<BTreeMap<String, Arc<AsyncMutex<Option<Instant>>>>>,
}
impl AnthropicModel {
    pub fn new(
        transport: Arc<HttpTransport>,
        settings: ModelSettings,
        version: AnthropicVersion,
    ) -> Self {
        Self {
            transport,
            settings,
            version,
            prompt_cache: false,
            warmups: Mutex::new(BTreeMap::new()),
        }
    }
    /// Enable Anthropic ephemeral caching when the endpoint explicitly supports it.
    pub fn with_prompt_cache(mut self, enabled: bool) -> Self {
        self.prompt_cache = enabled;
        self
    }
    fn warmup_slot(&self, body: &Value) -> Option<Arc<AsyncMutex<Option<Instant>>>> {
        let key = json!({"tools":body.get("tools"),"static_system":body["system"][0]["text"]})
            .to_string();
        let key: String = Sha256::digest(key.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut slots = self.warmups.lock().ok()?;
        if !slots.contains_key(&key) && slots.len() >= 128 {
            slots.retain(|_, slot| Arc::strong_count(slot) > 1);
            if slots.len() >= 128 {
                return None;
            }
        }
        Some(
            slots
                .entry(key)
                .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
                .clone(),
        )
    }
    fn request_body(&self, request: &ModelRequest) -> Result<Value, BoundaryError> {
        let output = self.settings.validate(request)?;
        let messages = history(&request.history)?;
        let system = if self.prompt_cache
            && request.purpose == crate::contract::ModelPurpose::Execution
            && request.dynamic_instructions.is_some()
        {
            json!([{"type":"text","text":request.instructions,"cache_control":{"type":"ephemeral"}},{"type":"text","text":request.dynamic_instructions}])
        } else {
            json!(request.combined_instructions())
        };
        let mut body = json!({"model":self.settings.model,"system":system,"messages":messages,"max_tokens":output,"stream":self.settings.stream});
        let map = body
            .as_object_mut()
            .ok_or_else(|| failure("invalid_config", "invalid request body"))?;
        if let Some(effort) = self.settings.thinking_effort {
            map.insert("output_config".into(), json!({"effort":effort}));
        }
        if !request.tools.is_empty() {
            let tools:Vec<_>=request.tools.iter().map(|tool|json!({"name":tool.name,"description":tool.description,"input_schema":tool.input_schema,"strict":self.settings.strict_tools})).collect();
            map.insert("tools".into(), Value::Array(tools));
        }
        self.settings.check_context(&body, output)?;
        Ok(body)
    }
}
impl Model for AnthropicModel {
    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        if session.protocol != ModelProtocol::AnthropicMessages
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
            let mut warmup = if body["system"].is_array() {
                if let Some(slot) = self.warmup_slot(&body) {
                    let guard = tokio::select! { biased; () = cancel.cancelled() => return Err(failure("cancelled", "request cancelled")), guard = slot.lock_owned() => guard };
                    if guard.is_some_and(|at| at.elapsed() < Duration::from_secs(240)) {
                        None
                    } else {
                        Some(guard)
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let response = self
                .transport
                .post("messages", &body, Some(self.version.header()), cancel)
                .await?;
            if self.settings.stream {
                let stream = AnthropicStream {
                    events: response.events()?,
                    state: StreamState::default(),
                    finished: false,
                };
                Ok(Box::new(WarmStream {
                    inner: stream,
                    warmup,
                }) as Box<dyn ModelStream>)
            } else {
                let value = response.json(cancel).await?;
                let (items, usage) = parse_message(&value)?;
                if let Some(mut guard) = warmup.take() {
                    *guard = Some(Instant::now());
                }
                Ok(
                    Box::new(Ready(Some(ModelUpdate::Finished { items, usage })))
                        as Box<dyn ModelStream>,
                )
            }
        })
    }
}
struct WarmStream {
    inner: AnthropicStream,
    warmup: Option<OwnedMutexGuard<Option<Instant>>>,
}
impl ModelStream for WarmStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            let result = self.inner.next(cancel).await;
            if let Some(mut guard) = self.warmup.take()
                && matches!(result, Ok(Some(_)))
            {
                *guard = Some(Instant::now());
            }
            result
        })
    }
}
struct Ready(Option<ModelUpdate>);
impl ModelStream for Ready {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(failure("cancelled", "request cancelled"));
            }
            Ok(self.0.take())
        })
    }
}

fn semantic(block: &Value) -> Result<Option<HistoryItem>, BoundaryError> {
    let obj = object(block)?;
    match obj.get("type").and_then(Value::as_str) {
        Some("text") => Ok(Some(HistoryItem::AssistantText(
            obj.get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| failure("invalid_model_response", "invalid text block"))?
                .into(),
        ))),
        Some("tool_use") => {
            let id = obj
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| failure("invalid_model_response", "tool use ID missing"))?;
            let name = obj
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| failure("invalid_model_response", "tool name missing"))?;
            let input = obj
                .get("input")
                .filter(|v| v.is_object())
                .ok_or_else(|| failure("invalid_model_response", "tool input is not an object"))?;
            Ok(Some(HistoryItem::ToolCall(ToolCall {
                call_id: id.into(),
                name: name.into(),
                arguments: input.to_string(),
            })))
        }
        Some("thinking") => {
            if obj.get("thinking").and_then(Value::as_str).is_none()
                || obj
                    .get("signature")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            {
                return Err(failure(
                    "invalid_model_response",
                    "signed thinking block is incomplete",
                ));
            }
            Ok(None)
        }
        Some("redacted_thinking") => {
            if obj
                .get("data")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err(failure(
                    "invalid_model_response",
                    "redacted thinking block is incomplete",
                ));
            }
            Ok(None)
        }
        _ => Err(failure(
            "invalid_model_response",
            "unsupported message content block",
        )),
    }
}
fn parse_blocks(blocks: &[Value], stop: &str) -> Result<Vec<HistoryItem>, BoundaryError> {
    match stop {
        "end_turn" | "tool_use" => {}
        "max_tokens" => return Err(failure("output_limit", "provider output limit reached")),
        "model_context_window_exceeded" => {
            return Err(failure("context_budget", "provider context limit reached"));
        }
        "refusal" => return Err(failure("provider_refusal", "provider refused the request")),
        _ => {
            return Err(failure(
                "invalid_model_response",
                "unsupported message stop reason",
            ));
        }
    }
    if blocks.len() > MAX_BLOCKS {
        return Err(failure(
            "invalid_model_response",
            "too many message content blocks",
        ));
    }
    let mut items = Vec::new();
    let mut ids = HashSet::new();
    let mut bytes = 0usize;
    for block in blocks {
        bytes = bytes.saturating_add(block.to_string().len());
        if bytes > MAX_OUTPUT_BYTES {
            return Err(failure(
                "invalid_model_response",
                "provider output exceeds size limit",
            ));
        }
        let item = semantic(block)?;
        if let Some(HistoryItem::ToolCall(call)) = &item
            && !ids.insert(call.call_id.clone())
        {
            return Err(failure("invalid_model_response", "duplicate tool use ID"));
        }
        items.push(HistoryItem::ProviderItem {
            protocol: ModelProtocol::AnthropicMessages,
            value: block.clone(),
        });
        if let Some(item) = item {
            items.push(item);
        }
    }
    let tools = items
        .iter()
        .any(|item| matches!(item, HistoryItem::ToolCall(_)));
    let text = items
        .iter()
        .any(|item| matches!(item,HistoryItem::AssistantText(s) if !s.is_empty()));
    if (stop == "tool_use") != tools || (!tools && !text) {
        return Err(failure(
            "invalid_model_response",
            "message stop reason does not match its content",
        ));
    }
    Ok(items)
}
fn usage(value: Option<&Value>) -> Result<Option<Usage>, BoundaryError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let obj = object(value)?;
    let input = obj.get("input_tokens").and_then(Value::as_u64);
    let output = obj.get("output_tokens").and_then(Value::as_u64);
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
fn parse_message(value: &Value) -> Result<(Vec<HistoryItem>, Option<Usage>), BoundaryError> {
    let obj = object(value)?;
    if obj.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(failure("invalid_model_response", "invalid message role"));
    }
    let blocks = obj
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("invalid_model_response", "message content missing"))?;
    let stop = obj
        .get("stop_reason")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("incomplete_model_response", "message stop reason missing"))?;
    Ok((parse_blocks(blocks, stop)?, usage(obj.get("usage"))?))
}
fn append_message(messages: &mut Vec<Value>, role: &str, block: Value) {
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.push(block);
        return;
    }
    messages.push(json!({"role":role,"content":[block]}));
}
fn history(items: &[HistoryItem]) -> Result<Vec<Value>, BoundaryError> {
    let mut messages = Vec::new();
    let mut pending = HashSet::new();
    let mut index = 0;
    while index < items.len() {
        match &items[index] {
            HistoryItem::UserText(text) => {
                if !pending.is_empty() {
                    return Err(failure("invalid_history", "tool results are incomplete"));
                }
                append_message(&mut messages, "user", json!({"type":"text","text":text}));
            }
            HistoryItem::AssistantText(text) | HistoryItem::AssistantPartial(text) => {
                if !pending.is_empty()
                    && messages
                        .last()
                        .and_then(|v| v.get("role"))
                        .and_then(Value::as_str)
                        != Some("assistant")
                {
                    return Err(failure("invalid_history", "tool results are incomplete"));
                }
                if !text.is_empty() {
                    append_message(
                        &mut messages,
                        "assistant",
                        json!({"type":"text","text":text}),
                    );
                }
            }
            HistoryItem::ToolCall(call) => {
                if !pending.is_empty()
                    && messages
                        .last()
                        .and_then(|v| v.get("role"))
                        .and_then(Value::as_str)
                        != Some("assistant")
                {
                    return Err(failure("invalid_history", "tool results are incomplete"));
                }
                let args: Value = serde_json::from_str(&call.arguments)
                    .map_err(|_| failure("invalid_history", "invalid tool arguments"))?;
                if !args.is_object()
                    || call.call_id.is_empty()
                    || !pending.insert(call.call_id.clone())
                {
                    return Err(failure("invalid_history", "invalid or duplicate tool call"));
                }
                append_message(
                    &mut messages,
                    "assistant",
                    json!({"type":"tool_use","id":call.call_id,"name":call.name,"input":args}),
                );
            }
            HistoryItem::ToolResult { call_id, result } => {
                if !pending.remove(call_id) {
                    return Err(failure(
                        "invalid_history",
                        "tool result has no matching call",
                    ));
                }
                let content = serde_json::to_string(result)
                    .map_err(|_| failure("invalid_history", "tool result cannot be serialized"))?;
                append_message(
                    &mut messages,
                    "user",
                    json!({"type":"tool_result","tool_use_id":call_id,"content":content,"is_error":!result.ok}),
                );
            }
            HistoryItem::ProviderItem {
                protocol: ModelProtocol::AnthropicMessages,
                value,
            } => {
                if !pending.is_empty()
                    && messages
                        .last()
                        .and_then(|v| v.get("role"))
                        .and_then(Value::as_str)
                        != Some("assistant")
                {
                    return Err(failure("invalid_history", "tool results are incomplete"));
                }
                let item = semantic(value)
                    .map_err(|_| failure("invalid_history", "invalid provider content block"))?;
                match item {
                    Some(HistoryItem::ToolCall(call)) => {
                        if !pending.insert(call.call_id.clone())
                            || items.get(index + 1) != Some(&HistoryItem::ToolCall(call))
                        {
                            return Err(failure(
                                "invalid_history",
                                "provider tool block does not match local call",
                            ));
                        }
                        index += 1;
                    }
                    Some(HistoryItem::AssistantText(text)) => {
                        if items.get(index + 1) != Some(&HistoryItem::AssistantText(text)) {
                            return Err(failure(
                                "invalid_history",
                                "provider text block does not match local text",
                            ));
                        }
                        index += 1;
                    }
                    None => {}
                    _ => return Err(failure("invalid_history", "unsupported provider content")),
                }
                append_message(&mut messages, "assistant", value.clone());
            }
            HistoryItem::ProviderItem { .. } => {
                return Err(failure("invalid_history", "incompatible provider history"));
            }
        }
        index += 1;
    }
    if !pending.is_empty() {
        return Err(failure("invalid_history", "tool results are incomplete"));
    }
    Ok(messages)
}

#[derive(Default)]
struct Block {
    value: Value,
    arguments: String,
    stopped: bool,
    invalid_input: bool,
}
#[derive(Default)]
struct StreamState {
    started: bool,
    stopped: bool,
    blocks: BTreeMap<usize, Block>,
    stop_reason: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    bytes: usize,
}
fn index(value: &Value) -> Result<usize, BoundaryError> {
    value
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n < MAX_BLOCKS)
        .ok_or_else(|| failure("invalid_model_response", "invalid content block index"))
}
impl StreamState {
    fn update(&mut self, value: &Value) -> Result<Option<String>, BoundaryError> {
        let obj = object(value)?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("invalid_model_response", "provider event has no type"))?;
        if self.stopped {
            return Err(failure(
                "invalid_model_response",
                "provider event after message stop",
            ));
        }
        self.bytes = self.bytes.saturating_add(value.to_string().len());
        if self.bytes > MAX_OUTPUT_BYTES {
            return Err(failure(
                "invalid_model_response",
                "provider output exceeds size limit",
            ));
        }
        match kind {
            "ping" => Ok(None),
            "message_start" => {
                if self.started {
                    return Err(failure("invalid_model_response", "duplicate message start"));
                }
                let message = obj
                    .get("message")
                    .ok_or_else(|| failure("invalid_model_response", "message start missing"))?;
                if message.get("role").and_then(Value::as_str) != Some("assistant")
                    || message
                        .get("content")
                        .and_then(Value::as_array)
                        .is_none_or(|v| !v.is_empty())
                {
                    return Err(failure("invalid_model_response", "invalid message start"));
                }
                if let Some(u) = message.get("usage") {
                    self.input_tokens = u.get("input_tokens").and_then(Value::as_u64);
                    self.output_tokens = u.get("output_tokens").and_then(Value::as_u64);
                }
                self.started = true;
                Ok(None)
            }
            "content_block_start" => {
                if !self.started || self.stop_reason.is_some() {
                    return Err(failure(
                        "invalid_model_response",
                        "invalid content block start",
                    ));
                }
                let idx = index(value)?;
                if idx != self.blocks.len() {
                    return Err(failure(
                        "invalid_model_response",
                        "content block indices are not contiguous",
                    ));
                }
                if self.blocks.values().any(|b| !b.stopped) {
                    return Err(failure(
                        "invalid_model_response",
                        "overlapping content blocks",
                    ));
                }
                let block = obj
                    .get("content_block")
                    .ok_or_else(|| failure("invalid_model_response", "content block missing"))?
                    .clone();
                let block_obj = object(&block)?;
                match block_obj.get("type").and_then(Value::as_str) {
                    Some("text") if block_obj.get("text").and_then(Value::as_str).is_some() => {}
                    Some("tool_use") if block_obj.get("input").is_some_and(Value::is_object) => {}
                    Some("thinking")
                        if block_obj.get("thinking").and_then(Value::as_str).is_some() => {}
                    Some("redacted_thinking") => {}
                    _ => {
                        return Err(failure(
                            "invalid_model_response",
                            "unsupported content block start",
                        ));
                    }
                }
                let initial_text = (block_obj.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| {
                        block_obj
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned()
                    })
                    .filter(|text| !text.is_empty());
                self.blocks.insert(
                    idx,
                    Block {
                        value: block,
                        ..Block::default()
                    },
                );
                Ok(initial_text)
            }
            "content_block_delta" => {
                let idx = index(value)?;
                let block = self
                    .blocks
                    .get_mut(&idx)
                    .filter(|b| !b.stopped)
                    .ok_or_else(|| {
                        failure(
                            "invalid_model_response",
                            "delta has no active content block",
                        )
                    })?;
                let delta = obj
                    .get("delta")
                    .ok_or_else(|| failure("invalid_model_response", "content delta missing"))?;
                let kind = delta.get("type").and_then(Value::as_str).ok_or_else(|| {
                    failure("invalid_model_response", "content delta type missing")
                })?;
                let block_kind = block
                    .value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let (field, delta_field) = match (block_kind, kind) {
                    ("text", "text_delta") => ("text", "text"),
                    ("thinking", "thinking_delta") => ("thinking", "thinking"),
                    ("thinking", "signature_delta") => ("signature", "signature"),
                    ("tool_use", "input_json_delta") => ("", "partial_json"),
                    _ => {
                        return Err(failure(
                            "invalid_model_response",
                            "delta does not match its content block",
                        ));
                    }
                };
                let text = delta
                    .get(delta_field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| failure("invalid_model_response", "invalid content delta"))?;
                if field.is_empty() {
                    block.arguments.push_str(text);
                } else {
                    let map = block.value.as_object_mut().ok_or_else(|| {
                        failure("invalid_model_response", "invalid content block")
                    })?;
                    let previous = map.get(field).and_then(Value::as_str).unwrap_or("");
                    map.insert(field.into(), json!(format!("{previous}{text}")));
                }
                if kind == "text_delta" {
                    Ok(Some(text.into()))
                } else {
                    Ok(None)
                }
            }
            "content_block_stop" => {
                let block = self
                    .blocks
                    .get_mut(&index(value)?)
                    .filter(|b| !b.stopped)
                    .ok_or_else(|| {
                        failure("invalid_model_response", "content stop has no active block")
                    })?;
                if !block.arguments.is_empty() {
                    if block
                        .value
                        .get("input")
                        .and_then(Value::as_object)
                        .is_some_and(|m| !m.is_empty())
                    {
                        block.invalid_input = true;
                    } else if let Ok(input) = serde_json::from_str::<Value>(&block.arguments)
                        && input.is_object()
                    {
                        block.value["input"] = input;
                    } else {
                        block.invalid_input = true;
                    }
                }
                block.stopped = true;
                Ok(None)
            }
            "message_delta" => {
                if !self.started
                    || self.stop_reason.is_some()
                    || self.blocks.values().any(|b| !b.stopped)
                {
                    return Err(failure("invalid_model_response", "invalid message delta"));
                }
                let delta = obj
                    .get("delta")
                    .ok_or_else(|| failure("invalid_model_response", "message delta missing"))?;
                match delta.get("stop_reason") {
                    None | Some(Value::Null) => {}
                    Some(Value::String(reason)) => self.stop_reason = Some(reason.clone()),
                    _ => {
                        return Err(failure(
                            "invalid_model_response",
                            "invalid message stop reason",
                        ));
                    }
                }
                if let Some(u) = obj.get("usage") {
                    self.output_tokens = u
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .or(self.output_tokens);
                }
                Ok(None)
            }
            "message_stop" => {
                if !self.started
                    || self.stop_reason.is_none()
                    || self.blocks.values().any(|b| !b.stopped)
                {
                    return Err(failure(
                        "incomplete_model_response",
                        "message ended before completion",
                    ));
                }
                self.stopped = true;
                Ok(None)
            }
            "error" => Err(failure(
                "provider_error",
                "provider reported a stream error",
            )),
            _ => Err(failure(
                "invalid_model_response",
                "unsupported provider stream event",
            )),
        }
    }
    fn finish(self) -> Result<(Vec<HistoryItem>, Option<Usage>), BoundaryError> {
        if !self.stopped {
            return Err(failure(
                "incomplete_model_response",
                "provider stream ended without message stop",
            ));
        }
        let reason = self
            .stop_reason
            .ok_or_else(|| failure("incomplete_model_response", "message stop reason missing"))?;
        let invalid_input = self.blocks.values().any(|block| block.invalid_input);
        let blocks: Vec<Value> = self.blocks.into_values().map(|b| b.value).collect();
        let result = parse_blocks(&blocks, &reason)?;
        if invalid_input {
            return Err(failure(
                "invalid_model_response",
                "tool input JSON is invalid or incomplete",
            ));
        }
        let usage = match (self.input_tokens, self.output_tokens) {
            (Some(i), Some(o)) => Some(Usage {
                input_tokens: Some(i),
                output_tokens: Some(o),
                estimated: false,
            }),
            _ => None,
        };
        Ok((result, usage))
    }
}
struct AnthropicStream {
    events: EventStream,
    state: StreamState,
    finished: bool,
}
impl ModelStream for AnthropicStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if self.finished {
                return Ok(None);
            }
            match self.events.next(cancel).await? {
                None => {
                    self.finished = true;
                    let (items, usage) = std::mem::take(&mut self.state).finish()?;
                    Ok(Some(ModelUpdate::Finished { items, usage }))
                }
                Some(event) if event.data == "[DONE]" => {
                    self.finished = true;
                    let (items, usage) = std::mem::take(&mut self.state).finish()?;
                    Ok(Some(ModelUpdate::Finished { items, usage }))
                }
                Some(event) => {
                    let value: Value = serde_json::from_str(&event.data).map_err(|_| {
                        failure(
                            "invalid_model_response",
                            "provider returned malformed event JSON",
                        )
                    })?;
                    let kind = value.get("type").and_then(Value::as_str).ok_or_else(|| {
                        failure("invalid_model_response", "provider event has no type")
                    })?;
                    if !event.event.is_empty() && event.event != "message" && event.event != kind {
                        return Err(failure(
                            "invalid_model_response",
                            "provider event type mismatch",
                        ));
                    }
                    Ok(Some(
                        self.state
                            .update(&value)?
                            .map_or(ModelUpdate::Progress, ModelUpdate::TextDelta),
                    ))
                }
            }
        })
    }
}
