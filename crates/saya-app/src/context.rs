//! Content-free, normalized context estimates taken at the model boundary.
//! Counts aggregate ASCII characters at four per token and other scalars at one
//! per token. They are not provider tokenizer counts or complete wire overhead.
use saya_core::{
    boundary::{Cancellation, Model, ModelRequest, ModelStream, Operation},
    contract::{
        BoundaryError, HistoryItem, ModelProtocol, ModelPurpose, PermissionMode, Session, WorkMode,
    },
};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEntry {
    pub label: &'static str,
    pub tokens: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSnapshot {
    pub entries: Vec<ContextEntry>,
    /// Discovered catalog entries. None means no recognized skills source.
    pub skills_count: Option<usize>,
}
/// Match the runtime's actual injected sources, not user-supplied JSON keys.
#[derive(Debug, Default, Clone, Copy)]
pub struct ContextSources {
    pub project: bool,
    pub personal: bool,
}

/// Publish only the latest execution estimate without buffering request text.
/// Validation, cancellation, errors and returned streams belong to the inner
/// model. Review requests leave the execution snapshot unchanged.
pub struct ContextModel {
    inner: Arc<dyn Model>,
    sender: watch::Sender<Option<ContextSnapshot>>,
    sources: ContextSources,
}
impl ContextModel {
    pub fn new(
        inner: Arc<dyn Model>,
        sender: watch::Sender<Option<ContextSnapshot>>,
        sources: ContextSources,
    ) -> Self {
        Self {
            inner,
            sender,
            sources,
        }
    }
}
impl Model for ContextModel {
    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        self.inner.validate_session(session)
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            if request.purpose == ModelPurpose::Execution {
                self.sender
                    .send_replace(Some(snapshot(request, self.sources)));
            }
            self.inner.start(request, cancel).await
        })
    }
}

const LABELS: [&str; 9] = [
    "System",
    "Instructions",
    "Skills",
    "Preferences",
    "Memory",
    "Conversation",
    "Files",
    "Tools",
    "Protocol",
];
const SYSTEM: usize = 0;
const INSTRUCTIONS: usize = 1;
const SKILLS: usize = 2;
const PREFERENCES: usize = 3;
const MEMORY: usize = 4;
const CONVERSATION: usize = 5;
const FILES: usize = 6;
const TOOLS: usize = 7;
const PROTOCOL: usize = 8;

#[derive(Clone, Copy, Default)]
struct Estimate {
    ascii: u64,
    other: u64,
}
impl Estimate {
    fn text(&mut self, text: &str) {
        for character in text.chars() {
            if character.is_ascii() {
                self.ascii = self.ascii.saturating_add(1);
            } else {
                self.other = self.other.saturating_add(1);
            }
        }
    }
    fn value(&mut self, value: &Value) {
        if !value.is_null() {
            self.text(&value.to_string());
        }
    }
    fn tokens(self) -> u64 {
        self.ascii.div_ceil(4).saturating_add(self.other)
    }
}
fn injected(history: &[HistoryItem], index: usize, key: &str) -> Option<Value> {
    let HistoryItem::UserText(text) = history.get(index)? else {
        return None;
    };
    let value: Value = serde_json::from_str(text).ok()?;
    let fields = value.as_object().filter(|fields| fields.len() == 1)?;
    let data = fields.get(key)?.as_object()?;
    let valid = match key {
        "saya_runtime_context_v1" => {
            ["workspace", "profile", "model"]
                .iter()
                .all(|key| data.get(*key).is_some_and(Value::is_string))
                && data.get("protocol").is_some_and(|value| {
                    serde_json::from_value::<ModelProtocol>(value.clone()).is_ok()
                })
                && data
                    .get("work_mode")
                    .is_some_and(|value| serde_json::from_value::<WorkMode>(value.clone()).is_ok())
                && data.get("permission_mode").is_some_and(|value| {
                    serde_json::from_value::<PermissionMode>(value.clone()).is_ok()
                })
        }
        "saya_project_context_v1" => ["instructions", "skills"]
            .iter()
            .all(|key| data.get(*key).is_some_and(Value::is_array)),
        "saya_personal_context_v1" => ["preferences", "memory"].iter().all(|key| {
            data.get(*key)
                .is_some_and(|value| value.is_null() || value.is_string())
        }),
        _ => false,
    };
    valid.then(|| Value::Object(data.clone()))
}

fn snapshot(request: &ModelRequest, sources: ContextSources) -> ContextSnapshot {
    let mut counts = [Estimate::default(); LABELS.len()];
    counts[SYSTEM].text(&request.instructions);
    if let Some(dynamic) = &request.dynamic_instructions {
        counts[SYSTEM].text(dynamic);
    }
    let mut first = 0;
    let mut skills_count = None;
    // Each expected host prefix must be present consecutively. Stop at the
    // first mismatch so later user JSON can never be promoted into host data.
    if injected(&request.history, first, "saya_runtime_context_v1").is_some() {
        if let HistoryItem::UserText(text) = &request.history[first] {
            counts[SYSTEM].text(text);
        }
        first += 1;
        let mut complete = true;
        if sources.project {
            if let Some(project) = injected(&request.history, first, "saya_project_context_v1") {
                if let Some(instructions) = project.get("instructions").and_then(Value::as_array) {
                    for instruction in instructions {
                        counts[INSTRUCTIONS].value(instruction);
                    }
                }
                if let Some(skills) = project.get("skills").and_then(Value::as_array) {
                    for skill in skills {
                        counts[SKILLS].value(skill);
                    }
                    skills_count = Some(skills.len());
                }
                first += 1;
            } else {
                complete = false;
            }
        }
        if complete
            && sources.personal
            && let Some(personal) = injected(&request.history, first, "saya_personal_context_v1")
        {
            if let Some(preferences) = personal.get("preferences") {
                counts[PREFERENCES].value(preferences);
            }
            if let Some(memory) = personal.get("memory") {
                counts[MEMORY].value(memory);
            }
            first += 1;
        }
    }
    let mut calls = HashMap::new();
    for (index, item) in request.history.iter().enumerate().skip(first) {
        match item {
            HistoryItem::UserText(text)
            | HistoryItem::AssistantText(text)
            | HistoryItem::AssistantPartial(text) => counts[CONVERSATION].text(text),
            HistoryItem::ToolCall(call) => {
                counts[TOOLS].text(&call.call_id);
                counts[TOOLS].text(&call.name);
                counts[TOOLS].text(&call.arguments);
                calls.insert(
                    call.call_id.as_str(),
                    matches!(
                        call.name.as_str(),
                        "read_file" | "list_dir" | "find_files" | "search_text"
                    ),
                );
            }
            HistoryItem::ToolResult { call_id, result } => {
                let category = if calls.remove(call_id.as_str()).unwrap_or(false) {
                    FILES
                } else {
                    TOOLS
                };
                counts[category].text(call_id);
                counts[category].text(if result.ok { "true" } else { "false" });
                counts[category].text(if result.truncated { "true" } else { "false" });
                if let Some(data) = &result.data {
                    counts[category].value(data);
                }
                if let Some(error) = &result.error {
                    counts[category].text(&error.code);
                    counts[category].text(&error.message);
                }
            }
            HistoryItem::ProviderItem { protocol, value } => {
                counts[PROTOCOL].value(&protocol_metadata(
                    *protocol,
                    value,
                    request.history.get(index + 1),
                ));
            }
        }
    }
    for tool in &request.tools {
        counts[TOOLS].text(&tool.name);
        counts[TOOLS].text(&tool.description);
        counts[TOOLS].value(&tool.input_schema);
    }
    ContextSnapshot {
        entries: LABELS
            .iter()
            .zip(counts)
            .map(|(label, estimate)| ContextEntry {
                label,
                tokens: estimate.tokens(),
            })
            .collect(),
        skills_count,
    }
}

fn protocol_metadata(protocol: ModelProtocol, value: &Value, next: Option<&HistoryItem>) -> Value {
    let mut metadata = value.clone();
    match (protocol, value.get("type").and_then(Value::as_str), next) {
        (
            ModelProtocol::AnthropicMessages,
            Some("text"),
            Some(HistoryItem::AssistantText(text)),
        ) if value["text"].as_str() == Some(text) => {
            if let Some(fields) = metadata.as_object_mut() {
                fields.remove("text");
            }
        }
        (ModelProtocol::Responses, Some("message"), Some(HistoryItem::AssistantText(text))) => {
            let content = value["content"].as_array().filter(|parts| {
                parts
                    .iter()
                    .all(|part| part["type"] == "output_text" && part["text"].is_string())
            });
            let visible: Option<String> = content.map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect()
            });
            if value["role"] == "assistant"
                && visible.as_deref() == Some(text.as_str())
                && let Some(parts) = metadata["content"].as_array_mut()
            {
                for part in parts {
                    if let Some(fields) = part.as_object_mut() {
                        fields.remove("text");
                    }
                }
            }
        }
        (ModelProtocol::Responses, Some("function_call"), Some(HistoryItem::ToolCall(call)))
            if value["call_id"].as_str() == Some(call.call_id.as_str())
                && value["name"].as_str() == Some(call.name.as_str())
                && value["arguments"].as_str() == Some(call.arguments.as_str()) =>
        {
            if let Some(fields) = metadata.as_object_mut() {
                for key in ["call_id", "name", "arguments"] {
                    fields.remove(key);
                }
            }
        }
        (ModelProtocol::AnthropicMessages, Some("tool_use"), Some(HistoryItem::ToolCall(call)))
            if value["id"].as_str() == Some(call.call_id.as_str())
                && value["name"].as_str() == Some(call.name.as_str())
                && value
                    .get("input")
                    .filter(|input| input.is_object())
                    .is_some_and(|input| {
                        serde_json::from_str::<Value>(&call.arguments)
                            .is_ok_and(|arguments| arguments == *input)
                    }) =>
        {
            if let Some(fields) = metadata.as_object_mut() {
                for key in ["id", "name", "input"] {
                    fields.remove(key);
                }
            }
        }
        _ => {}
    }
    metadata
}
