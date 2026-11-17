use super::{MAX_SERIALIZED_BYTES, counting, error};
use crate::contract::{BoundaryError, HistoryItem, ModelProtocol, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Half-open ordinals in the original source history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemRange {
    pub start: usize,
    pub end: usize,
}
impl ItemRange {
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtomicGroup {
    pub range: ItemRange,
    pub closed: bool,
    pub contains_provider_items: bool,
    pub call_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grouping {
    pub groups: Vec<AtomicGroup>,
    pub latest_user_index: Option<usize>,
    pub open_call_ids: Vec<String>,
}
impl Grouping {
    /// A compactable prefix ends at a closed group before any explicit pin.
    /// When it crosses the latest user, the builder replays that original user
    /// separately. Crossing only that user adds no compactable material.
    pub fn is_safe_prefix(&self, end: usize, protected: &[ItemRange]) -> bool {
        if end == 0 {
            return true;
        }
        self.latest_user_index
            .is_some_and(|latest| end <= latest || end > latest + 1)
            && self
                .groups
                .iter()
                .any(|group| group.closed && group.range.end == end)
            && !self.groups.iter().any(|group| {
                group.range.start < end && protected.iter().any(|range| range.overlaps(group.range))
            })
    }

    pub fn safe_prefix_ends(&self, protected: &[ItemRange]) -> Vec<usize> {
        let Some(latest) = self.latest_user_index else {
            return Vec::new();
        };
        let history_end = self.groups.last().map_or(0, |group| group.range.end);
        let first_pinned = self
            .groups
            .iter()
            .find(|group| protected.iter().any(|range| range.overlaps(group.range)))
            .map_or(history_end, |group| group.range.start);
        self.groups
            .iter()
            .filter(|group| {
                group.closed
                    && group.range.end <= first_pinned
                    && (group.range.end <= latest || group.range.end > latest + 1)
            })
            .map(|group| group.range.end)
            .collect()
    }
}

struct PendingGroup {
    start: usize,
    end: usize,
    provider: bool,
    call_ids: Vec<String>,
    results_started: bool,
}
impl PendingGroup {
    fn finish(self, closed: bool) -> AtomicGroup {
        AtomicGroup {
            range: ItemRange {
                start: self.start,
                end: self.end,
            },
            closed,
            contains_provider_items: self.provider,
            call_ids: self.call_ids,
        }
    }
}

/// Validate replay structure and expose safe groups without modifying any item.
/// Typed provider items and their exact semantic mirrors are consumed together.
/// Opaque reasoning/signatures remain in their original assistant/tool group.
/// An unfinished final tool group is returned as open, never a safe cut.
pub fn group_history(
    protocol: ModelProtocol,
    items: &[HistoryItem],
) -> Result<Grouping, BoundaryError> {
    counting::validate_history(items)?;
    counting::measure(&items, MAX_SERIALIZED_BYTES)?;
    let mut groups = Vec::new();
    let mut active: Option<PendingGroup> = None;
    let mut pending = BTreeSet::new();
    let mut latest_user_index = None;
    let mut index = 0usize;
    while index < items.len() {
        if let HistoryItem::UserText(_) = &items[index] {
            if !pending.is_empty() {
                return Err(error(
                    "invalid_history",
                    "User message splits an unfinished tool group",
                ));
            }
            if let Some(group) = active.take() {
                groups.push(group.finish(true));
            }
            groups.push(AtomicGroup {
                range: ItemRange {
                    start: index,
                    end: index + 1,
                },
                closed: true,
                contains_provider_items: false,
                call_ids: Vec::new(),
            });
            latest_user_index = Some(index);
            index += 1;
            continue;
        }
        let group = active.get_or_insert_with(|| PendingGroup {
            start: index,
            end: index,
            provider: false,
            call_ids: Vec::new(),
            results_started: false,
        });
        let mut consumed = 1usize;
        match &items[index] {
            HistoryItem::ToolResult { call_id, .. } => {
                if !pending.remove(call_id) {
                    return Err(error(
                        "invalid_history",
                        "Tool result has no matching pending call",
                    ));
                }
                group.results_started = true;
            }
            HistoryItem::ProviderItem {
                protocol: item_protocol,
                value,
            } => {
                if *item_protocol != protocol
                    || group.results_started
                    || (protocol == ModelProtocol::ChatCompletions && !pending.is_empty())
                {
                    return Err(error(
                        "invalid_history",
                        "Provider item is incompatible with its replay group",
                    ));
                }
                group.provider = true;
                if let Some(semantic) = provider_semantic(protocol, value, items.get(index + 1))? {
                    if items.get(index + 1) != Some(&semantic) {
                        return Err(error(
                            "invalid_history",
                            "Provider item and local semantic mirror differ",
                        ));
                    }
                    if let HistoryItem::ToolCall(call) = semantic {
                        add_call(&call, &mut pending, group)?;
                    }
                    consumed = 2;
                }
            }
            HistoryItem::ToolCall(call) => {
                if group.results_started {
                    return Err(error(
                        "invalid_history",
                        "Tool call splits an unfinished result group",
                    ));
                }
                if protocol == ModelProtocol::AnthropicMessages
                    && !serde_json::from_str::<Value>(&call.arguments)
                        .is_ok_and(|value| value.is_object())
                {
                    return Err(error(
                        "invalid_history",
                        "Anthropic tool arguments are not a JSON object",
                    ));
                }
                add_call(call, &mut pending, group)?;
            }
            HistoryItem::AssistantText(_) | HistoryItem::AssistantPartial(_) => {
                let ignored_partial = protocol == ModelProtocol::ChatCompletions
                    && matches!(&items[index], HistoryItem::AssistantPartial(text) if text.is_empty());
                if !ignored_partial
                    && !pending.is_empty()
                    && (group.results_started || protocol != ModelProtocol::AnthropicMessages)
                {
                    return Err(error(
                        "invalid_history",
                        "Assistant text splits an unfinished tool group",
                    ));
                }
            }
            HistoryItem::UserText(_) => {
                return Err(error(
                    "invalid_history",
                    "Unexpected user item in assistant group",
                ));
            }
        }
        index += consumed;
        group.end = index;
        if group.results_started
            && pending.is_empty()
            && let Some(group) = active.take()
        {
            groups.push(group.finish(true));
        }
    }
    if let Some(group) = active {
        groups.push(group.finish(pending.is_empty()));
    }
    Ok(Grouping {
        groups,
        latest_user_index,
        open_call_ids: pending.into_iter().collect(),
    })
}

fn add_call(
    call: &ToolCall,
    pending: &mut BTreeSet<String>,
    group: &mut PendingGroup,
) -> Result<(), BoundaryError> {
    if call.call_id.is_empty() || call.name.is_empty() || !pending.insert(call.call_id.clone()) {
        return Err(error(
            "invalid_history",
            "Tool call identity is empty or duplicated",
        ));
    }
    group.call_ids.push(call.call_id.clone());
    Ok(())
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, BoundaryError> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| {
        error(
            "invalid_history",
            "Provider replay field is missing or invalid",
        )
    })
}

fn provider_semantic(
    protocol: ModelProtocol,
    value: &Value,
    next: Option<&HistoryItem>,
) -> Result<Option<HistoryItem>, BoundaryError> {
    if !value.is_object() {
        return Err(error(
            "invalid_history",
            "Provider replay item is not an object",
        ));
    }
    if protocol == ModelProtocol::ChatCompletions {
        // Chat's retained reasoning is an adjunct, not a separately replayed
        // message. Bind it to the following complete assistant message/calls.
        let valid_next = matches!(next, Some(HistoryItem::AssistantText(text)) if !text.is_empty())
            || matches!(next, Some(HistoryItem::ToolCall(_)));
        if value.as_object().is_none_or(|object| object.len() != 1)
            || string(value, "reasoning_content")?.len() > 16 * 1024 * 1024
            || !valid_next
        {
            return Err(error(
                "invalid_history",
                "Chat reasoning lacks its complete assistant output",
            ));
        }
        return Ok(None);
    }
    let kind = string(value, "type")?;
    match (protocol, kind) {
        (ModelProtocol::Responses, "reasoning") => Ok(None),
        (ModelProtocol::Responses, "message") => {
            completed(value)?;
            if string(value, "role")? != "assistant" {
                return Err(error("invalid_history", "Response message role is invalid"));
            }
            let parts = value
                .get("content")
                .and_then(Value::as_array)
                .ok_or_else(|| error("invalid_history", "Response message content is invalid"))?;
            let mut text = String::new();
            for part in parts {
                if string(part, "type")? != "output_text" {
                    return Err(error(
                        "invalid_history",
                        "Response content type is unsupported",
                    ));
                }
                text.push_str(string(part, "text")?);
            }
            Ok(Some(HistoryItem::AssistantText(text)))
        }
        (ModelProtocol::Responses, "function_call") => {
            completed(value)?;
            let arguments = string(value, "arguments")?;
            if !serde_json::from_str::<Value>(arguments).is_ok_and(|value| value.is_object()) {
                return Err(error(
                    "invalid_history",
                    "Response tool arguments are not a JSON object",
                ));
            }
            Ok(Some(HistoryItem::ToolCall(ToolCall {
                call_id: string(value, "call_id")?.into(),
                name: string(value, "name")?.into(),
                arguments: arguments.into(),
            })))
        }
        (ModelProtocol::AnthropicMessages, "text") => Ok(Some(HistoryItem::AssistantText(
            string(value, "text")?.into(),
        ))),
        (ModelProtocol::AnthropicMessages, "tool_use") => {
            let input = value
                .get("input")
                .filter(|value| value.is_object())
                .ok_or_else(|| error("invalid_history", "Anthropic tool input is not an object"))?;
            Ok(Some(HistoryItem::ToolCall(ToolCall {
                call_id: string(value, "id")?.into(),
                name: string(value, "name")?.into(),
                arguments: input.to_string(),
            })))
        }
        (ModelProtocol::AnthropicMessages, "thinking") => {
            string(value, "thinking")?;
            if string(value, "signature")?.is_empty() {
                return Err(error(
                    "invalid_history",
                    "Anthropic thinking signature is missing",
                ));
            }
            Ok(None)
        }
        (ModelProtocol::AnthropicMessages, "redacted_thinking") => {
            if string(value, "data")?.is_empty() {
                return Err(error(
                    "invalid_history",
                    "Anthropic redacted thinking is incomplete",
                ));
            }
            Ok(None)
        }
        _ => Err(error(
            "invalid_history",
            "Provider replay item type is unsupported",
        )),
    }
}

fn completed(value: &Value) -> Result<(), BoundaryError> {
    if value
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|value| value != "completed")
    {
        return Err(error(
            "invalid_history",
            "Response replay item is incomplete",
        ));
    }
    Ok(())
}
