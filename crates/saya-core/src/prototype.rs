//! Controlled, test-only loop; it must not dispatch real workspace tools.
//! Permissions and durable persistence are not implemented.

use crate::boundary::{
    Cancellation, Model, ModelRequest, ModelUpdate, Store, StoreChange, Tool, ToolContext,
};
use crate::contract::{
    BoundaryError, ErrorInfo, HistoryItem, ModelPurpose, Run, Session, ToolCall, ToolResult,
    WorkMode,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;
use tokio::time::{Instant, sleep};
mod control;
use crate::mode::{ModeController as Modes, ModeSnapshot};
use control::{Budget, Limits, retry_delay};

fn error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolClass {
    Read,
    Write,
    Command,
    AskUser,
}
struct RegisteredTool {
    tool: Box<dyn Tool>,
    class: ToolClass,
}
#[derive(Default)]
struct ToolRegistry(BTreeMap<String, RegisteredTool>);
impl ToolRegistry {
    fn register(&mut self, tool: Box<dyn Tool>) -> Result<(), BoundaryError> {
        self.register_as(tool, ToolClass::Write)
    }
    fn register_as(&mut self, tool: Box<dyn Tool>, class: ToolClass) -> Result<(), BoundaryError> {
        let name = tool.definition().name;
        if name.is_empty() || self.0.contains_key(&name) {
            return Err(error(
                "duplicate_tool",
                "tool name is empty or already registered",
            ));
        }
        self.0.insert(name, RegisteredTool { tool, class });
        Ok(())
    }
}
fn allowed(mode: WorkMode, class: ToolClass) -> bool {
    mode != WorkMode::Plan || matches!(class, ToolClass::Read | ToolClass::AskUser)
}
fn mode_instruction(mode: WorkMode) -> &'static str {
    match mode {
        WorkMode::Plan => {
            "Investigate read-only, clarify requirements, and produce a plan and acceptance criteria."
        }
        WorkMode::Build => {
            "Implement and deliver the task end-to-end, including appropriate verification."
        }
        WorkMode::Code => {
            "Focus on writing, changing, and debugging code with appropriate verification."
        }
        WorkMode::Work => "Complete the general task and explain results and outstanding work.",
    }
}
struct Scheduler {
    modes: Modes,
    limits: Limits,
    shared_budget: Option<crate::review::SharedBudget>,
    background: Option<std::sync::Arc<crate::background::BackgroundJobs>>,
}
fn check_background(scheduler: &Scheduler) -> Result<(), BoundaryError> {
    if let Some(jobs) = &scheduler.background
        && let Some(info) = jobs.fatal_error()?
    {
        return Err(BoundaryError(info, None, None));
    }
    Ok(())
}
async fn sync_mode(
    session: &mut Session,
    store: &dyn Store,
    modes: &Modes,
) -> Result<ModeSnapshot, BoundaryError> {
    loop {
        let snap = modes.snapshot()?;
        if session.work_mode != snap.current {
            let mut updated = session.clone();
            updated.work_mode = snap.current;
            store.commit(&[StoreChange::Session(updated)]).await?;
            session.work_mode = snap.current;
        }
        let now = modes.snapshot()?;
        if now.revision == snap.revision && now.pending == snap.pending {
            return Ok(now);
        }
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
    scheduler: &Scheduler,
) -> Result<Vec<HistoryItem>, BoundaryError> {
    let limits = &scheduler.limits;
    for attempt in 0..=2 {
        if cancel.is_cancelled() {
            return Err(error("cancelled", "run cancelled"));
        }
        if !budget.request() {
            return Err(error("limit_reached", "model request budget exceeded"));
        }
        let snap = loop {
            let snap = sync_mode(session, store, &scheduler.modes).await?;
            if cancel.is_cancelled() {
                return Err(error("cancelled", "run cancelled"));
            }
            if scheduler.modes.begin_shared(snap.revision)? {
                break snap;
            }
            tokio::task::yield_now().await;
        };
        let request = ModelRequest {
            request_id: uuid::Uuid::now_v7().to_string(),
            purpose: ModelPurpose::Execution,
            profile: session.profile.clone(),
            instructions: mode_instruction(snap.current).into(),
            max_output_tokens: None,
            history: session.history.clone(),
            tools: tools
                .0
                .values()
                .filter(|tool| allowed(snap.current, tool.class))
                .map(|tool| tool.tool.definition())
                .collect(),
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
        scheduler.modes.finish()?;
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
                sync_mode(session, store, &scheduler.modes).await?;
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
    let scheduler = Scheduler {
        modes: Modes::new(session.work_mode),
        limits,
        shared_budget: None,
        background: None,
    };
    run_with_modes(session, run, model, tools, store, cancel, &scheduler).await
}

async fn run_with_modes(
    session: &mut Session,
    run: &Run,
    model: &dyn Model,
    tools: &ToolRegistry,
    store: &dyn Store,
    cancel: &dyn Cancellation,
    scheduler: &Scheduler,
) -> Result<String, BoundaryError> {
    let outcome = run_inner(session, run, model, tools, store, cancel, scheduler).await;
    if store.load_session(session.id).await?.is_some() {
        sync_mode(session, store, &scheduler.modes).await?;
    } else {
        session.work_mode = scheduler.modes.snapshot()?.current;
    }
    outcome
}

/// Test-only integration wrapper: no run may finish while its background jobs
/// are still executing. A real runtime must additionally share budgets/events.
async fn run_with_background(
    session: &mut Session,
    run: &Run,
    model: &dyn Model,
    tools: &ToolRegistry,
    store: &dyn Store,
    cancel: &dyn Cancellation,
    orchestration: (&Scheduler, &crate::background::BackgroundJobs),
) -> Result<String, BoundaryError> {
    let result = run_with_modes(session, run, model, tools, store, cancel, orchestration.0).await;
    orchestration.1.shutdown().await?;
    result
}

async fn run_inner(
    session: &mut Session,
    run: &Run,
    model: &dyn Model,
    tools: &ToolRegistry,
    store: &dyn Store,
    cancel: &dyn Cancellation,
    scheduler: &Scheduler,
) -> Result<String, BoundaryError> {
    let limits = &scheduler.limits;
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
    if let Some(shared) = &scheduler.shared_budget {
        budget = budget.with_shared(shared.clone());
    }
    let mut last_failure: Option<(String, serde_json::Value, u8)> = None;
    loop {
        check_background(scheduler)?;
        let items =
            model_turn(session, model, tools, store, cancel, &mut budget, scheduler).await?;
        check_background(scheduler)?;
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
        sync_mode(session, store, &scheduler.modes).await?;
        if cancel.is_cancelled() {
            return Err(error("cancelled", "run cancelled"));
        }
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
        for call in calls {
            check_background(scheduler)?;
            sync_mode(session, store, &scheduler.modes).await?;
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
            } else if let Some(registered) = tools.0.get(&call.name) {
                let dispatch = loop {
                    let snap = sync_mode(session, store, &scheduler.modes).await?;
                    if cancel.is_cancelled() {
                        return Err(error("cancelled", "run cancelled"));
                    }
                    if scheduler.modes.begin_shared(snap.revision)? {
                        if allowed(snap.current, registered.class) {
                            break Some(snap.current);
                        }
                        scheduler.modes.finish()?;
                        break None;
                    }
                    tokio::task::yield_now().await;
                };
                if let Some(mode) = dispatch {
                    let mut current_run = run.clone();
                    current_run.work_mode = mode;
                    let context = ToolContext {
                        run: current_run,
                        workspace: session.workspace.clone(),
                    };
                    let result = tool_call(
                        registered.tool.as_ref(),
                        &call,
                        &context,
                        cancel,
                        budget.remaining(),
                        limits.tool_timeout,
                    )
                    .await;
                    scheduler.modes.finish()?;
                    match result {
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
                            sync_mode(session, store, &scheduler.modes).await?;
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
                    failed("mode_denied", "tool is not allowed in PLAN")
                }
            } else {
                failed("unknown_tool", "tool is not registered")
            };
            let failed = !outcome.ok;
            let needs_confirmation = outcome
                .error
                .as_ref()
                .is_some_and(|error| error.code == "needs_confirmation");
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
            check_background(scheduler)?;
            sync_mode(session, store, &scheduler.modes).await?;
            if needs_confirmation {
                return Err(error(
                    "needs_confirmation",
                    "background operation requires confirmation",
                ));
            }
            if cancel.is_cancelled() {
                return Err(error("cancelled", "run cancelled"));
            }
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
                "needs_confirmation" => RunStatus::NeedsConfirmation,
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
