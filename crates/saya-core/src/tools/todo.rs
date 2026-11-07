//! Session task metadata, derived from persisted tool calls and their results.
use crate::{
    boundary::{Cancellation, Operation, Tool, ToolContext, ToolDefinition},
    contract::{BoundaryError, ErrorInfo, HistoryItem, ToolCall, ToolResult},
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_TODOS: usize = 64;
const MAX_ID_CHARS: usize = 64;
const MAX_CONTENT_CHARS: usize = 500;

/// Recorded progress for one task. Completion remains a model assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// One bounded session task, with a stable ID across full-list replacements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoList {
    todos: Vec<TodoItem>,
}

fn invalid() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "invalid_args".into(),
            message: "todos require at most 64 unique IDs, nonempty bounded content, and at most one in_progress item".into(),
        },
        None,
        None,
    )
}

fn valid(todos: &[TodoItem]) -> bool {
    let mut ids = HashSet::new();
    todos.len() <= MAX_TODOS
        && todos.iter().all(|todo| {
            !todo.id.trim().is_empty()
                && todo.id.chars().count() <= MAX_ID_CHARS
                && ids.insert(todo.id.as_str())
                && !todo.content.trim().is_empty()
                && todo.content.chars().count() <= MAX_CONTENT_CHARS
        })
        && todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::InProgress)
            .count()
            <= 1
}

pub(crate) fn parse(call: &ToolCall) -> Result<Vec<TodoItem>, BoundaryError> {
    if call.name != "todo_write" || call.arguments.len() > MAX_INPUT_BYTES {
        return Err(invalid());
    }
    let list: TodoList = serde_json::from_str(&call.arguments).map_err(|_| invalid())?;
    if !valid(&list.todos) {
        return Err(invalid());
    }
    Ok(list.todos)
}

/// Read a complete successful task snapshot from a trusted persisted tool event.
/// Untrusted history must use `current_todos` to verify its matching call too.
pub fn result_todos(result: &ToolResult) -> Option<Vec<TodoItem>> {
    if !result.ok || result.error.is_some() || result.truncated {
        return None;
    }
    let data = result.data.as_ref()?.as_object()?;
    if data.len() != 1 {
        return None;
    }
    let values = data.get("todos")?.as_array()?;
    if values.len() > MAX_TODOS
        || values.iter().any(|value| {
            value.as_object().is_none_or(|item| item.len() != 3)
                || value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|id| id.len() > MAX_ID_CHARS * 4)
                || value
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|content| content.len() > MAX_CONTENT_CHARS * 4)
                || !matches!(
                    value.get("status").and_then(serde_json::Value::as_str),
                    Some("pending" | "in_progress" | "completed")
                )
        })
    {
        return None;
    }
    if serde_json::to_vec(result.data.as_ref()?).ok()?.len() > MAX_INPUT_BYTES {
        return None;
    }
    let list: TodoList = serde_json::from_value(result.data.clone()?).ok()?;
    valid(&list.todos).then_some(list.todos)
}

/// Derive the most recent successful full-list replacement. Only a preceding,
/// matching call and one complete successful result can change the snapshot.
/// User text, failed calls, duplicate results, and malformed payloads are ignored.
/// Results are canonical after credential redaction and may differ from input.
pub fn current_todos(history: &[HistoryItem]) -> Vec<TodoItem> {
    let mut pending = HashMap::new();
    let mut seen = HashSet::new();
    let mut current = Vec::new();
    for item in history {
        match item {
            HistoryItem::ToolCall(call) => {
                pending.remove(call.call_id.as_str());
                if seen.insert(call.call_id.as_str())
                    && !call.call_id.is_empty()
                    && call.name == "todo_write"
                {
                    pending.insert(call.call_id.as_str(), call);
                }
            }
            HistoryItem::ToolResult { call_id, result } => {
                if let Some(call) = pending.remove(call_id.as_str())
                    && let Some(todos) = result_todos(result)
                    && parse(call).is_ok()
                {
                    current = todos;
                }
            }
            _ => {}
        }
    }
    current
}

/// Validate and return a full-list replacement. Authorization and persistence
/// are supplied by the dispatcher; this adapter performs no workspace I/O.
pub struct TodoTool;
impl Tool for TodoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "todo_write".into(),
            description: "Replace the session task list for a multi-step task. Preserve stable IDs, use pending, in_progress, or completed, and keep at most one item in_progress. Supply the whole current list; [] clears it. This records progress metadata and does not execute the tasks or finish the run.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "maxItems": MAX_TODOS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "minLength": 1, "maxLength": MAX_ID_CHARS},
                                "content": {"type": "string", "minLength": 1, "maxLength": MAX_CONTENT_CHARS},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                            },
                            "required": ["id", "content", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["todos"],
                "additionalProperties": false
            }),
        }
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(BoundaryError(
                    ErrorInfo {
                        code: "cancelled".into(),
                        message: "todo update cancelled".into(),
                    },
                    None,
                    None,
                ));
            }
            let todos = parse(call)?;
            Ok(ToolResult {
                ok: true,
                data: Some(serde_json::json!({"todos": todos})),
                error: None,
                truncated: false,
            })
        })
    }
}
