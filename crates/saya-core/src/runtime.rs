//! Reusable, UI-independent run loop. Network providers and durable storage are
//! supplied by callers. Memory-backed side effects are suitable only for tests.
#[path = "runtime_checkpoint.rs"]
mod checkpoint;
#[path = "runtime_context.rs"]
mod context;
#[path = "runtime_memory.rs"]
mod managed_memory;
#[path = "runtime_secrets.rs"]
mod secrets;
#[path = "runtime_services.rs"]
mod services;
#[path = "runtime_skills.rs"]
mod skills;
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
    collections::VecDeque,
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

const MAX_STEER_MESSAGES: usize = 16;
const MAX_STEER_BYTES: usize = 256 * 1024;

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
    secrets: secrets::SecretSource,
}
impl Store for RuntimeStore {
    fn memory_exposures(
        &self,
        id: crate::contract::SessionId,
    ) -> Operation<'_, Vec<crate::memory_context::MemoryExposure>> {
        self.inner.memory_exposures(id)
    }
    fn context_checkpoint(
        &self,
        id: crate::contract::SessionId,
    ) -> Operation<'_, Option<crate::context_store::ContextCheckpoint>> {
        self.inner.context_checkpoint(id)
    }

    fn output_references(
        &self,
        id: crate::contract::SessionId,
    ) -> Operation<'_, Vec<crate::output_store::OutputMetadata>> {
        self.inner.output_references(id)
    }

    fn load_session<'a>(
        &'a self,
        id: crate::contract::SessionId,
    ) -> Operation<'a, Option<Session>> {
        Box::pin(async move {
            let session = self
                .inner
                .load_session(id)
                .await
                .map_err(normalize_storage_error)?;
            secrets::loaded_session(session, &self.secrets)
        })
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            let changes = secrets::changes(changes, &self.secrets)?;
            self.inner
                .commit(&changes)
                .await
                .map_err(normalize_storage_error)?;
            self.metrics.observe(&changes);
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
    steers: SteerQueue,
    project_context: Option<Arc<Mutex<ProjectState>>>,
    personal_context: PersonalContext,
    context_state: Option<Arc<context::ContextState>>,
    managed_memory: Option<Arc<managed_memory::MemoryState>>,
    skill_state: Option<Arc<skills::SkillState>>,
    checkpoints: Option<Arc<checkpoint::CheckpointState>>,
    limits: RuntimeLimits,
    read_only: bool,
    used: bool,
    started: AtomicBool,
    secret_source: secrets::SecretSource,
    services: Arc<services::RuntimeServices>,
    media_store: Option<Arc<crate::output_store::OutputStore>>,
    media_capabilities: crate::config::MediaCapabilities,
    started_at: std::time::SystemTime,
}

#[derive(Default)]
struct SteerState {
    messages: VecDeque<String>,
    bytes: usize,
    closed: bool,
}

#[derive(Clone, Default)]
struct SteerQueue(Arc<Mutex<SteerState>>);

impl SteerQueue {
    fn push(&self, text: String) -> Result<(), BoundaryError> {
        if text.trim().is_empty() {
            return Err(error("invalid_steer", "steering message is empty"));
        }
        let mut state = self
            .0
            .lock()
            .map_err(|_| error("steer_state", "steering queue lock failed"))?;
        if state.closed {
            return Err(error(
                "steer_closed",
                "run no longer accepts steering messages",
            ));
        }
        if state.messages.len() >= MAX_STEER_MESSAGES
            || state
                .bytes
                .checked_add(text.len())
                .is_none_or(|bytes| bytes > MAX_STEER_BYTES)
        {
            return Err(error(
                "steer_limit",
                "queued steering messages exceed their limit",
            ));
        }
        state.bytes += text.len();
        state.messages.push_back(text);
        Ok(())
    }

    fn drain(&self) -> Result<Vec<String>, BoundaryError> {
        self.take(false)
    }

    fn finish_or_drain(&self) -> Result<Vec<String>, BoundaryError> {
        self.take(true)
    }

    fn take(&self, close_if_empty: bool) -> Result<Vec<String>, BoundaryError> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| error("steer_state", "steering queue lock failed"))?;
        if state.closed {
            return Ok(Vec::new());
        }
        if close_if_empty && state.messages.is_empty() {
            state.closed = true;
            return Ok(Vec::new());
        }
        state.bytes = 0;
        Ok(state.messages.drain(..).collect())
    }

    fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.closed = true;
            state.messages.clear();
            state.bytes = 0;
        }
    }
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
    provided: Option<(Option<PathBuf>, crate::project_context::ContextRecord)>,
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
    let state = state
        .lock()
        .map_err(|_| error("project_context_error", "project context lock failed"))?;
    let target = context_target(call).or_else(|| state.target.clone());
    let snapshot = state.loader.snapshot(target.as_deref())?;
    let Some((provided_target, provided)) = &state.provided else {
        return Ok(false);
    };
    let current_fingerprint = if target == *provided_target {
        snapshot.fingerprint()?
    } else {
        state
            .loader
            .snapshot(provided_target.as_deref())?
            .fingerprint()?
    };
    if current_fingerprint != provided.fingerprint {
        return Ok(false);
    }
    Ok(snapshot
        .instructions
        .iter()
        .map(|source| (&source.path, &source.sha256))
        .chain(
            snapshot
                .skills
                .iter()
                .map(|source| (&source.path, &source.sha256)),
        )
        .all(|(path, hash)| {
            provided
                .sources
                .iter()
                .any(|source| &source.path == path && &source.sha256 == hash)
        }))
}
/// Handle usable while the run future owns the runtime.
#[derive(Clone)]
pub struct RuntimeHandle {
    jobs: Arc<BackgroundJobs>,
    modes: ModeController,
    dispatcher: Arc<ToolDispatcher>,
    services: Arc<services::RuntimeServices>,
    context_state: Option<Arc<context::ContextState>>,
    skill_state: Option<std::sync::Weak<skills::SkillState>>,
    steers: SteerQueue,
}
impl RuntimeHandle {
    /// Queue a user guidance message for the next model boundary.
    pub fn steer(&self, text: impl Into<String>) -> Result<(), BoundaryError> {
        self.steers.push(text.into())
    }
    /// A queued request may outlive the final execution response. Interfaces
    /// can honor it with a separate compact run after successful completion.
    pub fn compaction_pending(&self) -> bool {
        self.context_state
            .as_ref()
            .is_some_and(|state| state.requested.load(Ordering::Acquire))
    }
    pub fn request_compaction(&self) -> Result<(), BoundaryError> {
        let state = self
            .context_state
            .as_ref()
            .ok_or_else(|| error("context_config", "Context budgeting is unavailable"))?;
        state.requested.store(true, Ordering::Release);
        Ok(())
    }

    pub fn request_mode(&self, mode: WorkMode) -> Result<(), BoundaryError> {
        self.services.request_mode(self, mode)
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
    /// Stop new background submissions, cancel existing jobs, and wait for their
    /// cleanup. This remains available after the owning run future fails or is
    /// aborted. Repeated calls are safe; completed job status stays readable.
    pub async fn shutdown_background(&self) -> Result<(), BoundaryError> {
        let background = self.jobs.shutdown().await;
        let skills = self
            .skill_state
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .map_or(Ok(()), |state| state.clear_plugin_leases());
        let services = self.services.shutdown(self.dispatcher.as_ref()).await;
        background?;
        skills?;
        services
    }
    /// Snapshot background lifecycle transitions in the order observed by this run.
    pub fn background_events(
        &self,
    ) -> Result<Vec<crate::background::BackgroundEvent>, BoundaryError> {
        self.jobs.events()
    }
}
impl AgentRuntime {
    /// Register a host-controlled external adapter under the shared tool policy.
    pub fn register_external(
        &self,
        server: &str,
        revision: &str,
        adapter: Arc<dyn crate::boundary::Tool>,
        access: crate::registry::ExternalAccess,
    ) -> Result<String, BoundaryError> {
        self.dispatcher
            .register_external(server, revision, adapter, access)
    }
    pub fn replace_external(
        &self,
        server: &str,
        revision: &str,
        tools: Vec<(
            Arc<dyn crate::boundary::Tool>,
            crate::registry::ExternalAccess,
        )>,
    ) -> Result<Vec<String>, BoundaryError> {
        self.dispatcher.replace_external(server, revision, tools)
    }
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
        let secret_source = secrets::source();
        let store: Arc<dyn Store> = Arc::new(RuntimeStore {
            inner: setup.store,
            metrics: metrics.clone(),
            secrets: secret_source.clone(),
        });
        let model: Arc<dyn Model> = Arc::new(MeasuredModel {
            inner: Arc::new(secrets::GuardedModel::new(
                setup.model,
                secret_source.clone(),
            )),
            metrics: metrics.clone(),
        });
        let modes = ModeController::recording(setup.run.work_mode);
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
        let jobs = Arc::new(
            BackgroundJobs::new(
                dispatcher.clone(),
                limits.background_running,
                limits.background_jobs,
            )?
            .with_tool_timeout(limits.tool_timeout)?,
        );
        let background = Arc::new(BackgroundTools::new(
            jobs.clone(),
            model.clone(),
            budget.clone(),
            review.clone(),
            setup.summary.clone(),
            setup.confirmation.is_some(),
        ));
        dispatcher.register_control_definition(crate::input::definition())?;
        for kind in [
            BackgroundToolKind::Start,
            BackgroundToolKind::Status,
            BackgroundToolKind::Cancel,
        ] {
            dispatcher.register_control_definition(background.tool(kind).definition())?;
        }
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
            steers: SteerQueue::default(),
            project_context: None,
            personal_context: PersonalContext::default(),
            context_state: None,
            managed_memory: None,
            skill_state: None,
            checkpoints: None,
            limits,
            read_only,
            used: false,
            started: AtomicBool::new(false),
            secret_source,
            services: Arc::new(services::RuntimeServices::default()),
            media_store: None,
            media_capabilities: crate::config::MediaCapabilities::default(),
            started_at: crate::prompt::cache_aligned_time(std::time::SystemTime::now()),
        })
    }
    pub fn handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            jobs: self.jobs.clone(),
            modes: self.modes.clone(),
            dispatcher: self.dispatcher.clone(),
            services: self.services.clone(),
            context_state: self.context_state.clone(),
            skill_state: self.skill_state.as_ref().map(Arc::downgrade),
            steers: self.steers.clone(),
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
    pub fn with_secret_registry(
        self,
        registry: Arc<crate::credentials::SecretRegistry>,
    ) -> Result<Self, BoundaryError> {
        registry.register(
            &self
                .review
                .known_secrets
                .iter()
                .filter(|value| !value.is_empty())
                .cloned()
                .collect::<Vec<_>>(),
        )?;
        self.dispatcher.set_secret_registry(registry.clone())?;
        *self.secret_source.write().map_err(|_| {
            error(
                "sensitive_data",
                "Dynamic secret registry could not be attached",
            )
        })? = Some(registry);
        Ok(self)
    }
    /// Apply a finite execution window and its resolved output target.
    pub fn with_context_budget(
        mut self,
        budget: crate::context_builder::ContextBudget,
    ) -> Result<Self, BoundaryError> {
        let config = crate::compaction::CompactionConfig {
            summary_output_tokens: u32::try_from(budget.output_reserve).unwrap_or(u32::MAX),
            ..Default::default()
        };
        self = self.with_context_settings(budget, true, config)?;
        Ok(self)
    }
    pub fn with_context_settings(
        mut self,
        budget: crate::context_builder::ContextBudget,
        automatic: bool,
        config: crate::compaction::CompactionConfig,
    ) -> Result<Self, BoundaryError> {
        budget.input_capacity()?;
        self.context_state = Some(Arc::new(context::ContextState::new(
            budget, automatic, config,
        )?));
        Ok(self)
    }
    /// Attach private media snapshot storage before this single-use run starts.
    pub fn with_media_store(
        mut self,
        store: Arc<crate::output_store::OutputStore>,
        capabilities: crate::config::MediaCapabilities,
    ) -> Self {
        self.media_store = Some(store);
        self.media_capabilities = capabilities;
        self
    }
    /// Attach private output storage before this single-use run starts.
    pub fn with_output_store(
        self,
        store: Arc<crate::output_store::OutputStore>,
    ) -> Result<Self, BoundaryError> {
        self.dispatcher
            .set_output_store(Arc::new(store.for_run(self.run.id)))?;
        Ok(self)
    }
    /// Exclude host control stores from every generic local file adapter.
    /// Process isolation uses the separately configured native policy.
    pub fn with_private_paths(self, paths: Vec<PathBuf>) -> Result<Self, BoundaryError> {
        self.dispatcher.protect_host_paths(paths)?;
        Ok(self)
    }
    pub fn with_execution_settings(
        self,
        policy: crate::execution::SandboxPolicy,
        broker: Option<crate::network::BrokerPolicy>,
    ) -> Result<Self, BoundaryError> {
        self.services.configure_execution(policy, broker)?;
        Ok(self)
    }
    pub fn with_plugins(
        self,
        store: crate::plugin_store::PluginStore,
        pool: Arc<crate::plugin_runtime::ViewPool>,
        bindings: crate::plugin_runtime::PluginHostBindings,
        journal: Arc<crate::hook_store::HookStore>,
    ) -> Result<Self, BoundaryError> {
        self.services
            .configure_plugins(store, pool, bindings, journal)?;
        Ok(self)
    }
    pub fn with_mcp_settings(
        self,
        settings: crate::mcp::config::McpSettings,
        environment: crate::config::Environment,
        authorizer: Option<Arc<dyn crate::mcp::config::McpHttpAuthorizer>>,
        registry: Arc<crate::credentials::SecretRegistry>,
    ) -> Result<Self, BoundaryError> {
        let runtime = self.with_secret_registry(registry.clone())?;
        runtime
            .services
            .configure_mcp(settings, environment, authorizer, registry)?;
        Ok(runtime)
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
            provided: None,
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
        self.handle().request_mode(mode)
    }
    /// Snapshot current and pending mode. This never changes authorization.
    pub fn mode(&self) -> Result<crate::mode::ModeSnapshot, BoundaryError> {
        self.modes.snapshot()
    }
    /// Project the model-facing tool definitions without executing a tool or
    /// starting MCP services. Host-selected capabilities use the same filters
    /// as normal execution; callers may append explicit external fixtures.
    pub fn model_tool_definitions(
        &self,
        mode: WorkMode,
    ) -> Result<Vec<ToolDefinition>, BoundaryError> {
        self.definitions(mode)
    }
    fn definitions(&self, mode: WorkMode) -> Result<Vec<ToolDefinition>, BoundaryError> {
        let mut tools = self.dispatcher.definitions(mode)?;
        if let Some(memory) = &self.managed_memory {
            tools.extend(memory.definitions(mode, self.read_only)?);
        }
        if let Some(skills) = &self.skill_state {
            tools.extend(skills.definitions());
        }
        if let Some(checkpoints) = &self.checkpoints {
            tools.extend(checkpoints.definitions(mode, self.read_only));
        }
        if (self.read_only || mode == WorkMode::Plan)
            && let Some(definition) = tools
                .iter_mut()
                .find(|definition| definition.name == "background_start")
        {
            definition.input_schema["properties"]["tool"]["enum"] =
                serde_json::json!(["read_file", "list_dir", "find_files", "search_text"]);
        }
        Ok(tools)
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
            let (snap, events) = self.modes.event_snapshot()?;
            let mut changes = Vec::new();
            if session.work_mode != snap.current {
                let mut update = session.clone();
                update.work_mode = snap.current;
                changes.push(StoreChange::Session(update));
            }
            changes.extend(events.iter().map(|event| StoreChange::Activity {
                run_id: self.run.id,
                payload: event.payload.clone(),
            }));
            if !changes.is_empty() {
                self.store.commit(&changes).await?;
                if let Some(event) = events.last() {
                    self.modes.acknowledge_events(event.sequence)?;
                }
                session.work_mode = snap.current;
            }
            let (now, pending_events) = self.modes.event_snapshot()?;
            if now.revision == snap.revision
                && now.pending == snap.pending
                && pending_events.is_empty()
            {
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
        if let Some(error) = self.services.fatal()? {
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
        let mut attempt = 0u32;
        loop {
            self.check()?;
            self.services.refresh(self).await?;
            let snap = loop {
                let snap = self.sync_mode(session).await?;
                self.check()?;
                if self.modes.begin_shared(snap.revision)? {
                    break snap;
                }
                sleep(Duration::from_millis(10)).await;
            };
            let environment_facts = match self.services.facts(snap.current).await {
                Ok(facts) => facts,
                Err(error) => {
                    self.modes.finish()?;
                    return Err(error);
                }
            };
            let memory_frame = match self.memory_frame(session).await {
                Ok(frame) => frame,
                Err(error) => {
                    self.modes.finish()?;
                    return Err(error);
                }
            };
            let skill_frame = match self.skill_frame(session).await {
                Ok(frame) => frame,
                Err(error) => {
                    self.modes.finish()?;
                    return Err(error);
                }
            };
            let request = (|| {
                let tools = self.definitions(snap.current)?;
                let instructions = crate::prompt::execution_with_memory(
                    snap.current,
                    self.run.permissions,
                    &tools,
                    self.managed_memory.is_some(),
                )?
                .instructions;
                let mut run = self.run.clone();
                run.work_mode = snap.current;
                let mut dynamic_instructions = String::new();
                if let Some(facts) = environment_facts {
                    dynamic_instructions.push_str(&facts);
                }
                dynamic_instructions.push_str(&crate::prompt::system_context_at(
                    session,
                    &run,
                    self.project_context.is_some(),
                    self.personal_context.preferences.is_some(),
                    self.personal_context.memory.is_some(),
                    self.started_at,
                )?);
                let dynamic_instructions = Some(dynamic_instructions);
                let request_id = uuid::Uuid::now_v7().to_string();
                let mut record = None;
                let mut history = vec![crate::prompt::runtime_context(session, &run)?];
                if let Some(state) = &self.project_context {
                    let mut state = state.lock().map_err(|_| {
                        error("project_context_error", "project context lock failed")
                    })?;
                    let snapshot = state.loader.snapshot(state.target.as_deref())?;
                    let context_record = snapshot.record(request_id.clone())?;
                    state.provided = Some((state.target.clone(), context_record.clone()));
                    record = Some(context_record);
                    let mut context = serde_json::json!({"saya_project_context_v1": snapshot});
                    crate::redaction::redact(&mut context, &self.review.known_secrets)?;
                    history.push(HistoryItem::UserText(context.to_string()));
                }
                if self.personal_context.preferences.is_some()
                    || self.personal_context.memory.is_some()
                {
                    history.push(HistoryItem::UserText(serde_json::json!({"saya_personal_context_v1": {"preferences": self.personal_context.preferences, "memory": self.personal_context.memory}}).to_string()));
                }
                if let Some(text) = memory_frame.as_ref().and_then(|frame| frame.text.as_ref()) {
                    history.push(HistoryItem::UserText(text.clone()));
                }
                if let Some(skills) = &skill_frame {
                    history.push(HistoryItem::UserText(skills.context_data().to_string()));
                }
                history.extend(session.history.iter().cloned());
                let media =
                    crate::media::load_history(&history, self.media_store.as_ref(), session.id)?;
                let request = ModelRequest {
                    request_id,
                    purpose: ModelPurpose::Execution,
                    profile: session.profile.clone(),
                    instructions,
                    dynamic_instructions,
                    max_output_tokens: None,
                    history,
                    media: Arc::new(media),
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
            let request = match self
                .context_request(
                    session,
                    request,
                    memory_frame.as_ref(),
                    skill_frame.as_ref(),
                )
                .await
            {
                Ok(request) => request,
                Err(error) => {
                    self.modes.finish()?;
                    return Err(error);
                }
            };
            if self
                .context_state
                .as_ref()
                .is_some_and(|state| state.rebuild.swap(false, Ordering::AcqRel))
            {
                self.modes.finish()?;
                continue;
            }
            if self
                .context_state
                .as_ref()
                .is_some_and(|state| state.manual_only.load(Ordering::Acquire))
            {
                self.modes.finish()?;
                return Ok(Vec::new());
            }
            if let Err(error) = secrets::model_request(&request, &self.secret_source) {
                self.modes.finish()?;
                return Err(error);
            }
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
            if let Err(error) = self.bind_memory_request(&request.request_id, memory_frame.as_ref())
            {
                self.modes.finish()?;
                return Err(error);
            }
            if let (Some(state), Some(frame)) = (&self.skill_state, &skill_frame) {
                let current = match state.context_version() {
                    Ok(current) => current,
                    Err(error) => {
                        self.modes.finish()?;
                        return Err(error);
                    }
                };
                if current != frame.version_sha256 {
                    self.modes.finish()?;
                    return Err(error(
                        "skill_context_changed",
                        "Active Skill context changed before model dispatch",
                    ));
                }
            }
            let start = Instant::now();
            let mut partial = String::new();
            let mut received = false;
            let response: Result<Vec<HistoryItem>, BoundaryError> = async {
                self.budget.consume()?;
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
                        Some(ModelUpdate::Progress | ModelUpdate::ThinkingDelta { .. }) => {
                            received = true
                        }
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
                    return secrets::model_response(items, &self.secret_source);
                }
                Err(err) => {
                    if received {
                        let item = HistoryItem::AssistantPartial(secrets::redact(
                            partial,
                            &self.secret_source,
                        )?);
                        let mut changes = vec![StoreChange::History {
                            session_id: session.id,
                            item: item.clone(),
                        }];
                        if let Some(record) =
                            self.memory_response_change(session, std::slice::from_ref(&item))?
                        {
                            changes.push(record);
                        }
                        self.store.commit(&changes).await?;
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
                        attempt += 1;
                    } else {
                        return Err(err);
                    }
                }
            }
        }
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
            let result = self
                .project_background_memory(session, call, result)
                .await?;
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
            if let Some(target) = context_target(call) {
                state
                    .lock()
                    .map_err(|_| error("project_context_error", "project context lock failed"))?
                    .target = Some(target);
            }
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
        let mut arguments: serde_json::Value = serde_json::from_str(&call.arguments)
            .unwrap_or_else(|_| serde_json::json!({"invalid_arguments":true}));
        if call.name.starts_with("memory_") {
            arguments = serde_json::json!({"host_managed_data":true,"arguments_sha256":crate::memory_context::digest(call.arguments.as_bytes())});
        }
        let hooks = self
            .services
            .fire_hooks(
                self,
                session,
                crate::hooks::HookEvent::ToolBeforeDispatch,
                format!(
                    "tool.before_dispatch:{}",
                    crate::memory_context::digest(call.call_id.as_bytes())
                ),
                Some(call.call_id.clone()),
                serde_json::json!({"tool":call.name,"arguments":arguments}),
            )
            .await?;
        if hooks.cleanup_pending {
            return Err(error(
                "outcome_unknown",
                "A pre-dispatch hook requires cleanup reconciliation",
            ));
        }
        if hooks.blocked {
            return self
                .save_local(
                    session,
                    call,
                    failed(
                        "hook_denied",
                        "A configured check hook blocked this operation",
                    ),
                )
                .await;
        }
        if matches!(
            call.name.as_str(),
            "memory_read" | "memory_search" | "memory_evidence" | "memory_update" | "memory_forget"
        ) {
            return self.execute_memory(session, call, &context, progress).await;
        }
        if matches!(
            call.name.as_str(),
            "skill_catalog" | "skill_activate" | "skill_deactivate" | "skill_read_resource"
        ) {
            return self.execute_skill(session, call, &context, progress).await;
        }
        if matches!(
            call.name.as_str(),
            "checkpoint_create"
                | "checkpoint_list"
                | "checkpoint_diff"
                | "checkpoint_accept"
                | "checkpoint_restore"
        ) {
            return self
                .execute_checkpoint(session, call, &context, progress)
                .await;
        }
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
                    if saved.history.len() < session.history.len() {
                        return Err(error(
                            "storage_error",
                            "saved session history became shorter during tool cleanup",
                        ));
                    }
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
    async fn append_steering(
        &self,
        session: &mut Session,
        messages: Vec<String>,
    ) -> Result<bool, BoundaryError> {
        if messages.is_empty() {
            return Ok(false);
        }
        let mut captures = Vec::new();
        let mut items = Vec::new();
        for text in messages {
            if !crate::media::has_media(&text) {
                items.push(HistoryItem::UserText(text));
                continue;
            }
            let store = self.media_store.as_ref().ok_or_else(|| {
                error(
                    "media_unavailable",
                    "durable media storage is not configured",
                )
            })?;
            let capture =
                crate::media::capture_user(&text, self.media_capabilities, store, session.id)?;
            items.push(HistoryItem::UserText(capture.text.clone()));
            captures.push(capture);
        }
        let changes = items
            .iter()
            .cloned()
            .map(|item| StoreChange::History {
                session_id: session.id,
                item,
            })
            .collect::<Vec<_>>();
        self.store.commit(&changes).await?;
        session.history.extend(items);
        Ok(true)
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
        if !self
            .context_state
            .as_ref()
            .is_some_and(|state| state.manual_only.load(Ordering::Acquire))
        {
            self.services.initialize(self, session).await?;
            let hooks = self
                .services
                .fire_hooks(
                    self,
                    session,
                    crate::hooks::HookEvent::RunStarted,
                    format!("run.started:{}", self.run.id.0),
                    None,
                    serde_json::json!({"history_items":session.history.len()}),
                )
                .await?;
            if hooks.cleanup_pending || hooks.blocked {
                return Err(error(
                    "outcome_unknown",
                    "A run-started hook did not complete safely",
                ));
            }
        }
        let mut previous: Option<(String, serde_json::Value, u8)> = None;
        loop {
            let steering = self.steers.drain()?;
            self.append_steering(session, steering).await?;
            self.check()?;
            let items = self.model_turn(session).await?;
            self.check()?;
            if let Some(state) = &self.context_state
                && state.manual_only.load(Ordering::Acquire)
            {
                return Ok(if state.published.load(Ordering::Acquire) {
                    "Context checkpoint published"
                } else {
                    "No closed history needs compaction"
                }
                .into());
            }
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
            let history_items = secrets::history(items.clone(), &self.secret_source)
                .map_err(normalize_storage_error)?;
            let mut changes: Vec<_> = history_items
                .iter()
                .cloned()
                .map(|item| StoreChange::History {
                    session_id: session.id,
                    item,
                })
                .collect();
            if let Some(record) = self.memory_response_change(session, &history_items)? {
                changes.push(record);
            }
            self.store.commit(&changes).await?;
            session.history.extend(history_items);
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
                let late = self.steers.finish_or_drain()?;
                if self.append_steering(session, late).await? {
                    continue;
                }
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
                if let Err(failure) = self.services.reserve_tool(self.limits.tool_calls) {
                    self.close_calls(session, &calls[index..], CallProgress::NotDispatched)
                        .await?;
                    return Err(failure);
                }
                self.metrics.tool_call();
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
                let finished = self.services.fire_hooks(self,session,crate::hooks::HookEvent::ToolFinished,
                    format!("tool.finished:{}",crate::memory_context::digest(call.call_id.as_bytes())),Some(call.call_id.clone()),
                    serde_json::json!({"tool":call.name,"ok":result.ok,"truncated":result.truncated,"error_code":result.error.as_ref().map(|error|error.code.as_str())})).await;
                if let Err(failure) = &finished {
                    if failure.0.code != "storage_error" && failure.2.is_none() {
                        self.close_calls(session, &calls[index + 1..], CallProgress::NotDispatched)
                            .await?;
                    }
                    return Err(failure.clone());
                }
                if finished?.cleanup_pending {
                    self.close_calls(session, &calls[index + 1..], CallProgress::NotDispatched)
                        .await?;
                    return Err(error(
                        "outcome_unknown",
                        "A tool-finished hook requires cleanup reconciliation",
                    ));
                }
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
                if let Some(failure) = &result.error
                    && matches!(failure.code.as_str(), "needs_auth" | "outcome_unknown")
                {
                    self.close_calls(session, &calls[index + 1..], CallProgress::NotDispatched)
                        .await?;
                    return Err(BoundaryError(failure.clone(), None, None));
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
    /// Run only a tool-free compaction operation against the saved history.
    pub async fn compact(&mut self, session: &mut Session) -> Result<RunResult, BoundaryError> {
        let state = self
            .context_state
            .as_ref()
            .ok_or_else(|| error("context_config", "Context budgeting is unavailable"))?;
        state.manual_only.store(true, Ordering::Release);
        state.requested.store(true, Ordering::Release);
        self.run(session).await
    }
    /// Run once, then cancel and await all background work. Execution returns
    /// typed status; storage failures return errors retaining observed results.
    #[tracing::instrument(target = "saya::runtime", name = "run", skip_all, fields(session_id = %session.id.0, run_id = %self.run.id.0))]
    pub async fn run(&mut self, session: &mut Session) -> Result<RunResult, BoundaryError> {
        if self.used {
            return Err(error("invalid_run", "runtime is single-use"));
        }
        tracing::info!(target: "saya::runtime", event = "run.started");
        self.budget.start_run()?;
        self.metrics.start();
        self.used = true;
        let initial = session.history.len();
        let mut outcome = match self.dispatcher.recover_outputs(session.id).await {
            Ok(()) => self.run_inner(session).await,
            Err(error) => Err(error),
        };
        self.steers.close();
        if self.started.load(Ordering::Acquire)
            && !self
                .context_state
                .as_ref()
                .is_some_and(|state| state.manual_only.load(Ordering::Acquire))
            && !outcome
                .as_ref()
                .err()
                .is_some_and(|failure| failure.0.code == "storage_error" || failure.2.is_some())
        {
            let finished=self.services.fire_hooks(self,session,crate::hooks::HookEvent::RunFinished,
                format!("run.finished:{}",self.run.id.0),None,serde_json::json!({"completed":outcome.is_ok(),"stop_code":outcome.as_ref().err().map(|failure|failure.0.code.as_str())})).await;
            match finished {
                Err(failure) => outcome = Err(failure),
                Ok(hooks) if hooks.cleanup_pending => {
                    outcome = Err(error(
                        "outcome_unknown",
                        "A run-finished hook requires cleanup reconciliation",
                    ))
                }
                _ => {}
            }
        }
        let unfinished = self.jobs.running_count();
        let cleanup = self.jobs.shutdown().await;
        let skill_cleanup = self
            .skill_state
            .as_ref()
            .map_or(Ok(()), |state| state.clear_plugin_leases());
        let service_cleanup = self.services.shutdown(self.dispatcher.as_ref()).await;
        cleanup?;
        skill_cleanup?;
        let unfinished = unfinished?;
        if let Some(error) = self.jobs.fatal_failure()? {
            return Err(error);
        }
        if let Err(e) = &outcome
            && (e.0.code == "storage_error" || e.2.is_some())
        {
            return Err(e.clone());
        }
        let outcome = match service_cleanup {
            Ok(()) => outcome,
            Err(failure) if failure.0.code == "storage_error" => return Err(failure),
            Err(_) => Err(error(
                "outcome_unknown",
                "Run service cleanup could not be confirmed",
            )),
        };
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
                    "limit_reached" | "context_budget" | "context_limit" | "output_limit" => {
                        RunStatus::LimitReached
                    }
                    "needs_confirmation" => RunStatus::NeedsConfirmation,
                    "needs_input" | "needs_auth" => RunStatus::NeedsInput,
                    _ => RunStatus::Failed,
                };
                let recent_history = session.history.get(initial..).ok_or_else(|| {
                    error(
                        "storage_error",
                        "session history became shorter during the run",
                    )
                })?;
                let text = recent_history
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
                            recent_history.iter().rev().find_map(|item| match item {
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
        let result = secrets::redact(result, &self.secret_source)?;
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
        tracing::info!(target: "saya::runtime", event = "run.finished", status = ?result.status,
            error_code = result.error.as_ref().map(|error| error.code.as_str()).unwrap_or(""));
        Ok(result)
    }
}

#[cfg(test)]
mod mode_event_tests {
    use super::*;
    use crate::{
        contract::{ModelProtocol, PermissionMode, RunId, SessionId},
        memory::MemoryStore,
        permissions::PermissionConfig,
        tools::read::ReadLimits,
    };

    struct Never;
    impl Cancellation for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }
    struct EndModel;
    struct EndStream(bool);
    impl Model for EndModel {
        fn start<'a>(
            &'a self,
            _: &'a ModelRequest,
            _: &'a dyn Cancellation,
        ) -> Operation<'a, Box<dyn crate::boundary::ModelStream>> {
            Box::pin(async {
                Ok(Box::new(EndStream(false)) as Box<dyn crate::boundary::ModelStream>)
            })
        }
    }
    impl crate::boundary::ModelStream for EndStream {
        fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
            Box::pin(async move {
                if std::mem::replace(&mut self.0, true) {
                    return Ok(None);
                }
                Ok(Some(ModelUpdate::Finished {
                    items: vec![HistoryItem::AssistantText("done".into())],
                    usage: None,
                }))
            })
        }
    }
    struct ModeStore {
        inner: MemoryStore,
        fail: AtomicBool,
        concurrent: Mutex<Option<ModeController>>,
        batches: Mutex<Vec<Vec<StoreChange>>>,
    }
    impl Store for ModeStore {
        fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
            self.inner.load_session(id)
        }
        fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
            Box::pin(async move {
                let mode_activity = changes.iter().any(|change| {
                    matches!(
                        change,
                        StoreChange::Activity {
                            payload: EventPayload::ModeChangeRequested { .. }
                                | EventPayload::ModeChanged { .. },
                            ..
                        }
                    )
                });
                if mode_activity && self.fail.swap(false, Ordering::AcqRel) {
                    return Err(error(
                        "storage_error",
                        "controlled mode transaction failure",
                    ));
                }
                self.inner.commit(changes).await?;
                self.batches.lock().unwrap().push(changes.to_vec());
                if mode_activity && let Some(modes) = self.concurrent.lock().unwrap().take() {
                    modes.request(WorkMode::Code)?;
                }
                Ok(())
            })
        }
    }
    fn fixture(root: &std::path::Path, store: Arc<ModeStore>) -> (AgentRuntime, Session) {
        let session = Session {
            id: SessionId::new(),
            workspace: root.into(),
            work_mode: WorkMode::Work,
            profile: "fixture".into(),
            protocol: ModelProtocol::ChatCompletions,
            model: "fixture".into(),
            history: vec![HistoryItem::UserText("fixture".into())],
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: WorkMode::Work,
            permissions: PermissionMode::Auto,
        };
        let runtime = AgentRuntime::new(RuntimeSetup {
            run,
            model: Arc::new(EndModel),
            store,
            policy: Arc::new(Policy::new(root, PermissionConfig::default()).unwrap()),
            files: Arc::new(ReadTools::new(root, &[], false, ReadLimits::default()).unwrap()),
            cancellation: Arc::new(Never),
            review: ReviewConfig::new("fixture".into()),
            exec: ExecConfig::default(),
            preauthorizations: Preauthorizations::default(),
            known_secrets: vec![],
            summary: "fixture".into(),
            confirmation: None,
            limits: RuntimeLimits::default(),
        })
        .unwrap();
        (runtime, session)
    }
    fn store(fail: bool) -> Arc<ModeStore> {
        Arc::new(ModeStore {
            inner: MemoryStore::default(),
            fail: AtomicBool::new(fail),
            concurrent: Mutex::new(None),
            batches: Mutex::new(vec![]),
        })
    }
    struct CleanupFailureModel(Arc<services::RuntimeServices>);
    impl Model for CleanupFailureModel {
        fn start<'a>(
            &'a self,
            request: &'a ModelRequest,
            cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Box<dyn crate::boundary::ModelStream>> {
            self.0
                .fail(error("outcome_unknown", "controlled cleanup failure"));
            EndModel.start(request, cancel)
        }
    }
    #[tokio::test]
    async fn unknown_service_cleanup_persists_a_failed_run_without_releasing_its_guard() {
        let root = tempfile::tempdir().unwrap();
        let store = store(false);
        let (mut runtime, mut session) = fixture(root.path(), store.clone());
        runtime.model = Arc::new(CleanupFailureModel(runtime.services.clone()));
        runtime.modes.hold_transition().unwrap();
        let result = runtime.run(&mut session).await.unwrap();
        assert_eq!(result.status, RunStatus::Failed);
        assert_eq!(result.stop_reason.as_deref(), Some("outcome_unknown"));
        assert_eq!(runtime.modes.active_invocations().unwrap(), 1);
        assert!(
            store
                .batches
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|change| {
                    matches!(change, StoreChange::RunFinished(saved) if saved == &result)
                })
        );
    }
    #[tokio::test]
    async fn mode_transaction_acknowledges_only_its_snapshot_during_concurrent_request() {
        let root = tempfile::tempdir().unwrap();
        let store = store(false);
        let (mut runtime, mut session) = fixture(root.path(), store.clone());
        runtime.request_mode(WorkMode::Plan).unwrap();
        *store.concurrent.lock().unwrap() = Some(runtime.modes.clone());
        assert_eq!(
            runtime.run(&mut session).await.unwrap().status,
            RunStatus::Completed
        );
        assert_eq!(session.work_mode, WorkMode::Code);
        let batches = store.batches.lock().unwrap();
        let activities = batches
            .iter()
            .flat_map(|batch| batch.iter())
            .filter_map(|change| {
                if let StoreChange::Activity { payload, .. } = change {
                    Some(payload.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            activities,
            vec![
                EventPayload::ModeChangeRequested {
                    from: WorkMode::Work,
                    to: WorkMode::Plan
                },
                EventPayload::ModeChanged {
                    from: WorkMode::Work,
                    to: WorkMode::Plan
                },
                EventPayload::ModeChangeRequested {
                    from: WorkMode::Plan,
                    to: WorkMode::Code
                },
                EventPayload::ModeChanged {
                    from: WorkMode::Plan,
                    to: WorkMode::Code
                },
            ]
        );
        for batch in batches.iter().filter(|batch| {
            batch
                .iter()
                .any(|change| matches!(change, StoreChange::Activity { .. }))
        }) {
            assert!(matches!(batch.first(), Some(StoreChange::Session(_))));
        }
        assert!(runtime.modes.event_snapshot().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn failed_mode_transaction_retains_journal_and_session_until_successful_commit() {
        let root = tempfile::tempdir().unwrap();
        let store = store(true);
        let (mut runtime, mut session) = fixture(root.path(), store.clone());
        runtime.request_mode(WorkMode::Plan).unwrap();
        assert_eq!(
            runtime.run(&mut session).await.unwrap_err().0.code,
            "storage_error"
        );
        assert_eq!(session.work_mode, WorkMode::Work);
        assert_eq!(runtime.modes.event_snapshot().unwrap().1.len(), 2);
        runtime.sync_mode(&mut session).await.unwrap();
        assert_eq!(session.work_mode, WorkMode::Plan);
        assert!(runtime.modes.event_snapshot().unwrap().1.is_empty());
        assert_eq!(
            store
                .inner
                .load_session(session.id)
                .await
                .unwrap()
                .unwrap()
                .work_mode,
            WorkMode::Plan
        );
    }
}
