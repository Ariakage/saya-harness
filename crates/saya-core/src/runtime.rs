//! Reusable, UI-independent run loop. Network providers and durable storage are
//! supplied by callers. Memory-backed side effects are suitable only for tests.
use crate::{
    approvals::{ApprovalSetup, Authorization, PermissionRevision, Preauthorizations},
    background::{BackgroundJobs, BackgroundToolKind, BackgroundTools},
    boundary::{
        Cancellation, Model, ModelRequest, ModelUpdate, Operation, Store, StoreChange, Tool,
        ToolContext, ToolDefinition,
    },
    contract::{
        ApprovalRequest, ApprovalSource, BoundaryError, ErrorInfo, EventPayload, HistoryItem,
        ModelPurpose, Persistence, Run, RunResult, RunStatus, SCHEMA_VERSION, Session, ToolCall,
        ToolResult, WorkMode,
    },
    dispatch::{DispatchReview, ToolDispatcher},
    input::{InputHandler, InputRequest},
    metrics::{MeasuredModel, Metrics},
    mode::ModeController,
    permissions::Policy,
    project_context::ProjectContext,
    review::{ReviewConfig, SharedBudget},
    tools::{exec::ExecConfig, read::ReadTools},
};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::{Instant, sleep};

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
fn invalid_tool_request(error: &BoundaryError) -> bool {
    matches!(
        error.0.code.as_str(),
        "unknown_tool"
            | "durability_required"
            | "invalid_args"
            | "invalid_path"
            | "path_denied"
            | "stale_approval"
            | "executable_not_found"
    )
}
fn context_target(call: &ToolCall) -> Option<PathBuf> {
    let value: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
    let (tool, args) = if call.name == "background_start" {
        (value.get("tool")?.as_str()?, value.get("arguments")?)
    } else {
        (call.name.as_str(), &value)
    };
    match tool {
        "read_file" | "list_dir" | "find_files" | "search_text" | "write_file" | "apply_patch" => {
            args.get("path")?.as_str().map(PathBuf::from)
        }
        "exec" => Some(
            args.get("cwd")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| PathBuf::from("."), PathBuf::from),
        ),
        _ => None,
    }
}
struct RuntimeStore {
    inner: Arc<dyn Store>,
    metrics: Arc<Metrics>,
}
impl Store for RuntimeStore {
    fn load_session<'a>(
        &'a self,
        id: crate::contract::SessionId,
    ) -> Operation<'a, Option<Session>> {
        Box::pin(async move {
            self.inner
                .load_session(id)
                .await
                .map_err(normalize_storage_error)
        })
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            self.inner
                .commit(changes)
                .await
                .map_err(normalize_storage_error)?;
            self.metrics.observe(changes);
            Ok(())
        })
    }
}
fn normalize_storage_error(mut error: BoundaryError) -> BoundaryError {
    error.0.code = "storage_error".into();
    error
}

struct CallCancel {
    root: Arc<dyn Cancellation>,
    signal: tokio::sync::watch::Sender<bool>,
}
impl CallCancel {
    fn new(root: Arc<dyn Cancellation>) -> Self {
        Self {
            root,
            signal: tokio::sync::watch::channel(false).0,
        }
    }
    fn stop(&self) {
        self.signal.send_replace(true);
    }
}
impl Cancellation for CallCancel {
    fn is_cancelled(&self) -> bool {
        self.root.is_cancelled() || *self.signal.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut receiver = self.signal.subscribe();
            tokio::select! { () = self.root.cancelled() => (), () = async { while !*receiver.borrow_and_update() { if receiver.changed().await.is_err() { break; } } } => () }
        })
    }
}

/// Callback for one explicit human confirmation. `None` means no answer.
pub trait ApprovalHandler: Send + Sync {
    fn confirm<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>>;
}
/// Host-controlled run budgets and timeouts. Values cannot be increased by tools.
#[derive(Debug, Clone)]
pub struct RuntimeLimits {
    pub model_requests: usize,
    pub tool_calls: usize,
    pub active_time: Duration,
    pub model_connect: Duration,
    pub model_idle: Duration,
    pub model_total: Duration,
    pub tool_timeout: Duration,
    pub retry_jitter_max: Duration,
    pub background_running: usize,
    pub background_jobs: usize,
    pub write_max_bytes: usize,
}
impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            model_requests: 64,
            tool_calls: 256,
            active_time: Duration::from_secs(1800),
            model_connect: Duration::from_secs(10),
            model_idle: Duration::from_secs(60),
            model_total: Duration::from_secs(180),
            tool_timeout: Duration::from_secs(120),
            retry_jitter_max: Duration::from_millis(250),
            background_running: 4,
            background_jobs: 256,
            write_max_bytes: crate::tools::write::DEFAULT_WRITE_MAX_BYTES,
        }
    }
}
/// Dependencies for a single run. Use a new runtime after resuming a session;
/// grants, rejection memory and background jobs are never restored.
pub struct RuntimeSetup {
    pub run: Run,
    pub model: Arc<dyn Model>,
    pub store: Arc<dyn Store>,
    pub policy: Arc<Policy>,
    pub files: Arc<ReadTools>,
    pub cancellation: Arc<dyn Cancellation>,
    pub review: ReviewConfig,
    pub exec: ExecConfig,
    pub preauthorizations: Preauthorizations,
    pub known_secrets: Vec<String>,
    pub summary: String,
    pub confirmation: Option<Arc<dyn ApprovalHandler>>,
    pub limits: RuntimeLimits,
}
/// A single-run coordinator. Construction and mode requests do not execute tools.
pub struct AgentRuntime {
    run: Run,
    model: Arc<dyn Model>,
    store: Arc<dyn Store>,
    cancel: Arc<dyn Cancellation>,
    dispatcher: Arc<ToolDispatcher>,
    modes: ModeController,
    jobs: Arc<BackgroundJobs>,
    background: Arc<BackgroundTools>,
    review: ReviewConfig,
    budget: SharedBudget,
    metrics: Arc<Metrics>,
    summary: String,
    confirmation: Option<Arc<dyn ApprovalHandler>>,
    input: Option<Arc<dyn InputHandler>>,
    project_context: Option<Arc<Mutex<ProjectState>>>,
    personal_context: PersonalContext,
    limits: RuntimeLimits,
    read_only: bool,
    used: bool,
    started: AtomicBool,
}
/// Explicitly supplied personal information, included as lower-trust request
/// data. It is neither discovered automatically nor saved by the runtime.
#[derive(Clone, Default)]
pub struct PersonalContext {
    pub preferences: Option<String>,
    pub memory: Option<String>,
}
struct ProjectState {
    loader: ProjectContext,
    target: Option<PathBuf>,
    provided_fingerprint: Option<String>,
}
#[derive(Clone, Copy)]
enum CallProgress {
    NotDispatched,
    Dispatched,
    WaitingInput,
    BackgroundStart,
    BackgroundWaitingApproval,
}
fn project_context_is_current(
    state: &Mutex<ProjectState>,
    call: &ToolCall,
) -> Result<bool, BoundaryError> {
    let mut state = state
        .lock()
        .map_err(|_| error("project_context_error", "project context lock failed"))?;
    let target = context_target(call).or_else(|| state.target.clone());
    let snapshot = state.loader.snapshot(target.as_deref())?;
    let fingerprint = snapshot.fingerprint()?;
    state.target = target;
    Ok(state.provided_fingerprint.as_deref() == Some(fingerprint.as_str()))
}
/// Handle usable while the run future owns the runtime.
#[derive(Clone)]
pub struct RuntimeHandle {
    jobs: Arc<BackgroundJobs>,
    modes: ModeController,
}
impl RuntimeHandle {
    pub fn request_mode(&self, mode: WorkMode) -> Result<(), BoundaryError> {
        self.jobs.request_mode(mode)
    }
    pub fn mode(&self) -> Result<crate::mode::ModeSnapshot, BoundaryError> {
        self.modes.snapshot()
    }
    pub fn background_status(
        &self,
        id: &str,
    ) -> Result<Option<crate::background::JobStatus>, BoundaryError> {
        self.jobs.status(id)
    }
    pub fn cancel_background(&self, id: &str) -> Result<bool, BoundaryError> {
        self.jobs.cancel(id)
    }
    /// Snapshot background lifecycle transitions in the order observed by this run.
    pub fn background_events(
        &self,
    ) -> Result<Vec<crate::background::BackgroundEvent>, BoundaryError> {
        self.jobs.events()
    }
}
impl AgentRuntime {
    pub fn new(setup: RuntimeSetup) -> Result<Self, BoundaryError> {
        Self::with_read_only(setup, false)
    }
    /// Construct a runtime that cannot dispatch writes or commands, even when
    /// a model fabricates an unregistered foreground or background tool call.
    pub fn new_read_only(setup: RuntimeSetup) -> Result<Self, BoundaryError> {
        Self::with_read_only(setup, true)
    }
    fn with_read_only(setup: RuntimeSetup, read_only: bool) -> Result<Self, BoundaryError> {
        let limits = setup.limits;
        if limits.model_requests == 0
            || limits.tool_calls == 0
            || limits.active_time.is_zero()
            || limits.model_connect.is_zero()
            || limits.model_idle.is_zero()
            || limits.model_total.is_zero()
            || limits.tool_timeout.is_zero()
            || limits.write_max_bytes == 0
        {
            return Err(error("invalid_config", "runtime limits must be positive"));
        }
        let metrics = Arc::new(Metrics::default());
        let store: Arc<dyn Store> = Arc::new(RuntimeStore {
            inner: setup.store,
            metrics: metrics.clone(),
        });
        let model: Arc<dyn Model> = Arc::new(MeasuredModel {
            inner: setup.model,
            metrics: metrics.clone(),
        });
        let modes = ModeController::new(setup.run.work_mode);
        let mut review = setup.review;
        review
            .known_secrets
            .extend(setup.known_secrets.iter().cloned());
        review.known_secrets.sort();
        review.known_secrets.dedup();
        let permission_revision = PermissionRevision::new(modes.clone(), setup.run.permissions);
        let budget = SharedBudget::new(limits.model_requests, limits.active_time);
        let dispatcher = ToolDispatcher::new(
            setup.policy,
            modes.clone(),
            permission_revision,
            store.clone(),
            ApprovalSetup {
                run_id: setup.run.id,
                session_id: setup.run.session_id,
                initial_permissions: setup.run.permissions,
                preauthorizations: setup.preauthorizations,
                known_secrets: setup.known_secrets,
                cancellation: setup.cancellation.clone(),
            },
            setup.files,
            setup.exec,
        )?
        .with_write_limit(limits.write_max_bytes);
        let dispatcher = Arc::new(if read_only {
            dispatcher.restrict_to_read_only()
        } else {
            dispatcher
        });
        let jobs = Arc::new(BackgroundJobs::new(
            dispatcher.clone(),
            limits.background_running,
            limits.background_jobs,
        )?);
        let background = Arc::new(BackgroundTools::new(
            jobs.clone(),
            model.clone(),
            budget.clone(),
            review.clone(),
            setup.summary.clone(),
            setup.confirmation.is_some(),
        ));
        Ok(Self {
            run: setup.run,
            model,
            store,
            cancel: setup.cancellation,
            dispatcher,
            modes,
            jobs,
            background,
            review,
            budget,
            metrics,
            summary: setup.summary,
            confirmation: setup.confirmation,
            input: None,
            project_context: None,
            personal_context: PersonalContext::default(),
            limits,
            read_only,
            used: false,
            started: AtomicBool::new(false),
        })
    }
    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            jobs: self.jobs.clone(),
            modes: self.modes.clone(),
        }
    }
    /// Apply explicitly configured rates to the two request categories.
    pub fn with_pricing(
        self,
        execution: Option<crate::config::Pricing>,
        review: Option<crate::config::Pricing>,
    ) -> Self {
        self.metrics.pricing(execution, review);
        self
    }
    /// Install a UI clarification handler before starting this single-use run.
    pub fn with_input_handler(mut self, handler: Arc<dyn InputHandler>) -> Self {
        self.input = Some(handler);
        self
    }
    /// Attach explicitly supplied preferences and memory for this run only.
    pub fn with_personal_context(mut self, context: PersonalContext) -> Self {
        self.personal_context = context;
        self
    }
    /// Supply scoped project context as untrusted data for each model request.
    pub fn with_project_context(mut self, loader: ProjectContext) -> Result<Self, BoundaryError> {
        let state = Arc::new(Mutex::new(ProjectState {
            loader,
            target: None,
            provided_fingerprint: None,
        }));
        let guard = state.clone();
        self.dispatcher.set_context_guard(Arc::new(move |call| {
            project_context_is_current(&guard, call)
        }))?;
        self.project_context = Some(state);
        Ok(self)
    }
    /// Queue a user-selected mode; PLAN first cancels mutating background jobs.
    pub fn request_mode(&self, mode: WorkMode) -> Result<(), BoundaryError> {
        self.jobs.request_mode(mode)
    }
    /// Snapshot current and pending mode. This never changes authorization.
    pub fn mode(&self) -> Result<crate::mode::ModeSnapshot, BoundaryError> {
        self.modes.snapshot()
    }
    fn definitions(&self, mode: WorkMode) -> Vec<ToolDefinition> {
        let mut tools = self.dispatcher.definitions();
        if mode == WorkMode::Plan {
            tools.retain(|d| {
                matches!(
                    d.name.as_str(),
                    "read_file" | "list_dir" | "find_files" | "search_text"
                )
            });
        }
        tools.push(crate::input::definition());
        for kind in [
            BackgroundToolKind::Start,
            BackgroundToolKind::Status,
            BackgroundToolKind::Cancel,
        ] {
            let mut definition = self.background.tool(kind).definition();
            if self.read_only && matches!(kind, BackgroundToolKind::Start) {
                definition.input_schema["properties"]["tool"]["enum"] =
                    serde_json::json!(["read_file", "list_dir", "find_files", "search_text"]);
            }
            tools.push(definition);
        }
        tools
    }
    fn context(&self, session: &Session, mode: WorkMode) -> ToolContext {
        let mut run = self.run.clone();
        run.work_mode = mode;
        ToolContext {
            run,
            workspace: session.workspace.clone(),
        }
    }
    async fn sync_mode(
        &self,
        session: &mut Session,
    ) -> Result<crate::mode::ModeSnapshot, BoundaryError> {
        loop {
            let snap = self.modes.snapshot()?;
            if session.work_mode != snap.current {
                let mut update = session.clone();
                update.work_mode = snap.current;
                self.store.commit(&[StoreChange::Session(update)]).await?;
                session.work_mode = snap.current;
            }
            let now = self.modes.snapshot()?;
            if now.revision == snap.revision && now.pending == snap.pending {
                return Ok(now);
            }
        }
    }
    fn check(&self) -> Result<(), BoundaryError> {
        if self.cancel.is_cancelled() {
            return Err(error("cancelled", "run cancelled"));
        }
        if let Some(error) = self.jobs.fatal_failure()? {
            return Err(error);
        }
        if self.budget.remaining()?.is_zero() {
            return Err(error("limit_reached", "active time budget exceeded"));
        }
        Ok(())
    }
    async fn bounded<T>(
        &self,
        operation: impl Future<Output = Result<T, BoundaryError>>,
        timeout: Duration,
        code: &'static str,
    ) -> Result<T, BoundaryError> {
        self.check()?;
        tokio::select! { biased; () = self.cancel.cancelled() => Err(error("cancelled", "run cancelled")), () = sleep(self.budget.remaining()?) => Err(error("limit_reached", "active time budget exceeded")), () = sleep(timeout) => Err(error(code, "operation timed out")), result = operation => result }
    }
    async fn model_turn(&self, session: &mut Session) -> Result<Vec<HistoryItem>, BoundaryError> {
        for attempt in 0..=2u32 {
            self.check()?;
            self.budget.consume()?;
            let snap = loop {
                let snap = self.sync_mode(session).await?;
                self.check()?;
                if self.modes.begin_shared(snap.revision)? {
                    break snap;
                }
                sleep(Duration::from_millis(10)).await;
            };
            let request = (|| {
                let tools = self.definitions(snap.current);
                let instructions =
                    crate::prompt::execution(snap.current, self.run.permissions, &tools)?
                        .instructions;
                let mut run = self.run.clone();
                run.work_mode = snap.current;
                let dynamic_instructions = Some(crate::prompt::system_context(
                    session,
                    &run,
                    self.project_context.is_some(),
                    self.personal_context.preferences.is_some(),
                    self.personal_context.memory.is_some(),
                )?);
                let request_id = uuid::Uuid::now_v7().to_string();
                let mut record = None;
                let mut history = vec![crate::prompt::runtime_context(session, &run)?];
                if let Some(state) = &self.project_context {
                    let mut state = state.lock().map_err(|_| {
                        error("project_context_error", "project context lock failed")
                    })?;
                    let snapshot = state.loader.snapshot(state.target.as_deref())?;
                    state.provided_fingerprint = Some(snapshot.fingerprint()?);
                    record = Some(snapshot.record(request_id.clone())?);
                    let mut context = serde_json::json!({"saya_project_context_v1": snapshot});
                    crate::redaction::redact(&mut context, &self.review.known_secrets)?;
                    history.push(HistoryItem::UserText(context.to_string()));
                }
                if self.personal_context.preferences.is_some()
                    || self.personal_context.memory.is_some()
                {
                    history.push(HistoryItem::UserText(serde_json::json!({"saya_personal_context_v1": {"preferences": self.personal_context.preferences, "memory": self.personal_context.memory}}).to_string()));
                }
                history.extend(session.history.iter().cloned());
                let request = ModelRequest {
                    request_id,
                    purpose: ModelPurpose::Execution,
                    profile: session.profile.clone(),
                    instructions,
                    dynamic_instructions,
                    max_output_tokens: None,
                    history,
                    tools,
                };
                Ok::<_, BoundaryError>((request, record))
            })();
            let (request, record) = match request {
                Ok(v) => v,
                Err(e) => {
                    self.modes.finish()?;
                    return Err(e);
                }
            };
            if let Some(record) = record
                && let Err(e) = self
                    .store
                    .commit(&[StoreChange::ProjectContext {
                        run_id: self.run.id,
                        record,
                    }])
                    .await
            {
                self.modes.finish()?;
                return Err(e);
            }
            let start = Instant::now();
            let mut partial = String::new();
            let mut received = false;
            let response: Result<Vec<HistoryItem>, BoundaryError> = async {
                let mut stream = self
                    .bounded(
                        self.model.start(&request, self.cancel.as_ref()),
                        self.limits.model_connect.min(self.limits.model_total),
                        "model_connect_timeout",
                    )
                    .await?;
                loop {
                    let left = self.limits.model_total.saturating_sub(start.elapsed());
                    match self
                        .bounded(
                            stream.next(self.cancel.as_ref()),
                            self.limits.model_idle.min(left),
                            "model_timeout",
                        )
                        .await?
                    {
                        Some(ModelUpdate::TextDelta(text)) => {
                            received = true;
                            partial.push_str(&text);
                        }
                        Some(ModelUpdate::Progress) => received = true,
                        Some(ModelUpdate::Finished { items, .. }) => return Ok(items),
                        None => {
                            return Err(error(
                                "incomplete_model_response",
                                "stream ended before completion",
                            ));
                        }
                    }
                }
            }
            .await;
            self.modes.finish()?;
            match response {
                Ok(items) => {
                    if !items.iter().any(|item| {
                        matches!(
                            item,
                            HistoryItem::AssistantText(_) | HistoryItem::ToolCall(_)
                        )
                    }) {
                        return Err(error(
                            "invalid_model_response",
                            "model completed without text or tool calls",
                        ));
                    }
                    return Ok(items);
                }
                Err(err) => {
                    if received {
                        let item = HistoryItem::AssistantPartial(partial);
                        self.store
                            .commit(&[StoreChange::History {
                                session_id: session.id,
                                item: item.clone(),
                            }])
                            .await?;
                        session.history.push(item);
                    }
                    if !received
                        && attempt < 2
                        && matches!(
                            err.0.code.as_str(),
                            "transient_connection" | "rate_limited" | "server_error"
                        )
                    {
                        let jitter_max =
                            self.limits
                                .retry_jitter_max
                                .as_millis()
                                .min(u128::from(u64::MAX)) as u64;
                        let jitter = if jitter_max == 0 {
                            Duration::ZERO
                        } else {
                            Duration::from_millis(
                                (uuid::Uuid::now_v7().as_u128() as u64)
                                    % jitter_max.saturating_add(1),
                            )
                        };
                        let delay = (Duration::from_secs(1u64 << attempt) + jitter)
                            .max(err.1.unwrap_or_default().min(Duration::from_secs(30)));
                        self.bounded(
                            async {
                                sleep(delay).await;
                                Ok(())
                            },
                            self.limits.active_time,
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
    async fn confirm(&self, request: &ApprovalRequest) -> Result<Option<bool>, BoundaryError> {
        let Some(handler) = &self.confirmation else {
            return Ok(None);
        };
        self.budget.pause()?;
        let answer = tokio::select! { biased; ()=self.cancel.cancelled()=>Err(error("cancelled","run cancelled")), answer=handler.confirm(request,self.cancel.as_ref())=>answer };
        self.budget.resume()?;
        answer
    }
    async fn save_local(
        &self,
        session: &mut Session,
        call: &ToolCall,
        result: ToolResult,
    ) -> Result<ToolResult, BoundaryError> {
        let item = HistoryItem::ToolResult {
            call_id: call.call_id.clone(),
            result: result.clone(),
        };
        let mut changes = vec![
            StoreChange::ToolIntent {
                run_id: self.run.id,
                call: call.clone(),
            },
            StoreChange::ToolFinished {
                run_id: self.run.id,
                call_id: call.call_id.clone(),
                result: result.clone(),
            },
            StoreChange::History {
                session_id: session.id,
                item: item.clone(),
            },
        ];
        if let Some(code) = result.error.as_ref().map(|error| error.code.as_str()) {
            let state = match code {
                "permission_denied" | "mode_denied" => Some(crate::contract::ToolState::Denied),
                "needs_confirmation" => Some(crate::contract::ToolState::WaitingConfirmation),
                _ => None,
            };
            if let Some(state) = state {
                changes.push(StoreChange::ToolState {
                    run_id: self.run.id,
                    call_id: call.call_id.clone(),
                    state,
                });
            }
        }
        self.store.commit(&changes).await?;
        session.history.push(item);
        Ok(result)
    }
    async fn finish_control(
        &self,
        session: &mut Session,
        call: &ToolCall,
        kind: BackgroundToolKind,
        result: ToolResult,
    ) -> Result<ToolResult, BoundaryError> {
        if !matches!(kind, BackgroundToolKind::Start) {
            return self.save_local(session, call, result).await;
        }
        let item = HistoryItem::ToolResult {
            call_id: call.call_id.clone(),
            result: result.clone(),
        };
        let mut changes = vec![
            StoreChange::ToolFinished {
                run_id: self.run.id,
                call_id: call.call_id.clone(),
                result: result.clone(),
            },
            StoreChange::History {
                session_id: session.id,
                item: item.clone(),
            },
        ];
        let state = match result.error.as_ref().map(|error| error.code.as_str()) {
            Some("permission_denied" | "mode_denied") => Some(crate::contract::ToolState::Denied),
            Some("needs_confirmation") => Some(crate::contract::ToolState::WaitingConfirmation),
            _ => None,
        };
        if let Some(state) = state {
            changes.push(StoreChange::ToolState {
                run_id: self.run.id,
                call_id: call.call_id.clone(),
                state,
            });
        }
        if let Err(mut error) = self.store.commit(&changes).await {
            error.2 = Some(Box::new(result));
            return Err(error);
        }
        session.history.push(item);
        Ok(result)
    }
    async fn close_calls(
        &self,
        session: &mut Session,
        calls: &[ToolCall],
        first_progress: CallProgress,
    ) -> Result<(), BoundaryError> {
        let mut changes = Vec::new();
        let mut items = Vec::new();
        for (index, call) in calls.iter().enumerate() {
            if session.history.iter().any(|item| matches!(item, HistoryItem::ToolResult { call_id, .. } if call_id == &call.call_id)) { continue; }
            let result = match (index == 0, first_progress) {
                (true, CallProgress::Dispatched) => failed(
                    "outcome_unknown",
                    "operation may have started; do not replay automatically",
                ),
                (true, CallProgress::BackgroundStart) => failed(
                    "outcome_unknown",
                    "background submission was interrupted; do not replay automatically",
                ),
                (true, CallProgress::BackgroundWaitingApproval) => failed(
                    "needs_confirmation",
                    "background approval was interrupted; no authorization was restored",
                ),
                (true, CallProgress::WaitingInput) => failed(
                    "input_interrupted",
                    "clarification ended before an answer was received",
                ),
                _ => failed(
                    "not_executed",
                    "run ended before this operation was dispatched",
                ),
            };
            let item = HistoryItem::ToolResult {
                call_id: call.call_id.clone(),
                result: result.clone(),
            };
            if index == 0
                && matches!(
                    first_progress,
                    CallProgress::WaitingInput
                        | CallProgress::BackgroundStart
                        | CallProgress::BackgroundWaitingApproval
                )
            {
                changes.push(StoreChange::ToolFinished {
                    run_id: self.run.id,
                    call_id: call.call_id.clone(),
                    result,
                });
            }
            changes.push(StoreChange::History {
                session_id: session.id,
                item: item.clone(),
            });
            items.push(item);
        }
        if !changes.is_empty() {
            self.store.commit(&changes).await?;
            session.history.extend(items);
        }
        Ok(())
    }
    async fn execute(
        &self,
        session: &mut Session,
        call: &ToolCall,
        progress: &mut CallProgress,
    ) -> Result<ToolResult, BoundaryError> {
        self.check()?;
        if let Some(state) = &self.project_context {
            let changed = !project_context_is_current(state, call)?;
            if changed {
                return self.save_local(session, call, failed("project_context_changed", "applicable project context changed; review it before requesting this operation again")).await;
            }
        }
        let snap = loop {
            let snap = self.sync_mode(session).await?;
            self.check()?;
            if snap.pending.is_none() {
                break snap;
            }
            sleep(Duration::from_millis(10)).await;
        };
        let context = self.context(session, snap.current);
        let control = match call.name.as_str() {
            "background_start" => Some(BackgroundToolKind::Start),
            "background_status" => Some(BackgroundToolKind::Status),
            "background_cancel" => Some(BackgroundToolKind::Cancel),
            _ => None,
        };
        if let Some(kind) = control {
            if matches!(kind, BackgroundToolKind::Start) {
                self.store
                    .commit(&[StoreChange::ToolIntent {
                        run_id: self.run.id,
                        call: call.clone(),
                    }])
                    .await?;
                *progress = CallProgress::BackgroundStart;
            }
            if matches!(kind, BackgroundToolKind::Cancel) {
                *progress = CallProgress::Dispatched;
            }
            let outcome = self
                .background
                .tool(kind)
                .execute(call, &context, self.cancel.as_ref())
                .await;
            let result = match outcome {
                Ok(result) => result,
                Err(error) if invalid_tool_request(&error) => {
                    failed(&error.0.code, &error.0.message)
                }
                Err(error) => return Err(error),
            };
            if result
                .error
                .as_ref()
                .is_some_and(|e| e.code == "needs_confirmation")
            {
                if matches!(kind, BackgroundToolKind::Start) {
                    *progress = CallProgress::BackgroundWaitingApproval;
                }
                let request: ApprovalRequest = serde_json::from_value(
                    result
                        .data
                        .as_ref()
                        .and_then(|v| v.get("request"))
                        .cloned()
                        .ok_or_else(|| error("invalid_args", "missing approval request"))?,
                )
                .map_err(|_| error("invalid_args", "invalid approval request"))?;
                let answer = match self.confirm(&request).await {
                    Err(e) if e.0.code == "stale_approval" => {
                        return self
                            .finish_control(
                                session,
                                call,
                                kind,
                                failed("stale_approval", "mode changed while awaiting approval"),
                            )
                            .await;
                    }
                    other => other?,
                };
                if let Some(answer) = answer {
                    self.store
                        .commit(&[StoreChange::Activity {
                            run_id: self.run.id,
                            payload: EventPayload::PermissionResolved {
                                approval_id: request.approval_id.clone(),
                                call_id: request.call_id.clone(),
                                decision: if answer { "allow" } else { "deny" }.into(),
                                source: ApprovalSource::User,
                            },
                        }])
                        .await?;
                    if let Some(state) = &self.project_context
                        && !project_context_is_current(state, call)?
                    {
                        return self
                            .finish_control(
                                session,
                                call,
                                kind,
                                failed(
                                    "project_context_changed",
                                    "project context changed while awaiting approval",
                                ),
                            )
                            .await;
                    }
                    if matches!(kind, BackgroundToolKind::Start) {
                        *progress = CallProgress::BackgroundStart;
                    }
                    let result = self.background.resolve(&request.approval_id, answer)?;
                    return self.finish_control(session, call, kind, result).await;
                }
            }
            return self.finish_control(session, call, kind, result).await;
        }
        let review = DispatchReview {
            model: self.model.as_ref(),
            budget: &self.budget,
            config: &self.review,
        };
        let authorization = match self
            .dispatcher
            .authorize(
                call,
                &context,
                &self.summary,
                self.confirmation.is_some(),
                review,
            )
            .await
        {
            Ok(value) => value,
            Err(e) if invalid_tool_request(&e) => {
                return self
                    .save_local(session, call, failed(&e.0.code, &e.0.message))
                    .await;
            }
            Err(e) => return Err(e),
        };
        let authorization = match authorization {
            Authorization::Pending(request) => match self.confirm(&request).await {
                Ok(Some(answer)) => {
                    self.store
                        .commit(&[StoreChange::Activity {
                            run_id: self.run.id,
                            payload: EventPayload::PermissionResolved {
                                approval_id: request.approval_id.clone(),
                                call_id: request.call_id.clone(),
                                decision: if answer { "allow" } else { "deny" }.into(),
                                source: ApprovalSource::User,
                            },
                        }])
                        .await?;
                    self.dispatcher.resolve(&request.approval_id, answer)?
                }
                Err(e) if e.0.code == "stale_approval" => Authorization::Stale,
                Err(e) => return Err(e),
                Ok(None) => {
                    return self
                        .save_local(
                            session,
                            call,
                            failed("needs_confirmation", "confirmation unavailable"),
                        )
                        .await;
                }
            },
            other => other,
        };
        match authorization {
            Authorization::Permit(permit) => {
                if let Some(state) = &self.project_context
                    && !project_context_is_current(state, call)?
                {
                    return self
                        .save_local(
                            session,
                            call,
                            failed(
                                "project_context_changed",
                                "project context changed while awaiting approval",
                            ),
                        )
                        .await;
                }
                if call.name == "ask_user" {
                    if !self.dispatcher.owns_permit(&permit) {
                        return Err(error(
                            "permission_denied",
                            "permit belongs to another dispatcher",
                        ));
                    }
                    let request = InputRequest::parse(call)?;
                    let mut redacted = serde_json::to_value(&request)
                        .map_err(|_| error("invalid_args", "cannot encode input request"))?;
                    crate::redaction::redact(&mut redacted, &self.review.known_secrets)?;
                    let request: InputRequest = serde_json::from_value(redacted)
                        .map_err(|_| error("invalid_args", "cannot redact input request"))?;
                    permit.consume_input(call, &context)?;
                    self.store
                        .commit(&[StoreChange::Activity {
                            run_id: self.run.id,
                            payload: EventPayload::InputRequested {
                                input_id: call.call_id.clone(),
                                question: request.question.clone(),
                                options: request.options.clone(),
                            },
                        }])
                        .await?;
                    let answer = if let Some(handler) = &self.input {
                        self.store
                            .commit(&[StoreChange::ToolIntent {
                                run_id: self.run.id,
                                call: call.clone(),
                            }])
                            .await?;
                        *progress = CallProgress::WaitingInput;
                        self.budget.pause()?;
                        let result = tokio::select! { biased; () = self.cancel.cancelled() => Err(error("cancelled", "run cancelled")), result = handler.ask(&request, self.cancel.as_ref()) => result };
                        self.budget.resume()?;
                        result?
                    } else {
                        None
                    };
                    let result = match answer {
                        Some(answer) if !answer.trim().is_empty() && answer.len() <= 65536 => {
                            self.store
                                .commit(&[StoreChange::Activity {
                                    run_id: self.run.id,
                                    payload: EventPayload::InputReceived {
                                        input_id: call.call_id.clone(),
                                        answer: answer.clone(),
                                    },
                                }])
                                .await?;
                            ToolResult {
                                ok: true,
                                data: Some(serde_json::json!({"answer": answer})),
                                error: None,
                                truncated: false,
                            }
                        }
                        _ => ToolResult {
                            ok: false,
                            data: Some(
                                serde_json::json!({"question": request.question, "options": request.options}),
                            ),
                            error: Some(ErrorInfo {
                                code: "needs_input".into(),
                                message: "user input required".into(),
                            }),
                            truncated: false,
                        },
                    };
                    let result =
                        crate::redaction::tool_result(&result, &self.review.known_secrets)?;
                    if self.input.is_some() {
                        crate::tools::write::save_result(
                            self.store.as_ref(),
                            call,
                            &context,
                            &result,
                        )
                        .await?;
                        session.history.push(HistoryItem::ToolResult {
                            call_id: call.call_id.clone(),
                            result: result.clone(),
                        });
                        return Ok(result);
                    }
                    return self.save_local(session, call, result).await;
                }
                let cancel = CallCancel::new(self.cancel.clone());
                *progress = CallProgress::Dispatched;
                let operation = self
                    .dispatcher
                    .execute_with_cancel(call, &context, permit, &cancel, false);
                tokio::pin!(operation);
                let interrupted = tokio::select! {
                    biased;
                    () = self.cancel.cancelled() => Some(error("cancelled", "run cancelled")),
                    () = sleep(self.budget.remaining()?) => Some(error("limit_reached", "active time budget exceeded")),
                    () = sleep(self.limits.tool_timeout), if call.name != "exec" => Some(error("tool_timeout", "tool timed out")),
                    result = &mut operation => {
                        let result = match result {
                            Err(err) if err.0.code == "project_context_changed" => {
                                return self.save_local(session, call, failed("project_context_changed", "project context changed before dispatch")).await;
                            }
                            other => other?,
                        };
                        session.history.push(HistoryItem::ToolResult { call_id:call.call_id.clone(), result:result.clone() });
                        return Ok(result);
                    }
                };
                cancel.stop();
                let result = operation.await;
                if let Err(err) = &result
                    && err.0.code != "cancelled"
                {
                    return Err(err.clone());
                }
                if let Ok(result) = result {
                    session.history.push(HistoryItem::ToolResult {
                        call_id: call.call_id.clone(),
                        result,
                    });
                } else if let Some(saved) = self.store.load_session(session.id).await? {
                    session.history = saved.history;
                }
                Err(interrupted.unwrap_or_else(|| error("cancelled", "run interrupted")))
            }
            Authorization::NeedsConfirmation(_) | Authorization::Pending(_) => {
                self.save_local(
                    session,
                    call,
                    failed("needs_confirmation", "confirmation required"),
                )
                .await
            }
            Authorization::Denied(_) => {
                self.save_local(
                    session,
                    call,
                    failed("permission_denied", "operation denied"),
                )
                .await
            }
            Authorization::Stale => {
                self.save_local(
                    session,
                    call,
                    failed("stale_approval", "mode or permission changed"),
                )
                .await
            }
        }
    }
    async fn run_inner(&self, session: &mut Session) -> Result<String, BoundaryError> {
        self.check()?;
        if session.id != self.run.session_id || session.work_mode != self.run.work_mode {
            return Err(error("invalid_run", "session or mode mismatch"));
        }
        if self.review.execution_profile != session.profile {
            return Err(error(
                "invalid_config",
                "review execution profile differs from the session",
            ));
        }
        self.model.validate_session(session)?;
        let existing = self.store.load_session(session.id).await?;
        if existing.as_ref().is_some_and(|saved| saved != session) {
            return Err(error(
                "session_conflict",
                "saved session differs from provided session",
            ));
        }
        let mut start = Vec::new();
        if existing.is_none() {
            start.push(StoreChange::Session(session.clone()));
        }
        start.push(StoreChange::RunStarted(self.run.clone()));
        self.store.commit(&start).await?;
        self.started.store(true, Ordering::Release);
        let mut calls_used = 0usize;
        let mut previous: Option<(String, serde_json::Value, u8)> = None;
        loop {
            self.check()?;
            let items = self.model_turn(session).await?;
            self.check()?;
            let mut seen_ids: std::collections::HashSet<&str> = session
                .history
                .iter()
                .filter_map(|item| {
                    if let HistoryItem::ToolCall(call) = item {
                        Some(call.call_id.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            for item in &items {
                match item {
                    HistoryItem::ToolCall(call)
                        if call.call_id.is_empty() || !seen_ids.insert(call.call_id.as_str()) =>
                    {
                        return Err(error(
                            "invalid_model_response",
                            "tool call IDs must be nonempty and unique",
                        ));
                    }
                    HistoryItem::UserText(_)
                    | HistoryItem::ToolResult { .. }
                    | HistoryItem::AssistantPartial(_) => {
                        return Err(error(
                            "invalid_model_response",
                            "unexpected item in model response",
                        ));
                    }
                    _ => {}
                }
            }
            let changes: Vec<_> = items
                .iter()
                .cloned()
                .map(|item| StoreChange::History {
                    session_id: session.id,
                    item,
                })
                .collect();
            self.store.commit(&changes).await?;
            session.history.extend(items.iter().cloned());
            let calls: Vec<_> = items
                .iter()
                .filter_map(|item| {
                    if let HistoryItem::ToolCall(call) = item {
                        Some(call.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if calls.is_empty() {
                return Ok(items
                    .iter()
                    .filter_map(|item| {
                        if let HistoryItem::AssistantText(text) = item {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect());
            }
            for (index, call) in calls.iter().enumerate() {
                if let Err(err) = self.check() {
                    self.close_calls(session, &calls[index..], CallProgress::NotDispatched)
                        .await?;
                    return Err(err);
                }
                calls_used += 1;
                self.metrics.tool_call();
                if calls_used > self.limits.tool_calls {
                    self.close_calls(session, &calls[index..], CallProgress::NotDispatched)
                        .await?;
                    return Err(error("limit_reached", "tool call budget exceeded"));
                }
                let mut progress = CallProgress::NotDispatched;
                let result = match self.execute(session, call, &mut progress).await {
                    Ok(result) => result,
                    Err(err) => {
                        if err.0.code != "storage_error" && err.2.is_none() {
                            self.close_calls(session, &calls[index..], progress).await?;
                        }
                        return Err(err);
                    }
                };
                if result
                    .error
                    .as_ref()
                    .is_some_and(|e| e.code == "needs_confirmation")
                {
                    self.close_calls(session, &calls[index + 1..], CallProgress::NotDispatched)
                        .await?;
                    return Err(error("needs_confirmation", "confirmation required"));
                }
                if result
                    .error
                    .as_ref()
                    .is_some_and(|e| e.code == "needs_input")
                {
                    self.close_calls(session, &calls[index + 1..], CallProgress::NotDispatched)
                        .await?;
                    return Err(error("needs_input", "user input required"));
                }
                if result
                    .error
                    .as_ref()
                    .is_some_and(|e| e.code == "project_context_changed")
                {
                    self.close_calls(session, &calls[index + 1..], CallProgress::NotDispatched)
                        .await?;
                    break;
                }
                let args = serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .unwrap_or_else(|_| serde_json::Value::String(call.arguments.clone()));
                if !result.ok {
                    let count = match &previous {
                        Some((name, old, count)) if name == &call.name && old == &args => {
                            count.saturating_add(1)
                        }
                        _ => 1,
                    };
                    previous = Some((call.name.clone(), args, count));
                    if count >= 3 {
                        self.close_calls(session, &calls[index + 1..], CallProgress::NotDispatched)
                            .await?;
                        return Err(error(
                            "repeated_tool_failure",
                            "same tool and arguments failed three times",
                        ));
                    }
                } else {
                    previous = None;
                }
            }
        }
    }
    /// Run once, then cancel and await all background work. Execution returns
    /// typed status; storage failures return errors retaining observed results.
    pub async fn run(&mut self, session: &mut Session) -> Result<RunResult, BoundaryError> {
        if self.used {
            return Err(error("invalid_run", "runtime is single-use"));
        }
        self.budget.start_run()?;
        self.metrics.start();
        self.used = true;
        let initial = session.history.len();
        let outcome = self.run_inner(session).await;
        let unfinished = self.jobs.running_count()?;
        let cleanup = self.jobs.shutdown().await;
        cleanup?;
        if let Some(error) = self.jobs.fatal_failure()? {
            return Err(error);
        }
        if let Err(e) = &outcome
            && (e.0.code == "storage_error" || e.2.is_some())
        {
            return Err(e.clone());
        }
        if self.started.load(Ordering::Acquire) {
            self.sync_mode(session).await?;
        }
        let (status, reason, text, info) = match outcome {
            Ok(text) => (
                RunStatus::Completed,
                if unfinished == 0 {
                    "completed"
                } else {
                    "completed_with_background_cleanup"
                }
                .into(),
                Some(text),
                None,
            ),
            Err(err) => {
                let status = match err.0.code.as_str() {
                    "cancelled" => RunStatus::Cancelled,
                    "limit_reached" | "context_budget" | "output_limit" => RunStatus::LimitReached,
                    "needs_confirmation" => RunStatus::NeedsConfirmation,
                    "needs_input" => RunStatus::NeedsInput,
                    _ => RunStatus::Failed,
                };
                let text = session.history[initial..]
                    .iter()
                    .rev()
                    .find_map(|item| {
                        if let HistoryItem::AssistantPartial(text) = item {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        if status == RunStatus::NeedsInput {
                            session.history[initial..]
                                .iter()
                                .rev()
                                .find_map(|item| match item {
                                    HistoryItem::ToolResult { result, .. }
                                        if result
                                            .error
                                            .as_ref()
                                            .is_some_and(|e| e.code == "needs_input") =>
                                    {
                                        result
                                            .data
                                            .as_ref()?
                                            .get("question")?
                                            .as_str()
                                            .map(str::to_owned)
                                    }
                                    _ => None,
                                })
                        } else {
                            None
                        }
                    });
                (status, err.0.code.clone(), text, Some(err.0))
            }
        };
        let started = self.started.load(Ordering::Acquire);
        let (metrics, usage, artifacts) = self
            .metrics
            .snapshot()
            .ok_or_else(|| error("metrics_error", "run metrics lock failed"))?;
        let result = RunResult {
            schema_version: SCHEMA_VERSION,
            session_id: Some(session.id),
            run_id: Some(self.run.id),
            status,
            stop_reason: Some(reason),
            text,
            error: info,
            usage,
            artifacts,
            persistence: Some(Persistence {
                saved: started,
                error: None,
            }),
        };
        if started {
            self.store
                .commit(&[
                    StoreChange::RunMetrics {
                        run_id: self.run.id,
                        metrics,
                    },
                    StoreChange::RunFinished(result.clone()),
                ])
                .await?;
        }
        Ok(result)
    }
}
