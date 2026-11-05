//! Controlled, test-only loop; it must not dispatch real workspace tools.
//! Full cancellation, deadlines, permissions and persistence are later tasks.

use crate::boundary::{
    Cancellation, Model, ModelRequest, ModelUpdate, Store, StoreChange, Tool, ToolContext,
};
use crate::contract::{
    BoundaryError, ErrorInfo, HistoryItem, ModelPurpose, Run, Session, ToolCall, ToolResult,
};
use std::collections::BTreeMap;

fn error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(ErrorInfo {
        code: code.into(),
        message: message.into(),
    })
}
fn failed(code: &str, message: &str) -> ToolResult {
    ToolResult {
        ok: false,
        data: None,
        error: Some(ErrorInfo {
            code: code.into(),
            message: message.into(),
        }),
        truncated: false,
    }
}

#[derive(Default)]
struct ToolRegistry(BTreeMap<String, Box<dyn Tool>>);
impl ToolRegistry {
    fn register(&mut self, tool: Box<dyn Tool>) -> Result<(), BoundaryError> {
        let name = tool.definition().name;
        if name.is_empty() || self.0.contains_key(&name) {
            return Err(error(
                "duplicate_tool",
                "tool name is empty or already registered",
            ));
        }
        self.0.insert(name, tool);
        Ok(())
    }
}

/// In-memory prototype. The caller supplies fake adapters only; never exposed by CLI.
async fn run(
    session: &mut Session,
    run: &Run,
    model: &dyn Model,
    tools: &ToolRegistry,
    store: &dyn Store,
    cancel: &dyn Cancellation,
) -> Result<String, BoundaryError> {
    if session.id != run.session_id {
        return Err(error("invalid_run", "session ID mismatch"));
    }
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    let mut tool_count = 0usize;
    for request_number in 0..64 {
        let request = ModelRequest {
            request_id: format!("request-{request_number}"),
            purpose: ModelPurpose::Execution,
            profile: session.profile.clone(),
            instructions: String::new(),
            history: session.history.clone(),
            tools: tools.0.values().map(|tool| tool.definition()).collect(),
        };
        let mut stream = model.start(&request, cancel).await?;
        let items = loop {
            match stream.next(cancel).await? {
                Some(ModelUpdate::TextDelta(_)) => {}
                Some(ModelUpdate::Finished { items, .. }) => break items,
                None => {
                    return Err(error(
                        "incomplete_model_response",
                        "stream ended without a complete turn",
                    ));
                }
            }
        };
        let changes: Vec<_> = items
            .iter()
            .cloned()
            .map(|item| StoreChange::History {
                session_id: session.id,
                item,
            })
            .collect();
        store.commit(&changes).await?;
        session.history.extend(items.iter().cloned());
        let calls: Vec<ToolCall> = items
            .iter()
            .filter_map(|item| match item {
                HistoryItem::ToolCall(call) => Some(call.clone()),
                _ => None,
            })
            .collect();
        if calls.is_empty() {
            return Ok(items
                .iter()
                .filter_map(|item| match item {
                    HistoryItem::AssistantText(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect());
        }
        let context = ToolContext {
            run: run.clone(),
            workspace: session.workspace.clone(),
        };
        for call in calls {
            tool_count += 1;
            if tool_count > 256 {
                return Err(error("limit_reached", "tool call budget exceeded"));
            }
            store
                .commit(&[StoreChange::ToolIntent {
                    run_id: run.id,
                    call: call.clone(),
                }])
                .await?;
            let valid_object = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .is_ok_and(|value| value.is_object());
            let outcome = if !valid_object {
                failed("invalid_args", "tool arguments must be a JSON object")
            } else if let Some(tool) = tools.0.get(&call.name) {
                match tool.execute(&call, &context, cancel).await {
                    Ok(result) => result,
                    Err(err) => ToolResult {
                        ok: false,
                        data: None,
                        error: Some(err.0),
                        truncated: false,
                    },
                }
            } else {
                failed("unknown_tool", "tool is not registered")
            };
            let item = HistoryItem::ToolResult {
                call_id: call.call_id.clone(),
                result: outcome.clone(),
            };
            store
                .commit(&[
                    StoreChange::ToolFinished {
                        run_id: run.id,
                        call_id: call.call_id.clone(),
                        result: outcome,
                    },
                    StoreChange::History {
                        session_id: session.id,
                        item: item.clone(),
                    },
                ])
                .await?;
            session.history.push(item);
        }
    }
    Err(error("limit_reached", "model request budget exceeded"))
}

#[cfg(test)]
mod tests;
