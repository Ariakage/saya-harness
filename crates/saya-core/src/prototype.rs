//! Controlled, test-only loop; it must not dispatch real workspace tools.
//! Permissions and durable persistence are not implemented.

use crate::boundary::{
    Cancellation, Model, ModelRequest, ModelUpdate, Store, StoreChange, Tool, ToolContext,
};
use crate::contract::{
    BoundaryError, ErrorInfo, HistoryItem, ModelPurpose, Run, Session, ToolCall, ToolResult,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;
use tokio::time::{Instant, sleep};
mod control;
use control::{Budget, Limits, retry_delay};

fn error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
    )
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

fn jitter(max: Duration) -> Duration {
    let upper = max.as_millis().min(u128::from(u64::MAX)) as u64;
    if upper == 0 {
        return Duration::ZERO;
    }
    let sample = uuid::Uuid::now_v7().as_u128() as u64;
    Duration::from_millis(sample % upper.saturating_add(1))
}

async fn bounded<T>(
    operation: impl Future<Output = Result<T, BoundaryError>>,
    cancel: &dyn Cancellation,
    active: Duration,
    timeout: Duration,
    timeout_code: &'static str,
) -> Result<T, BoundaryError> {
    if cancel.is_cancelled() {
        return Err(error("cancelled", "run cancelled"));
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(error("cancelled", "run cancelled")),
        _ = sleep(active) => Err(error("limit_reached", "active time budget exceeded")),
        _ = sleep(timeout) => Err(error(timeout_code, "operation timed out")),
        result = operation => result,
    }
}

struct ChildCancellation(tokio::sync::watch::Sender<bool>);
impl ChildCancellation {
    fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
    fn cancel(&self) {
        self.0.send_replace(true);
    }
}
impl Cancellation for ChildCancellation {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut receiver = self.0.subscribe();
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}

async fn tool_call(
    tool: &dyn Tool,
    call: &ToolCall,
    context: &ToolContext,
    cancel: &dyn Cancellation,
    active: Duration,
    timeout: Duration,
) -> Result<ToolResult, BoundaryError> {
    if cancel.is_cancelled() {
        return Err(error("cancelled", "run cancelled"));
    }
    let child = ChildCancellation::new();
    let operation = tool.execute(call, context, &child);
    tokio::pin!(operation);
    let interrupted = tokio::select! {
        biased;
        _ = cancel.cancelled() => error("cancelled", "run cancelled"),
        _ = sleep(active) => error("limit_reached", "active time budget exceeded"),
        _ = sleep(timeout) => error("tool_timeout", "tool timed out"),
        result = &mut operation => return result,
    };
    child.cancel();
    // The prototype has no child-process handle; a real tool must implement
    // forceful process-group cleanup after this cooperative grace period.
    let _ = tokio::time::timeout(Duration::from_secs(5), &mut operation).await;
    Err(interrupted)
}

async fn model_turn(
    session: &mut Session,
    model: &dyn Model,
    tools: &ToolRegistry,
    store: &dyn Store,
    cancel: &dyn Cancellation,
    budget: &mut Budget,
    limits: &Limits,
) -> Result<Vec<HistoryItem>, BoundaryError> {
    for attempt in 0..=2 {
        if cancel.is_cancelled() {
            return Err(error("cancelled", "run cancelled"));
        }
        if !budget.request() {
            return Err(error("limit_reached", "model request budget exceeded"));
        }
        let request = ModelRequest {
            request_id: uuid::Uuid::now_v7().to_string(),
            purpose: ModelPurpose::Execution,
            profile: session.profile.clone(),
            instructions: String::new(),
            history: session.history.clone(),
            tools: tools.0.values().map(|tool| tool.definition()).collect(),
        };
        let started = Instant::now();
        let mut partial = String::new();
        let mut received = false;
        let response: Result<Vec<HistoryItem>, BoundaryError> = async {
            let mut stream = bounded(
                model.start(&request, cancel),
                cancel,
                budget.remaining(),
                limits.model_connect.min(limits.model_total),
                "model_connect_timeout",
            )
            .await?;
            loop {
                let remaining = limits.model_total.saturating_sub(started.elapsed());
                let update = bounded(
                    stream.next(cancel),
                    cancel,
                    budget.remaining(),
                    limits.model_idle.min(remaining),
                    "model_timeout",
                )
                .await?;
                match update {
                    Some(ModelUpdate::TextDelta(text)) => {
                        received = true;
                        partial.push_str(&text);
                    }
                    Some(ModelUpdate::Finished { items, .. }) => return Ok(items),
                    None => {
                        return Err(error(
                            "incomplete_model_response",
                            "stream ended without a complete turn",
                        ));
                    }
                }
            }
        }
        .await;
        match response {
            Ok(items) => return Ok(items),
            Err(err) => {
                if received {
                    let item = HistoryItem::AssistantPartial(partial);
                    store
                        .commit(&[StoreChange::History {
                            session_id: session.id,
                            item: item.clone(),
                        }])
                        .await?;
                    session.history.push(item);
                }
                if let Some(delay) = retry_delay(
                    &err.0.code,
                    attempt,
                    received,
                    err.1,
                    jitter(limits.retry_jitter_max),
                ) {
                    bounded(
                        async {
                            sleep(delay).await;
                            Ok(())
                        },
                        cancel,
                        budget.remaining(),
                        limits.active_time,
                        "retry_timeout",
                    )
                    .await?;
                } else {
                    return Err(err);
                }
            }
        }
    }
    Err(error("model_error", "retry attempts exhausted"))
}

/// Controlled prototype; no real workspace tools are reachable through the CLI.
async fn run(
    session: &mut Session,
    run: &Run,
    model: &dyn Model,
    tools: &ToolRegistry,
    store: &dyn Store,
    cancel: &dyn Cancellation,
) -> Result<String, BoundaryError> {
    run_with_limits(session, run, model, tools, store, cancel, Limits::default()).await
}

async fn run_with_limits(
    session: &mut Session,
    run: &Run,
    model: &dyn Model,
    tools: &ToolRegistry,
    store: &dyn Store,
    cancel: &dyn Cancellation,
    limits: Limits,
) -> Result<String, BoundaryError> {
    if session.id != run.session_id {
        return Err(error("invalid_run", "session ID mismatch"));
    }
    if cancel.is_cancelled() {
        return Err(error("cancelled", "run cancelled"));
    }
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    let mut budget = Budget::new(limits.clone());
    let mut last_failure: Option<(String, serde_json::Value, u8)> = None;
    loop {
        let items = model_turn(session, model, tools, store, cancel, &mut budget, &limits).await?;
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
            if cancel.is_cancelled() {
                return Err(error("cancelled", "run cancelled"));
            }
            if !budget.tool() {
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
                match tool_call(
                    tool.as_ref(),
                    &call,
                    &context,
                    cancel,
                    budget.remaining(),
                    limits.tool_timeout,
                )
                .await
                {
                    Ok(result) => result,
                    Err(err)
                        if matches!(
                            err.0.code.as_str(),
                            "cancelled" | "limit_reached" | "tool_timeout"
                        ) =>
                    {
                        store
                            .commit(&[StoreChange::ToolState {
                                run_id: run.id,
                                call_id: call.call_id.clone(),
                                state: crate::contract::ToolState::Unknown,
                            }])
                            .await?;
                        return Err(err);
                    }
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
            let failed = !outcome.ok;
            let normalized = serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_| serde_json::Value::String(call.arguments.clone()));
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
            if failed {
                let count = match &last_failure {
                    Some((name, args, count)) if name == &call.name && args == &normalized => {
                        count.saturating_add(1)
                    }
                    _ => 1,
                };
                last_failure = Some((call.name.clone(), normalized, count));
                if count >= 3 {
                    return Err(error(
                        "repeated_tool_failure",
                        "same tool and arguments failed three times",
                    ));
                }
            } else {
                last_failure = None;
            }
        }
    }
}

#[cfg(test)]
mod tests;

/// Test-only result mapping. The error and saved partial output remain visible.
async fn run_outcome(
    session: &mut Session,
    run_id: &Run,
    model: &dyn Model,
    tools: &ToolRegistry,
    store: &dyn Store,
    cancel: &dyn Cancellation,
    limits: Limits,
) -> crate::contract::RunResult {
    use crate::contract::{Persistence, RunResult, RunStatus, SCHEMA_VERSION};
    let initial_history_len = session.history.len();
    let outcome = run_with_limits(session, run_id, model, tools, store, cancel, limits).await;
    let (status, stop_reason, text, error) = match outcome {
        Ok(text) => (
            RunStatus::Completed,
            Some("completed".into()),
            Some(text),
            None,
        ),
        Err(err) => {
            let status = match err.0.code.as_str() {
                "cancelled" => RunStatus::Cancelled,
                "limit_reached" => RunStatus::LimitReached,
                _ => RunStatus::Failed,
            };
            let text = session.history[initial_history_len..]
                .iter()
                .rev()
                .find_map(|item| match item {
                    HistoryItem::AssistantPartial(text) => Some(text.clone()),
                    _ => None,
                });
            (status, Some(err.0.code.clone()), text, Some(err.0))
        }
    };
    let mut result = RunResult {
        schema_version: SCHEMA_VERSION,
        session_id: Some(session.id),
        run_id: Some(run_id.id),
        status,
        stop_reason,
        text,
        error,
        usage: None,
        artifacts: vec![],
        persistence: Some(Persistence {
            saved: true,
            error: None,
        }),
    };
    if result.status == RunStatus::Cancelled
        && matches!(store.load_session(session.id).await, Ok(None))
    {
        result.persistence = Some(Persistence {
            saved: false,
            error: None,
        });
        return result;
    }
    if let Err(err) = store
        .commit(&[StoreChange::RunFinished(result.clone())])
        .await
    {
        result.status = RunStatus::Failed;
        result.stop_reason = Some("storage_error".into());
        result.error = Some(err.0.clone());
        result.persistence = Some(Persistence {
            saved: false,
            error: Some(err.0),
        });
    }
    result
}
