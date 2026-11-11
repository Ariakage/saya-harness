//! OpenAI Responses adapter with local, ordered replay of provider items.
use super::{
    ModelSettings, failure, object, strict_schema,
    transport::{EventStream, HttpTransport},
};
use crate::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
    contract::{BoundaryError, HistoryItem, ModelProtocol, ModelPurpose, Session, ToolCall, Usage},
    media,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ITEMS: usize = 256;

/// Compatibility setting for gateways that reject the optional include field.
#[derive(Default, Clone, Copy)]
pub struct ResponsesCompatibility {
    pub omit_reasoning_include: bool,
}
pub struct ResponsesModel {
    transport: Arc<HttpTransport>,
    settings: ModelSettings,
    compatibility: ResponsesCompatibility,
}
impl ResponsesModel {
    pub fn new(
        transport: Arc<HttpTransport>,
        settings: ModelSettings,
        compatibility: ResponsesCompatibility,
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
        let input = history(
            &request.history,
            self.settings.multimodal,
            &request.media,
            request.purpose == ModelPurpose::Execution,
        )?;
        let mut body = json!({"model":self.settings.model,"instructions":request.combined_instructions(),"input":input,"store":false,"stream":self.settings.stream,"max_output_tokens":output});
        let map = body
            .as_object_mut()
            .ok_or_else(|| failure("invalid_config", "invalid request body"))?;
        if let Some(effort) = self.settings.thinking_effort {
            map.insert("reasoning".into(), json!({"effort":effort}));
        }
        if !self.compatibility.omit_reasoning_include {
            map.insert("include".into(), json!(["reasoning.encrypted_content"]));
        }
        if !request.tools.is_empty() {
            let mut tools = Vec::new();
            for tool in &request.tools {
                let parameters = if self.settings.strict_tools {
                    strict_schema(&tool.input_schema)?
                } else {
                    tool.input_schema.clone()
                };
                tools.push(json!({"type":"function","name":tool.name,"description":tool.description,"parameters":parameters,"strict":self.settings.strict_tools}));
            }
            map.insert("tools".into(), Value::Array(tools));
        }
        self.settings
            .fit_output(&mut body, "max_output_tokens", output, check_budget)?;
        Ok(body)
    }
}
impl crate::context_builder::RequestCounter for ResponsesModel {
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
            crate::contract::ModelProtocol::Responses,
            &body,
        )
    }
}
impl Model for ResponsesModel {
    fn request_counter(&self) -> Option<&dyn crate::context_builder::RequestCounter> {
        Some(self)
    }

    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        if session.protocol != ModelProtocol::Responses
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
                .post("responses", &body, None, cancel)
                .await?;
            if self.settings.stream {
                Ok(Box::new(ResponsesStream {
                    events: response.events()?,
                    state: StreamState::default(),
                    finished: false,
                    settings: self.settings.clone(),
                }) as Box<dyn ModelStream>)
            } else {
                let value = response.json(cancel).await?;
                self.settings.observe_output_limit(&value);
                let (items, usage) = parse_response(&value)?;
                Ok(
                    Box::new(Ready(Some(ModelUpdate::Finished { items, usage })))
                        as Box<dyn ModelStream>,
                )
            }
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

/// Preserve each provider-owned output item immediately before its matching
/// semantic item. This makes replay lossless without duplicating tool calls.
fn parse_output(output: &[Value]) -> Result<Vec<HistoryItem>, BoundaryError> {
    if output.len() > MAX_ITEMS {
        return Err(failure(
            "invalid_model_response",
            "too many provider output items",
        ));
    }
    let mut result = Vec::new();
    let mut ids = HashSet::new();
    let mut total = 0usize;
    for item in output {
        total = total.saturating_add(item.to_string().len());
        if total > MAX_OUTPUT_BYTES {
            return Err(failure(
                "invalid_model_response",
                "provider output exceeds size limit",
            ));
        }
        let obj = object(item)?;
        if obj
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "completed")
        {
            return Err(failure(
                "invalid_model_response",
                "provider output item is incomplete",
            ));
        }
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("invalid_model_response", "provider output item has no type"))?;
        let semantic = match kind {
            "reasoning" => None,
            "message" => {
                if obj.get("role").and_then(Value::as_str) != Some("assistant") {
                    return Err(failure(
                        "invalid_model_response",
                        "invalid output message role",
                    ));
                }
                let content = obj
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        failure("invalid_model_response", "invalid output message content")
                    })?;
                let mut text = String::new();
                for part in content {
                    let part = object(part)?;
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            text.push_str(part.get("text").and_then(Value::as_str).ok_or_else(
                                || failure("invalid_model_response", "invalid output text"),
                            )?)
                        }
                        Some("refusal") => {
                            return Err(failure(
                                "provider_refusal",
                                "provider refused the request",
                            ));
                        }
                        _ => {
                            return Err(failure(
                                "invalid_model_response",
                                "unsupported output content",
                            ));
                        }
                    }
                }
                Some(HistoryItem::AssistantText(text))
            }
            "function_call" => {
                let id = obj
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| failure("invalid_model_response", "function call ID missing"))?;
                let name = obj
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| failure("invalid_model_response", "function name missing"))?;
                let arguments = obj
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        failure("invalid_model_response", "function arguments missing")
                    })?;
                if !ids.insert(id.to_owned())
                    || !serde_json::from_str::<Value>(arguments).is_ok_and(|v| v.is_object())
                {
                    return Err(failure(
                        "invalid_model_response",
                        "function call is duplicate or incomplete",
                    ));
                }
                Some(HistoryItem::ToolCall(ToolCall {
                    call_id: id.into(),
                    name: name.into(),
                    arguments: arguments.into(),
                }))
            }
            _ => {
                return Err(failure(
                    "invalid_model_response",
                    "unsupported provider output item",
                ));
            }
        };
        result.push(HistoryItem::ProviderItem {
            protocol: ModelProtocol::Responses,
            value: item.clone(),
        });
        if let Some(item) = semantic {
            result.push(item);
        }
    }
    Ok(result)
}
fn parse_response(value: &Value) -> Result<(Vec<HistoryItem>, Option<Usage>), BoundaryError> {
    let obj = object(value)?;
    match obj.get("status").and_then(Value::as_str) {
        Some("completed") => {}
        Some("incomplete") => {
            return Err(
                match obj
                    .get("incomplete_details")
                    .and_then(|v| v.get("reason"))
                    .and_then(Value::as_str)
                {
                    Some("max_output_tokens") => {
                        failure("output_limit", "provider output limit reached")
                    }
                    Some("content_filter") => {
                        failure("provider_refusal", "provider filtered the response")
                    }
                    _ => failure(
                        "invalid_model_response",
                        "provider response is incomplete for an unknown reason",
                    ),
                },
            );
        }
        Some("failed" | "cancelled") => {
            return Err(failure("provider_error", "provider response failed"));
        }
        _ => {
            return Err(failure(
                "invalid_model_response",
                "unsupported provider response status",
            ));
        }
    }
    let output = obj
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("invalid_model_response", "provider response has no output"))?;
    let usage = match obj.get("usage").filter(|v| !v.is_null()) {
        None => None,
        Some(value) => {
            let usage = object(value)?;
            let input = usage.get("input_tokens").and_then(Value::as_u64);
            let output = usage.get("output_tokens").and_then(Value::as_u64);
            if input.is_none() || output.is_none() {
                return Err(failure(
                    "invalid_model_response",
                    "provider usage is incomplete",
                ));
            }
            let cached = usage
                .get("input_tokens_details")
                .and_then(Value::as_object)
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64);
            Some(Usage {
                input_tokens: input,
                output_tokens: output,
                cached_input_tokens: cached,
                estimated: false,
            })
        }
    };
    let items = parse_output(output)?;
    if !items.iter().any(|i| {
        matches!(i,HistoryItem::AssistantText(s) if !s.is_empty())
            || matches!(i, HistoryItem::ToolCall(_))
    }) {
        return Err(failure(
            "invalid_model_response",
            "provider returned an empty response",
        ));
    }
    Ok((items, usage))
}
fn responses_user_content(
    text: &str,
    capabilities: crate::config::MediaCapabilities,
    snapshots: &crate::media::MediaAttachments,
    expand: bool,
) -> Result<Value, BoundaryError> {
    let parts = media::expand_user_for_purpose(text, capabilities, snapshots, expand)?;
    if !parts
        .iter()
        .any(|part| matches!(part, media::ContentPart::Media(_)))
    {
        return Ok(Value::String(if expand {
            text.to_owned()
        } else {
            media::display(text)
        }));
    }
    let content = parts
        .into_iter()
        .map(|part| -> Result<Value, BoundaryError> {
            Ok(match part {
                media::ContentPart::Text(text) => json!({"type":"input_text","text":text}),
                media::ContentPart::Media(item) => match item.kind {
                    media::MediaKind::Image => json!({
                        "type":"input_image",
                        "image_url":media::data_url(&item)
                    }),
                    media::MediaKind::Audio => {
                        let format = media::audio_format(&item).ok_or_else(|| {
                            failure(
                                "unsupported_media",
                                "audio format is not supported by Responses",
                            )
                        })?;
                        json!({
                            "type":"input_audio",
                            "input_audio":{"data":item.data,"format":format}
                        })
                    }
                    media::MediaKind::Video => json!({
                        "type":"input_file",
                        "filename":item.filename,
                        "file_data":media::data_url(&item)
                    }),
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Array(content))
}

fn history(
    items: &[HistoryItem],
    capabilities: crate::config::MediaCapabilities,
    snapshots: &crate::media::MediaAttachments,
    expand: bool,
) -> Result<Vec<Value>, BoundaryError> {
    let mut input = Vec::new();
    let mut pending = HashSet::new();
    let mut index = 0;
    while index < items.len() {
        match &items[index] {
            HistoryItem::UserText(text) => {
                if !pending.is_empty() {
                    return Err(failure("invalid_history", "tool results are incomplete"));
                }
                input.push(json!({
                    "role":"user",
                    "content":responses_user_content(text, capabilities, snapshots, expand)?
                }));
            }
            HistoryItem::AssistantText(text) | HistoryItem::AssistantPartial(text) => {
                if !pending.is_empty() {
                    return Err(failure("invalid_history", "tool results are incomplete"));
                }
                if !text.is_empty() {
                    input.push(json!({"role":"assistant","content":text}));
                }
            }
            HistoryItem::ToolCall(call) => {
                if call.call_id.is_empty() || !pending.insert(call.call_id.clone()) {
                    return Err(failure("invalid_history", "duplicate tool call"));
                }
                input.push(json!({"type":"function_call","call_id":call.call_id,"name":call.name,"arguments":call.arguments}));
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
                input.push(
                    json!({"type":"function_call_output","call_id":call_id,"output":content}),
                );
            }
            HistoryItem::ProviderItem {
                protocol: ModelProtocol::Responses,
                value,
            } => {
                let parsed = parse_output_item_for_replay(value)?;
                match parsed {
                    Some(HistoryItem::ToolCall(call)) => {
                        if !pending.insert(call.call_id.clone())
                            || items.get(index + 1) != Some(&HistoryItem::ToolCall(call))
                        {
                            return Err(failure(
                                "invalid_history",
                                "provider tool item does not match local call",
                            ));
                        }
                        index += 1;
                    }
                    Some(HistoryItem::AssistantText(text)) => {
                        if items.get(index + 1) != Some(&HistoryItem::AssistantText(text)) {
                            return Err(failure(
                                "invalid_history",
                                "provider message does not match local text",
                            ));
                        }
                        index += 1;
                    }
                    None => {}
                    _ => return Err(failure("invalid_history", "unsupported provider item")),
                }
                input.push(value.clone());
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
    Ok(input)
}
fn parse_output_item_for_replay(item: &Value) -> Result<Option<HistoryItem>, BoundaryError> {
    let kind = object(item)?
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("invalid_history", "provider item type missing"))?;
    if kind == "reasoning" {
        return Ok(None);
    }
    let parsed = parse_output(std::slice::from_ref(item))
        .map_err(|_| failure("invalid_history", "invalid provider output item"))?;
    Ok(parsed
        .into_iter()
        .find(|v| matches!(v, HistoryItem::AssistantText(_) | HistoryItem::ToolCall(_))))
}

#[derive(Default)]
struct StreamState {
    item_done: BTreeMap<usize, Value>,
    text: BTreeMap<(usize, usize), String>,
    text_done: BTreeMap<(usize, usize), String>,
    arguments: BTreeMap<usize, String>,
    argument_done: BTreeMap<usize, String>,
    item_ids: BTreeMap<usize, String>,
    reasoning: BTreeMap<(usize, &'static str, usize), String>,
    reasoning_done: BTreeMap<(usize, &'static str, usize), String>,
    completed: Option<(Vec<HistoryItem>, Option<Usage>)>,
    bytes: usize,
}
struct ResponsesStream {
    events: EventStream,
    state: StreamState,
    finished: bool,
    settings: ModelSettings,
}
fn index(value: &Value, key: &str) -> Result<usize, BoundaryError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .filter(|n| *n < MAX_ITEMS)
        .ok_or_else(|| failure("invalid_model_response", "invalid provider item index"))
}
impl StreamState {
    fn update(&mut self, value: &Value) -> Result<Option<String>, BoundaryError> {
        let obj = object(value)?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("invalid_model_response", "provider event has no type"))?;
        if self.completed.is_some() {
            return Err(failure(
                "invalid_model_response",
                "provider sent event after completion",
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
            "response.output_text.delta" => {
                let key = (
                    index(value, "output_index")?,
                    index(value, "content_index")?,
                );
                self.check_item_id(key.0, obj)?;
                if self.text_done.contains_key(&key) {
                    return Err(failure(
                        "invalid_model_response",
                        "text delta after completion",
                    ));
                }
                let delta = obj
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(|| failure("invalid_model_response", "invalid text delta"))?;
                self.text.entry(key).or_default().push_str(delta);
                Ok(Some(delta.into()))
            }
            "response.output_text.done" => {
                let key = (
                    index(value, "output_index")?,
                    index(value, "content_index")?,
                );
                self.check_item_id(key.0, obj)?;
                let text = obj
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| failure("invalid_model_response", "completed text missing"))?;
                if self.text.get(&key).is_some_and(|delta| delta != text)
                    || self.text_done.insert(key, text.into()).is_some()
                {
                    return Err(failure(
                        "invalid_model_response",
                        "completed text differs from deltas or is duplicated",
                    ));
                }
                Ok(None)
            }
            "response.function_call_arguments.delta" => {
                let key = index(value, "output_index")?;
                self.check_item_id(key, obj)?;
                let delta = obj
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(|| failure("invalid_model_response", "invalid argument delta"))?;
                self.arguments.entry(key).or_default().push_str(delta);
                Ok(None)
            }
            "response.function_call_arguments.done" => {
                let key = index(value, "output_index")?;
                self.check_item_id(key, obj)?;
                let arguments = obj
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        failure("invalid_model_response", "completed arguments missing")
                    })?;
                if self.argument_done.insert(key, arguments.into()).is_some() {
                    return Err(failure(
                        "invalid_model_response",
                        "duplicate completed arguments",
                    ));
                }
                Ok(None)
            }
            "response.output_item.done" => {
                let idx = index(value, "output_index")?;
                let item = obj
                    .get("item")
                    .ok_or_else(|| failure("invalid_model_response", "completed item missing"))?;
                if self.item_done.insert(idx, item.clone()).is_some() {
                    return Err(failure(
                        "invalid_model_response",
                        "duplicate completed item",
                    ));
                }
                Ok(None)
            }
            "response.completed" => {
                let response = obj.get("response").ok_or_else(|| {
                    failure("invalid_model_response", "completed response missing")
                })?;
                let output = response
                    .get("output")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        failure("invalid_model_response", "provider response has no output")
                    })?;
                for (idx, item) in &self.item_done {
                    if output.get(*idx) != Some(item) {
                        return Err(failure(
                            "invalid_model_response",
                            "completed item differs from final output",
                        ));
                    }
                }
                for (idx, args) in &self.arguments {
                    if output
                        .get(*idx)
                        .and_then(|v| v.get("arguments"))
                        .and_then(Value::as_str)
                        != Some(args)
                    {
                        return Err(failure(
                            "invalid_model_response",
                            "function argument deltas differ from final output",
                        ));
                    }
                }
                for (idx, args) in &self.argument_done {
                    if output
                        .get(*idx)
                        .and_then(|v| v.get("arguments"))
                        .and_then(Value::as_str)
                        != Some(args)
                    {
                        return Err(failure(
                            "invalid_model_response",
                            "completed arguments differ from final output",
                        ));
                    }
                }
                for (idx, id) in &self.item_ids {
                    if output
                        .get(*idx)
                        .and_then(|v| v.get("id"))
                        .and_then(Value::as_str)
                        != Some(id)
                    {
                        return Err(failure(
                            "invalid_model_response",
                            "output item identity differs from final output",
                        ));
                    }
                }
                for ((idx, part), text) in self.text.iter().chain(&self.text_done) {
                    if output
                        .get(*idx)
                        .and_then(|v| v.get("content"))
                        .and_then(Value::as_array)
                        .and_then(|parts| parts.get(*part))
                        .and_then(|v| v.get("text"))
                        .and_then(Value::as_str)
                        != Some(text)
                    {
                        return Err(failure(
                            "invalid_model_response",
                            "text deltas differ from final output",
                        ));
                    }
                }
                for ((idx, field, part), text) in self.reasoning.iter().chain(&self.reasoning_done)
                {
                    let item = output
                        .get(*idx)
                        .filter(|item| item["type"] == "reasoning")
                        .ok_or_else(|| {
                            failure(
                                "invalid_model_response",
                                "reasoning has no matching output item",
                            )
                        })?;
                    // Compatible services can omit plaintext and retain only opaque reasoning.
                    if let Some(parts) = item.get(*field)
                        && !parts.as_array().is_some_and(Vec::is_empty)
                    {
                        let expected_kind = if *field == "summary" {
                            "summary_text"
                        } else {
                            "reasoning_text"
                        };
                        if parts
                            .as_array()
                            .and_then(|parts| parts.get(*part))
                            .filter(|part| part["type"] == expected_kind)
                            .and_then(|part| part.get("text"))
                            .and_then(Value::as_str)
                            != Some(text)
                        {
                            return Err(failure(
                                "invalid_model_response",
                                "reasoning text differs from final output",
                            ));
                        }
                    } else if item
                        .get("encrypted_content")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                    {
                        return Err(failure(
                            "invalid_model_response",
                            "reasoning text missing from final output",
                        ));
                    }
                }
                self.check_final_parts(output)?;
                self.completed = Some(parse_response(response)?);
                Ok(None)
            }
            "response.failed" | "error" => Err(failure(
                "provider_error",
                "provider reported a stream error",
            )),
            "response.incomplete" => {
                let response = obj.get("response").ok_or_else(|| {
                    failure(
                        "invalid_model_response",
                        "incomplete response details missing",
                    )
                })?;
                match parse_response(response) {
                    Err(error) => Err(error),
                    Ok(_) => Err(failure(
                        "invalid_model_response",
                        "invalid incomplete response",
                    )),
                }
            }
            "response.refusal.delta" | "response.refusal.done" => {
                Err(failure("provider_refusal", "provider refused the request"))
            }
            "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.done" => {
                let (field, part_index) = if kind.starts_with("response.reasoning_summary") {
                    ("summary", "summary_index")
                } else {
                    ("content", "content_index")
                };
                let output_index = index(value, "output_index")?;
                let key = (output_index, field, index(value, part_index)?);
                self.check_item_id(output_index, obj)?;
                let completed = kind.ends_with(".done");
                let text = obj
                    .get(if completed { "text" } else { "delta" })
                    .and_then(Value::as_str)
                    .ok_or_else(|| failure("invalid_model_response", "invalid reasoning text"))?;
                if self.reasoning_done.contains_key(&key) {
                    return Err(failure(
                        "invalid_model_response",
                        "reasoning text after completion",
                    ));
                }
                if completed {
                    if self.reasoning.get(&key).is_some_and(|delta| delta != text) {
                        return Err(failure(
                            "invalid_model_response",
                            "completed reasoning differs from deltas",
                        ));
                    }
                    self.reasoning_done.insert(key, text.into());
                } else {
                    self.reasoning.entry(key).or_default().push_str(text);
                }
                Ok(None)
            }
            "response.created"
            | "response.queued"
            | "response.in_progress"
            | "response.output_item.added"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done" => Ok(None),
            _ => Err(failure(
                "invalid_model_response",
                "unsupported provider stream event",
            )),
        }
    }
    fn check_final_parts(&self, output: &[Value]) -> Result<(), BoundaryError> {
        if self.text.is_empty()
            && self.text_done.is_empty()
            && self.reasoning.is_empty()
            && self.reasoning_done.is_empty()
            && self.arguments.is_empty()
            && self.argument_done.is_empty()
            && self.item_done.is_empty()
        {
            // Some compatible endpoints provide the entire output at completion.
            return Ok(());
        }
        for (index, item) in output.iter().enumerate() {
            if self.item_done.contains_key(&index) {
                continue;
            }
            for field in ["content", "summary"] {
                let Some(parts) = item.get(field).and_then(Value::as_array) else {
                    continue;
                };
                for (part_index, part) in parts.iter().enumerate() {
                    let Some(text) = part
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    else {
                        continue;
                    };
                    let received = match (item["type"].as_str(), part["type"].as_str(), field) {
                        (Some("message"), Some("output_text"), "content") => {
                            let key = (index, part_index);
                            self.text.get(&key).or_else(|| self.text_done.get(&key))
                        }
                        (Some("reasoning"), Some("summary_text"), "summary")
                        | (Some("reasoning"), Some("reasoning_text"), "content") => {
                            let key = (index, field, part_index);
                            self.reasoning
                                .get(&key)
                                .or_else(|| self.reasoning_done.get(&key))
                        }
                        _ => continue,
                    };
                    if received.is_none_or(|received| received != text) {
                        return Err(failure(
                            "invalid_model_response",
                            "final output contains unobserved text",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    fn check_item_id(
        &mut self,
        index: usize,
        obj: &serde_json::Map<String, Value>,
    ) -> Result<(), BoundaryError> {
        if let Some(id) = obj.get("item_id") {
            let id = id
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| failure("invalid_model_response", "invalid output item identity"))?;
            if self.item_ids.get(&index).is_some_and(|old| old != id) {
                return Err(failure(
                    "invalid_model_response",
                    "output item identity changed",
                ));
            }
            self.item_ids.insert(index, id.into());
        }
        Ok(())
    }
}
impl ModelStream for ResponsesStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if self.finished {
                return Ok(None);
            }
            let event = self.events.next(cancel).await?;
            match event {
                None => {
                    self.finished = true;
                    let (items, usage) = self.state.completed.take().ok_or_else(|| {
                        failure(
                            "incomplete_model_response",
                            "provider stream ended without completion",
                        )
                    })?;
                    Ok(Some(ModelUpdate::Finished { items, usage }))
                }
                Some(event) if event.data == "[DONE]" => {
                    self.finished = true;
                    let (items, usage) = self.state.completed.take().ok_or_else(|| {
                        failure(
                            "incomplete_model_response",
                            "provider stream ended without completion",
                        )
                    })?;
                    Ok(Some(ModelUpdate::Finished { items, usage }))
                }
                Some(event) => {
                    let value: Value = serde_json::from_str(&event.data).map_err(|_| {
                        failure(
                            "invalid_model_response",
                            "provider returned malformed event JSON",
                        )
                    })?;
                    self.settings.observe_output_limit(&value);
                    let kind = value.get("type").and_then(Value::as_str).ok_or_else(|| {
                        failure("invalid_model_response", "provider event has no type")
                    })?;
                    if !event.event.is_empty() && event.event != "message" && event.event != kind {
                        return Err(failure(
                            "invalid_model_response",
                            "provider event type mismatch",
                        ));
                    }
                    let visible = self.state.update(&value)?;
                    Ok(Some(match kind {
                        "response.reasoning_summary_text.delta"
                        | "response.reasoning_text.delta" => ModelUpdate::ThinkingDelta {
                            text: value["delta"].as_str().unwrap_or_default().into(),
                        },
                        _ => visible.map_or(ModelUpdate::Progress, ModelUpdate::TextDelta),
                    }))
                }
            }
        })
    }
}

#[cfg(test)]
mod media_tests {
    use super::*;

    #[test]
    fn user_media_maps_to_responses_content_parts() {
        let root = tempfile::tempdir().unwrap();
        let image = root.path().join("image.jpg");
        let audio = root.path().join("audio.mp3");
        let video = root.path().join("video.mp4");
        std::fs::write(&image, b"jpg").unwrap();
        std::fs::write(&audio, b"mp3").unwrap();
        std::fs::write(&video, b"mp4").unwrap();
        let text = format!(
            "[Image 1](<{}>) [Audio 1](<{}>) [Video 1](<{}>)",
            image.display(),
            audio.display(),
            video.display()
        );
        let content = responses_user_content(
            &text,
            crate::config::MediaCapabilities {
                image: true,
                audio: true,
                video: true,
            },
            &crate::media::MediaAttachments::default(),
            true,
        )
        .unwrap();
        let parts = content.as_array().unwrap();
        assert!(parts.iter().any(|part| part["type"] == "input_image"));
        let audio = parts
            .iter()
            .find(|part| part["type"] == "input_audio")
            .unwrap();
        assert_eq!(audio["input_audio"]["format"], "mp3");
        let video = parts
            .iter()
            .find(|part| part["type"] == "input_file")
            .unwrap();
        assert_eq!(video["filename"], "video.mp4");
    }

    #[test]
    fn non_execution_requests_keep_media_markers_as_display_text() {
        let text = "[Image 1](<saya-media://media_0123456789abcdef0123456789abcdef_deadbeef>)";
        let content = responses_user_content(
            text,
            crate::config::MediaCapabilities {
                image: true,
                ..crate::config::MediaCapabilities::default()
            },
            &crate::media::MediaAttachments::default(),
            false,
        )
        .unwrap();
        assert_eq!(content, json!("[Image 1]"));
    }
}
