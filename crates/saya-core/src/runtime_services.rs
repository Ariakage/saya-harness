#[path = "runtime_plugins.rs"]
mod plugins;
use super::{AgentRuntime, RuntimeHandle, error};
use crate::{
    approvals::{Authorization, AuthorizationInput},
    boundary::{Cancellation, Operation, Store, StoreChange, Tool, ToolContext, ToolDefinition},
    config::Environment,
    contract::{
        ApprovalSource, BoundaryError, EventPayload, McpServerState, McpServerStatus, McpTransport,
        RunId, Session, ToolCall, ToolResult, ToolState, WorkMode,
    },
    credentials::SecretRegistry,
    dispatch::{DispatchReview, ToolDispatcher},
    execution::{NetworkPolicy, PreparedEnvironment, ProxyEndpoint, SandboxPolicy},
    mcp::{
        McpClient, McpHttpClient, McpHttpConfig, McpToolAdapter,
        config::{
            McpHttpAuthorizer, McpServerSettings, McpSettings, McpTransportSettings,
            StdioMcpSettings,
        },
    },
    network::{BrokerHandle, BrokerPolicy, NetworkBroker},
    registry::ExternalAccess,
    tools::exec::{ExecGate, ManagedProcess},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncReadExt, sync::Mutex as AsyncMutex, task::JoinHandle};

#[derive(Clone, Default)]
struct Settings {
    execution: Option<(SandboxPolicy, Option<BrokerPolicy>)>,
    mcp: McpSettings,
    environment: Environment,
    authorizer: Option<Arc<dyn McpHttpAuthorizer>>,
    registry: Option<Arc<SecretRegistry>>,
    plugins: Option<plugins::PluginConfiguration>,
}
struct LocalProcess {
    process: ManagedProcess,
    stderr: Option<JoinHandle<()>>,
}
struct Server {
    client: Arc<McpClient>,
    revision: String,
    local: Option<LocalProcess>,
    tools: usize,
    read_only_tools: usize,
}
#[derive(Default)]
struct State {
    initialized: bool,
    closed: bool,
    environment: Option<Arc<PreparedEnvironment>>,
    broker: Option<BrokerHandle>,
    network_budget: Option<Arc<crate::network::RunNetworkBudget>>,
    broker_directory: Option<std::path::PathBuf>,
    servers: BTreeMap<String, Server>,
    pending_locals: BTreeMap<String, LocalProcess>,
    status_store: Option<(Arc<dyn Store>, RunId)>,
    statuses: BTreeMap<String, McpServerStatus>,
    emitted_status: Vec<McpServerStatus>,
    plugin_preparation: Option<plugins::PluginPreparation>,
    plugins: Option<Arc<plugins::PluginRun>>,
    plugin_broker_directory: Option<std::path::PathBuf>,
}

pub(super) struct RuntimeServices {
    settings: Mutex<Settings>,
    state: AsyncMutex<State>,
    begun: AtomicBool,
    local_configured: AtomicBool,
    active: Arc<AtomicUsize>,
    startup_calls: AtomicUsize,
    used_tools: AtomicUsize,
    fatal: Mutex<Option<BoundaryError>>,
    executor: Mutex<Option<tokio::runtime::Handle>>,
    transition_running: Mutex<bool>,
}
impl Default for RuntimeServices {
    fn default() -> Self {
        Self {
            settings: Mutex::new(Settings::default()),
            state: AsyncMutex::new(State::default()),
            begun: AtomicBool::new(false),
            local_configured: AtomicBool::new(false),
            active: Arc::new(AtomicUsize::new(0)),
            startup_calls: AtomicUsize::new(0),
            used_tools: AtomicUsize::new(0),
            fatal: Mutex::new(None),
            executor: Mutex::new(None),
            transition_running: Mutex::new(false),
        }
    }
}

struct ActiveCall(Arc<AtomicUsize>);
impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct StartupPermit(Arc<crate::approvals::OneShotPermit>);
impl Drop for StartupPermit {
    fn drop(&mut self) {
        let _ = ExecGate::finish(self.0.as_ref());
    }
}
struct StartupCancel {
    root: Arc<dyn Cancellation>,
    modes: crate::mode::ModeController,
    revision: u64,
    current: WorkMode,
}
impl Cancellation for StartupCancel {
    fn is_cancelled(&self) -> bool {
        self.root.is_cancelled()
            || self.modes.snapshot().map_or(true, |mode| {
                mode.current != self.current
                    || mode.revision != self.revision
                    || mode.pending.is_some()
            })
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            tokio::select! {_ =self.root.cancelled()=>{},_ =async{while !self.is_cancelled(){tokio::time::sleep(Duration::from_millis(5)).await;}}=>{}}
        })
    }
}
struct ManagedTool {
    inner: McpToolAdapter,
    active: Arc<AtomicUsize>,
    services: Weak<RuntimeServices>,
}
impl Tool for ManagedTool {
    fn definition(&self) -> ToolDefinition {
        self.inner.definition()
    }
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(async move {
            self.active.fetch_add(1, Ordering::AcqRel);
            let _active = ActiveCall(self.active.clone());
            if let Some(services) = self.services.upgrade() {
                services.check_plugin_entry(self.inner.server_id()).await?;
            }
            let outcome = self.inner.execute(call, context, cancel).await;
            if let Err(failure) = &outcome
                && let Some(services) = self.services.upgrade()
            {
                services
                    .mark_failure(self.inner.server_id(), failure)
                    .await?;
            }
            outcome
        })
    }
}

impl RuntimeServices {
    async fn emit_status(&self, state: &mut State) -> Result<(), BoundaryError> {
        let servers = state.statuses.values().cloned().collect::<Vec<_>>();
        if servers.is_empty() || servers == state.emitted_status {
            return Ok(());
        }
        let (store, run_id) = state
            .status_store
            .as_ref()
            .ok_or_else(|| error("runtime_services", "MCP lifecycle store is unavailable"))?;
        store
            .commit(&[StoreChange::Activity {
                run_id: *run_id,
                payload: EventPayload::McpStatus {
                    servers: servers.clone(),
                },
            }])
            .await?;
        state.emitted_status = servers;
        Ok(())
    }
    async fn mark_failure(&self, id: &str, failure: &BoundaryError) -> Result<(), BoundaryError> {
        if failure.0.code == "needs_input" {
            return Ok(());
        }
        let mut state = self.state.lock().await;
        if let Some(status) = state.statuses.get_mut(id) {
            status.state = if failure.0.code == "needs_auth" {
                McpServerState::NeedsAuth
            } else {
                McpServerState::Failed
            };
            status.tools = 0;
        }
        self.emit_status(&mut state).await
    }
    pub(super) fn configure_execution(
        &self,
        policy: SandboxPolicy,
        broker: Option<BrokerPolicy>,
    ) -> Result<(), BoundaryError> {
        if self.begun.load(Ordering::Acquire) {
            return Err(error(
                "invalid_run",
                "Execution settings cannot change during a run",
            ));
        }
        self.settings
            .lock()
            .map_err(|_| error("runtime_services", "Service settings lock failed"))?
            .execution = Some((policy, broker));
        Ok(())
    }
    pub(super) fn configure_mcp(
        &self,
        mcp: McpSettings,
        environment: Environment,
        authorizer: Option<Arc<dyn McpHttpAuthorizer>>,
        registry: Arc<SecretRegistry>,
    ) -> Result<(), BoundaryError> {
        mcp.validate()?;
        if self.begun.load(Ordering::Acquire) {
            return Err(error(
                "invalid_run",
                "MCP settings cannot change during a run",
            ));
        }
        self.local_configured.store(
            self.local_configured.load(Ordering::Acquire)
                || mcp.servers.values().any(|server| {
                    server.enabled && matches!(server.transport, McpTransportSettings::Stdio(_))
                }),
            Ordering::Release,
        );
        let mut settings = self
            .settings
            .lock()
            .map_err(|_| error("runtime_services", "Service settings lock failed"))?;
        settings.mcp = mcp;
        settings.environment = environment;
        settings.authorizer = authorizer;
        settings.registry = Some(registry);
        Ok(())
    }
    /// Reserve one host or model-dispatched tool attempt from the same Run
    /// counter. A failed CAS never starts an additional operation.
    pub(super) fn reserve_tool(&self, limit: usize) -> Result<(), BoundaryError> {
        self.used_tools
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < limit).then_some(used + 1)
            })
            .map(|_| ())
            .map_err(|_| error("limit_reached", "Run tool budget is exhausted"))
    }
    pub(super) fn startup_calls(&self) -> usize {
        self.startup_calls.load(Ordering::Acquire)
    }
    pub(super) fn fatal(&self) -> Result<Option<BoundaryError>, BoundaryError> {
        Ok(self
            .fatal
            .lock()
            .map_err(|_| error("runtime_services", "Service failure lock failed"))?
            .clone())
    }
    pub(super) fn fail(&self, failure: BoundaryError) {
        if let Ok(mut fatal) = self.fatal.lock() {
            *fatal = Some(failure);
        }
    }

    pub(super) fn request_mode(
        self: &Arc<Self>,
        handle: &RuntimeHandle,
        target: WorkMode,
    ) -> Result<(), BoundaryError> {
        let execution = handle.dispatcher.effective_exec_config()?.execution;
        let isolated = execution.is_some()
            || self
                .settings
                .lock()
                .map_err(|_| error("runtime_services", "Service settings lock failed"))?
                .execution
                .is_some();
        if target != WorkMode::Plan
            || !self.begun.load(Ordering::Acquire)
            || (!self.local_configured.load(Ordering::Acquire) && !isolated)
        {
            return handle.jobs.request_mode(target);
        }
        let executor = self
            .executor
            .lock()
            .map_err(|_| error("runtime_services", "Service executor lock failed"))?
            .clone()
            .ok_or_else(|| error("runtime_services", "Service executor is unavailable"))?;
        let mut transition_running = self
            .transition_running
            .lock()
            .map_err(|_| error("runtime_services", "Mode cleanup lock failed"))?;
        if *transition_running {
            return handle.jobs.request_mode(target);
        }
        handle.modes.hold_transition()?;
        if let Err(failure) = handle.jobs.request_mode(target) {
            handle.modes.finish()?;
            return Err(failure);
        }
        // The request is already pending under our guard, so no fresh dispatch
        // can race this idle fast path. In-flight invocations retain the guard
        // across the second permit check and native process-lease acquisition.
        if !self.local_configured.load(Ordering::Acquire)
            && handle.modes.active_invocations()? == 1
            && execution
                .as_ref()
                .is_none_or(|environment| environment.active_processes() == 0)
        {
            handle.modes.finish()?;
            return Ok(());
        }
        *transition_running = true;
        let services = self.clone();
        let modes = handle.modes.clone();
        let dispatcher = handle.dispatcher.clone();
        executor.spawn(async move {
            let cleanup = services
                .cleanup_for_plan(&modes, &dispatcher, Duration::from_secs(10))
                .await;
            match cleanup {
                Ok(()) => {
                    if let Err(failure) = modes.finish() {
                        services.fail(failure);
                    } else if let Ok(mut running) = services.transition_running.lock() {
                        *running = false;
                    }
                }
                Err(_) => services.fail(error(
                    "outcome_unknown",
                    "MCP cleanup could not be confirmed before PLAN",
                )),
            }
        });
        Ok(())
    }

    async fn cleanup_for_plan(
        &self,
        modes: &crate::mode::ModeController,
        dispatcher: &ToolDispatcher,
        orphan_timeout: Duration,
    ) -> Result<(), BoundaryError> {
        let mut orphan_started = None;
        let mut hooks_stopped = false;
        loop {
            let mode = modes.snapshot()?;
            if mode.pending != Some(WorkMode::Plan) {
                return Ok(());
            }
            // Existing foreground/model/human-input invocations retain their
            // original lifetime. Cleanup deadlines start at the idle barrier.
            if modes.active_invocations()? == 1 {
                if !hooks_stopped {
                    let plugins = tokio::time::timeout(Duration::from_secs(130), async {
                        self.state.lock().await.plugins.clone()
                    })
                    .await
                    .map_err(|_| {
                        error(
                            "outcome_unknown",
                            "Service initialization did not yield the mode cleanup boundary",
                        )
                    })?;
                    if let Some(plugins) = plugins {
                        tokio::time::timeout(
                            crate::hooks::HookLimits::default().cleanup_timeout,
                            plugins.processes.stop_all(),
                        )
                        .await
                        .map_err(|_| {
                            error("outcome_unknown", "Hook cleanup timed out before PLAN")
                        })??;
                    }
                    hooks_stopped = true;
                }
                if self.active.load(Ordering::Acquire) == 0 {
                    break;
                }
                let started = orphan_started.get_or_insert_with(tokio::time::Instant::now);
                if started.elapsed() >= orphan_timeout {
                    return Err(error(
                        "outcome_unknown",
                        "Unowned service activity prevents a confirmed PLAN transition",
                    ));
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let mut state = tokio::time::timeout(Duration::from_secs(130), self.state.lock())
            .await
            .map_err(|_| {
                error(
                    "outcome_unknown",
                    "Service cleanup ownership is unavailable",
                )
            })?;
        let entries = state
            .servers
            .len()
            .saturating_add(state.pending_locals.len());
        let seconds = u64::try_from(entries)
            .ok()
            .and_then(|entries| entries.checked_mul(12))
            .and_then(|seconds| seconds.checked_add(20))
            .ok_or_else(|| error("outcome_unknown", "Service cleanup budget is invalid"))?;
        tokio::time::timeout(Duration::from_secs(seconds), async {
            self.stop_servers(&mut state, dispatcher, true).await?;
            if let Some(environment) = &state.environment {
                environment.wait_for_process_cleanup().await?;
            }
            Ok::<(), BoundaryError>(())
        })
        .await
        .map_err(|_| {
            error(
                "outcome_unknown",
                "Service cleanup could not finish before PLAN",
            )
        })?
    }

    pub(super) async fn initialize(
        self: &Arc<Self>,
        runtime: &AgentRuntime,
        session: &Session,
    ) -> Result<(), BoundaryError> {
        let settings = self
            .settings
            .lock()
            .map_err(|_| error("runtime_services", "Service settings lock failed"))?
            .clone();
        *self
            .executor
            .lock()
            .map_err(|_| error("runtime_services", "Service executor lock failed"))? =
            Some(tokio::runtime::Handle::current());
        self.begun.store(true, Ordering::Release);
        let mut state = self.state.lock().await;
        if state.initialized {
            return Ok(());
        }
        if state.closed {
            return Err(error("invalid_run", "Run services have already closed"));
        }
        state.status_store = Some((runtime.store.clone(), runtime.run.id));
        if let Some(plugins) = &settings.plugins {
            use std::os::unix::fs::DirBuilderExt;
            let (base, _) = settings.execution.as_ref().ok_or_else(|| {
                error(
                    "sandbox_required",
                    "Plugin runtime requires host execution policy",
                )
            })?;
            plugins.pool.validate_location(
                &session.workspace,
                &base.protected_paths,
                &base.write_roots,
            )?;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&plugins.broker_root)
                .map_err(|_| {
                    error(
                        "sandbox_unavailable",
                        "Private plugin broker root could not be created",
                    )
                })?;
            state.plugin_broker_directory = Some(plugins.broker_root.clone());
        }
        state.statuses = settings
            .mcp
            .servers
            .iter()
            .filter(|(_, server)| server.enabled)
            .map(|(id, server)| {
                (
                    id.clone(),
                    McpServerStatus {
                        server: id.clone(),
                        transport: if matches!(server.transport, McpTransportSettings::Stdio(_)) {
                            McpTransport::Stdio
                        } else {
                            McpTransport::Http
                        },
                        state: McpServerState::Configured,
                        tools: 0,
                    },
                )
            })
            .collect();
        self.emit_status(&mut state).await?;
        if let Some((mut policy, broker)) = settings.execution.clone() {
            if let Some(plugins) = &settings.plugins {
                for path in [
                    plugins.store.protected_path(),
                    plugins.journal.protected_path(),
                    plugins.pool.path(),
                    plugins.broker_root.as_path(),
                ] {
                    if !policy.protected_paths.contains(&path.to_path_buf()) {
                        policy.protected_paths.push(path.to_path_buf());
                    }
                }
            }
            if policy.workspace.canonicalize().ok() != session.workspace.canonicalize().ok() {
                return Err(error(
                    "invalid_workspace",
                    "Execution settings differ from the active workspace",
                ));
            }
            if let Some(broker) = broker {
                #[cfg(target_os = "linux")]
                let mut broker = broker;
                #[cfg(target_os = "linux")]
                if broker.unix_socket.is_none() {
                    use std::os::unix::fs::DirBuilderExt;
                    let workspace = policy.workspace.canonicalize().map_err(|_| {
                        error(
                            "invalid_workspace",
                            "Broker workspace could not be resolved",
                        )
                    })?;
                    let roots = policy
                        .write_roots
                        .iter()
                        .map(|root| root.canonicalize())
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| {
                            error(
                                "invalid_sandbox_policy",
                                "Broker writable roots could not be resolved",
                            )
                        })?;
                    let base = [std::env::temp_dir(), std::path::PathBuf::from("/var/tmp")]
                        .into_iter()
                        .filter_map(|path| path.canonicalize().ok())
                        .find(|path| {
                            !path.starts_with(&workspace)
                                && !roots.iter().any(|root| path.starts_with(root))
                        })
                        .ok_or_else(|| {
                            error(
                                "sandbox_unavailable",
                                "Private broker storage is unavailable outside writable roots",
                            )
                        })?;
                    let directory = base.join(format!("saya-broker-{}", uuid::Uuid::now_v7()));
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&directory)
                        .map_err(|_| {
                            error(
                                "sandbox_unavailable",
                                "Private broker directory could not be created",
                            )
                        })?;
                    broker.unix_socket = Some(directory.join("proxy.sock"));
                    state.broker_directory = Some(directory);
                }
                let shared = crate::network::RunNetworkBudget::new(&broker.limits)
                    .map_err(|_| error("sandbox_unavailable", "Run network budget is invalid"))?;
                state.network_budget = Some(shared.clone());
                let broker = runtime
                    .bounded(
                        async {
                            NetworkBroker::start_scoped(broker, runtime.cancel.clone(), shared)
                                .await
                                .map_err(|_| {
                                    error(
                                        "sandbox_unavailable",
                                        "Run network broker could not start",
                                    )
                                })
                        },
                        runtime.limits.tool_timeout,
                        "sandbox_timeout",
                    )
                    .await?;
                policy.network = NetworkPolicy::Proxy(ProxyEndpoint {
                    endpoint: broker.tcp_endpoint(),
                    unix_socket: broker.unix_socket().map(std::path::Path::to_path_buf),
                });
                state.broker = Some(broker);
            }
            runtime.check()?;
            let environment = Arc::new(crate::execution::prepare(&policy)?);
            runtime
                .dispatcher
                .set_execution_environment(environment.clone())?;
            state.environment = Some(environment);
            runtime.check()?;
        }
        self.initialize_plugins(runtime, session, &settings, &mut state)
            .await?;
        let mut registered = settings.mcp.clone();
        if let Some(plugins) = state.plugins.clone() {
            for (id, server) in &plugins.servers {
                if registered
                    .servers
                    .insert(id.clone(), server.settings.clone())
                    .is_some()
                {
                    return Err(error(
                        "plugin_identity",
                        "Plugin and configured MCP identities collide",
                    ));
                }
                state.statuses.insert(
                    id.clone(),
                    McpServerStatus {
                        server: id.clone(),
                        transport: if server.process.is_some() {
                            McpTransport::Stdio
                        } else {
                            McpTransport::Http
                        },
                        state: McpServerState::Configured,
                        tools: 0,
                    },
                );
            }
        }
        self.emit_status(&mut state).await?;
        for (id, server) in &registered.servers {
            if !server.enabled {
                continue;
            }
            runtime.check()?;
            let snapshot = runtime.modes.snapshot()?;
            if matches!(server.transport, McpTransportSettings::Stdio(_))
                && (runtime.read_only
                    || snapshot.current == WorkMode::Plan
                    || snapshot.pending == Some(WorkMode::Plan))
            {
                if let Some(status) = state.statuses.get_mut(id) {
                    status.state = McpServerState::Stopped;
                }
                self.emit_status(&mut state).await?;
                continue;
            }
            if matches!(server.transport, McpTransportSettings::Http(_))
                && (snapshot.current == WorkMode::Plan || snapshot.pending == Some(WorkMode::Plan))
                && server.read_only_tools.is_empty()
            {
                if let Some(status) = state.statuses.get_mut(id) {
                    status.state = McpServerState::Stopped;
                }
                self.emit_status(&mut state).await?;
                continue;
            }
            if let Some(status) = state.statuses.get_mut(id) {
                status.state = McpServerState::Connecting;
            }
            self.emit_status(&mut state).await?;
            let mut entry_settings = settings.clone();
            if let Some(descriptor) = state
                .plugins
                .as_ref()
                .and_then(|plugins| plugins.server(id))
            {
                state
                    .plugins
                    .as_ref()
                    .ok_or_else(|| error("plugin_state", "Plugin runtime is unavailable"))?
                    .check(id)?;
                entry_settings.environment = descriptor.environment().clone();
            }
            let outcome = async {
                match &server.transport {
                    McpTransportSettings::Stdio(_) => {
                        if state.environment.is_none() {
                            return Err(error(
                                "sandbox_required",
                                "MCP stdio startup requires a prepared execution environment",
                            ));
                        }
                        self.start_local(runtime, session, id, server, &entry_settings, &mut state)
                            .await?;
                    }
                    McpTransportSettings::Http(http) => {
                        let limits = server.limits.resolve()?;
                        let mut config = McpHttpConfig::new(&http.endpoint);
                        config.allow_insecure_http = http.allow_insecure_http;
                        let registry = settings.registry.as_ref().ok_or_else(|| {
                            error("sensitive_data", "MCP credential registry is unavailable")
                        })?;
                        for (name, reference) in &http.headers_env {
                            let value = env_reference(&entry_settings.environment, reference)?;
                            registry.register(std::slice::from_ref(&value))?;
                            config.headers.insert(name.clone(), value);
                        }
                        if let Some(reference) = &http.bearer_env {
                            let value = env_reference(&entry_settings.environment, reference)?;
                            registry.register(std::slice::from_ref(&value))?;
                            config.bearer_token = Some(value);
                        }
                        let mut client = if let Some(plugins) = state
                            .plugins
                            .as_ref()
                            .filter(|plugins| plugins.server(id).is_some())
                        {
                            plugins.http_client(id, &limits).await?
                        } else {
                            McpHttpClient::new(&limits)?
                        };
                        if http.credential_ref.is_some() {
                            let authorizer = settings.authorizer.as_ref().ok_or_else(|| {
                                error("needs_auth", "MCP credential resolver is unavailable")
                            })?;
                            let authorization = runtime
                                .bounded(
                                    authorizer.authorize(id, http, client, runtime.cancel.as_ref()),
                                    limits.connect_timeout,
                                    "mcp_timeout",
                                )
                                .await?;
                            client = authorization.client;
                            config.bearer_token = authorization.bearer_token;
                        }
                        let connect_timeout = limits.connect_timeout;
                        let client = Arc::new(
                            runtime
                                .bounded(
                                    McpClient::connect_http_with_client(
                                        id,
                                        client,
                                        config,
                                        limits,
                                        runtime.cancel.as_ref(),
                                    ),
                                    connect_timeout,
                                    "mcp_timeout",
                                )
                                .await?,
                        );
                        if let Some(plugins) = &state.plugins
                            && plugins.server(id).is_some()
                        {
                            plugins.record(id, crate::plugin_runtime::StopEvidence::Unknown)?;
                        }
                        state.servers.insert(
                            id.clone(),
                            Server {
                                client,
                                revision: String::new(),
                                local: None,
                                tools: 0,
                                read_only_tools: 0,
                            },
                        );
                    }
                }
                Ok::<(), BoundaryError>(())
            }
            .await;
            if let Err(failure) = outcome {
                if let Some(status) = state.statuses.get_mut(id) {
                    status.state = match failure.0.code.as_str() {
                        "stale_approval" => McpServerState::Stopped,
                        "needs_auth" => McpServerState::NeedsAuth,
                        _ => McpServerState::Failed,
                    };
                    status.tools = 0;
                }
                self.emit_status(&mut state).await?;
                if failure.0.code == "stale_approval" {
                    continue;
                }
                return Err(failure);
            }
        }
        state.initialized = true;
        drop(state);
        self.refresh(runtime).await
    }

    async fn start_local(
        &self,
        runtime: &AgentRuntime,
        session: &Session,
        id: &str,
        server: &McpServerSettings,
        settings: &Settings,
        state: &mut State,
    ) -> Result<(), BoundaryError> {
        runtime.check()?;
        let McpTransportSettings::Stdio(local) = &server.transport else {
            return Err(error(
                "invalid_config",
                "MCP startup requires a stdio transport",
            ));
        };
        let limits = server.limits.resolve()?;
        let snapshot = runtime.modes.snapshot()?;
        let startup_cancel = StartupCancel {
            root: runtime.cancel.clone(),
            modes: runtime.modes.clone(),
            revision: snapshot.revision,
            current: snapshot.current,
        };
        if startup_cancel.is_cancelled() {
            return Err(startup_cancelled(runtime));
        }
        self.reserve_tool(runtime.limits.tool_calls)?;
        self.startup_calls
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < runtime.limits.tool_calls).then_some(count + 1)
            })
            .map_err(|_| error("limit_reached", "MCP startup exhausted the tool budget"))?;
        let references = local.env.values().cloned().collect::<BTreeSet<_>>();
        let environment = local
            .env
            .iter()
            .map(|(name, reference)| {
                (
                    name.clone(),
                    serde_json::json!({"source":"reference","value":reference}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let call = ToolCall {
            call_id: format!("mcp-start-{}", uuid::Uuid::now_v7()),
            name: "exec".into(),
            arguments: serde_json::json!({"argv":local.argv,"cwd":local.cwd,"env":environment})
                .to_string(),
        };
        let context = runtime.context(session, runtime.modes.snapshot()?.current);
        let plugin = state
            .plugins
            .as_ref()
            .filter(|plugins| plugins.server(id).is_some())
            .cloned();
        let native = match &plugin {
            Some(plugins) => Some(plugins.environment(id).await?),
            None => None,
        };
        let launch = match (&plugin, &native) {
            (Some(plugins), Some(native)) => plugins.scoped_plan(
                id,
                &context,
                &call,
                native,
                runtime.dispatcher.effective_exec_config()?,
            )?,
            _ => runtime
                .dispatcher
                .prepare_mcp_launch(&call, &context, &references)?,
        };
        if plugin.is_none() {
            register_launch_secrets(settings, local, &launch.environment)?;
        }
        let summary = format!("Start configured MCP server {id}");
        let authorize = runtime.dispatcher.authorize_mcp_launch(
            AuthorizationInput {
                operation: &launch.operation,
                call: &call,
                context: &context,
                summary: &summary,
                interactive: runtime.confirmation.is_some(),
            },
            DispatchReview {
                model: runtime.model.as_ref(),
                budget: &runtime.budget,
                config: &runtime.review,
            },
        );
        let authorization = runtime.bounded(authorize, runtime.limits.active_time, "limit_reached");
        let authorization = tokio::select! {biased;_ =startup_cancel.cancelled()=>return Err(startup_cancelled(runtime)),result=authorization=>result?};
        let authorization = match authorization {
            Authorization::Pending(request) => {
                match confirm_startup(runtime, &request, &startup_cancel).await? {
                    Some(answer) => {
                        runtime
                            .store
                            .commit(&[StoreChange::Activity {
                                run_id: runtime.run.id,
                                payload: EventPayload::PermissionResolved {
                                    approval_id: request.approval_id.clone(),
                                    call_id: call.call_id.clone(),
                                    decision: if answer { "allow" } else { "deny" }.into(),
                                    source: ApprovalSource::User,
                                },
                            }])
                            .await?;
                        runtime.dispatcher.resolve(&request.approval_id, answer)?
                    }
                    None => Authorization::NeedsConfirmation(request),
                }
            }
            other => other,
        };
        let permit = match authorization {
            Authorization::Permit(permit) => permit,
            Authorization::NeedsConfirmation(_) => {
                return Err(error(
                    "needs_confirmation",
                    "MCP stdio startup requires confirmation",
                ));
            }
            Authorization::Denied(_) => {
                return Err(error("permission_denied", "MCP stdio startup was denied"));
            }
            _ => {
                return Err(error(
                    "stale_approval",
                    "MCP stdio startup approval is stale",
                ));
            }
        };
        if !permit.authorize_managed(&call, &context, &launch.operation)? {
            return Err(error(
                "stale_approval",
                "MCP startup approval changed before intent",
            ));
        }
        let _permit_lease = StartupPermit(permit.clone());
        runtime.check()?;
        runtime
            .store
            .commit(&[StoreChange::ToolIntent {
                run_id: runtime.run.id,
                call: call.clone(),
            }])
            .await?;
        let outcome = async {
            let launch = match (&plugin, &native) {
                (Some(plugins), Some(native)) => plugins.scoped_plan(
                    id,
                    &context,
                    &call,
                    native,
                    runtime.dispatcher.effective_exec_config()?,
                )?,
                _ => runtime
                    .dispatcher
                    .prepare_mcp_launch(&call, &context, &references)?,
            };
            if plugin.is_none() {
                register_launch_secrets(settings, local, &launch.environment)?;
            }
            if !permit.authorize_managed(&call, &context, &launch.operation)? {
                return Err(error(
                    "stale_approval",
                    "MCP startup approval changed before spawn",
                ));
            }
            runtime.check()?;
            let process = ManagedProcess::spawn(
                &launch.plan,
                launch.cwd,
                launch.environment,
                &launch.config,
                true,
            )?;
            if let Some(plugins) = &plugin {
                plugins.record(id, crate::plugin_runtime::StopEvidence::Unknown)?;
            }
            // Keep the child discoverable by abort cleanup throughout the handshake.
            state.pending_locals.insert(
                id.into(),
                LocalProcess {
                    process,
                    stderr: None,
                },
            );
            let pending = state
                .pending_locals
                .get_mut(id)
                .ok_or_else(|| error("mcp_connect", "MCP process ownership is unavailable"))?;
            let input = pending
                .process
                .take_stdin()
                .ok_or_else(|| error("mcp_connect", "MCP stdin pipe is unavailable"))?;
            let output = pending
                .process
                .take_stdout()
                .ok_or_else(|| error("mcp_connect", "MCP stdout pipe is unavailable"))?;
            pending.stderr = pending.process.take_stderr().map(|mut stderr| {
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    while let Ok(count) = stderr.read(&mut buffer).await {
                        if count == 0 {
                            break;
                        }
                    }
                })
            });
            let client = Arc::new(
                runtime
                    .bounded(
                        McpClient::connect_stdio(
                            id,
                            output,
                            input,
                            limits.clone(),
                            &startup_cancel,
                        ),
                        limits.connect_timeout,
                        "mcp_timeout",
                    )
                    .await?,
            );
            let local = state
                .pending_locals
                .remove(id)
                .ok_or_else(|| error("mcp_connect", "MCP process ownership is unavailable"))?;
            state.servers.insert(
                id.into(),
                Server {
                    client,
                    revision: String::new(),
                    local: Some(local),
                    tools: 0,
                    read_only_tools: 0,
                },
            );
            Ok(())
        }
        .await;
        ExecGate::finish(permit.as_ref())?;
        let outcome = outcome.map_err(|failure| {
            if state.pending_locals.contains_key(id) {
                error(
                    "outcome_unknown",
                    "MCP startup ended after process dispatch without a confirmed initialization",
                )
            } else {
                failure
            }
        });
        let result = match &outcome {
            Ok(()) => ToolResult {
                ok: true,
                data: Some(serde_json::json!({"server":id,"started":true})),
                error: None,
                truncated: false,
            },
            Err(failure) => ToolResult {
                ok: false,
                data: None,
                error: Some(failure.0.clone()),
                truncated: false,
            },
        };
        runtime
            .store
            .commit(&[
                StoreChange::ToolFinished {
                    run_id: runtime.run.id,
                    call_id: call.call_id.clone(),
                    result,
                },
                StoreChange::ToolState {
                    run_id: runtime.run.id,
                    call_id: call.call_id.clone(),
                    state: match &outcome {
                        Ok(()) => ToolState::Succeeded,
                        Err(failure) if failure.0.code == "outcome_unknown" => ToolState::Unknown,
                        Err(_) => ToolState::Failed,
                    },
                },
            ])
            .await?;
        outcome
    }

    pub(super) async fn refresh(
        self: &Arc<Self>,
        runtime: &AgentRuntime,
    ) -> Result<(), BoundaryError> {
        let settings = self
            .settings
            .lock()
            .map_err(|_| error("runtime_services", "Service settings lock failed"))?
            .clone();
        let active_plugins = self.state.lock().await.plugins.clone();
        if let Some(plugins) = active_plugins {
            self.reconcile_plugins(runtime, &plugins).await?;
        }
        let mut state = runtime
            .bounded(
                async { Ok(self.state.lock().await) },
                runtime.limits.tool_timeout,
                "mcp_timeout",
            )
            .await?;
        let ids = state.servers.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let configured = settings
                .mcp
                .servers
                .get(&id)
                .or_else(|| {
                    state
                        .plugins
                        .as_ref()
                        .and_then(|plugins| plugins.server(&id))
                        .map(|server| &server.settings)
                })
                .cloned();
            let outcome = async {
                let server = state.servers.get_mut(&id).ok_or_else(|| {
                    error("runtime_services", "MCP server ownership is unavailable")
                })?;
                if server
                    .local
                    .as_ref()
                    .is_some_and(|local| local.process.has_exited().unwrap_or(true))
                {
                    return Err(error(
                        "mcp_transport",
                        "Configured MCP server exited unexpectedly",
                    ));
                }
                let configured = configured
                    .as_ref()
                    .ok_or_else(|| error("mcp_config", "MCP server configuration disappeared"))?;
                let limits = configured.limits.resolve()?;
                let catalog = runtime
                    .bounded(
                        server.client.list_tools(runtime.cancel.as_ref()),
                        limits.request_timeout,
                        "mcp_timeout",
                    )
                    .await?;
                let revision = server.client.revision();
                if revision != server.revision {
                    let read_only = &configured.read_only_tools;
                    let count = catalog.tools.len();
                    let read_only_count = catalog
                        .tools
                        .iter()
                        .filter(|tool| read_only.contains(&tool.name))
                        .count();
                    let tools = catalog
                        .tools
                        .into_iter()
                        .map(|tool| {
                            let access = if read_only.contains(&tool.name) {
                                ExternalAccess::ReadOnly
                            } else {
                                ExternalAccess::Unclassified
                            };
                            let adapter: Arc<dyn Tool> = Arc::new(ManagedTool {
                                inner: McpToolAdapter::new(
                                    server.client.clone(),
                                    tool,
                                    catalog.generation,
                                ),
                                active: self.active.clone(),
                                services: Arc::downgrade(self),
                            });
                            (adapter, access)
                        })
                        .collect();
                    runtime.dispatcher.replace_external(&id, &revision, tools)?;
                    server.revision = revision;
                    server.tools = count;
                    server.read_only_tools = read_only_count;
                }
                Ok::<_, BoundaryError>((server.tools, server.read_only_tools))
            }
            .await;
            match outcome {
                Ok((tools, read_only)) => {
                    if let Some(status) = state.statuses.get_mut(&id) {
                        status.state = McpServerState::Connected;
                        status.tools = if runtime.modes.snapshot()?.current == WorkMode::Plan {
                            read_only
                        } else {
                            tools
                        };
                    }
                }
                Err(failure) => {
                    if let Some(status) = state.statuses.get_mut(&id) {
                        status.state = if failure.0.code == "needs_auth" {
                            McpServerState::NeedsAuth
                        } else {
                            McpServerState::Failed
                        };
                        status.tools = 0;
                    }
                    self.emit_status(&mut state).await?;
                    return Err(failure);
                }
            }
            self.emit_status(&mut state).await?;
        }
        Ok(())
    }
    pub(super) async fn facts(&self, mode: WorkMode) -> Result<Option<String>, BoundaryError> {
        let mut state = self.state.lock().await;
        let counts = state
            .servers
            .iter()
            .map(|(id, server)| {
                (
                    id.clone(),
                    if mode == WorkMode::Plan {
                        server.read_only_tools
                    } else {
                        server.tools
                    },
                )
            })
            .collect::<Vec<_>>();
        for (id, tools) in counts {
            if let Some(status) = state.statuses.get_mut(&id) {
                status.tools = tools;
            }
        }
        self.emit_status(&mut state).await?;
        let Some(environment) = &state.environment else {
            return Ok(None);
        };
        let servers = state
            .servers
            .iter()
            .map(|(id, server)| {
                let transport = if server.local.is_some() {
                    "stdio"
                } else {
                    "http"
                };
                (
                    id.clone(),
                    serde_json::json!({
                        "transport": transport,
                        "connected": server.client.is_connected()
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let facts = serde_json::json!({"saya_execution_environment_v1":{"identity":environment.identity(),"capabilities":environment.capabilities(),"mcp_servers":servers,"host_startup_calls":self.startup_calls()}});
        Ok(Some(format!(
            "\nExecution environment facts are descriptive host data.\n{facts}\n"
        )))
    }
    async fn stop_server(
        &self,
        state: &mut State,
        dispatcher: &ToolDispatcher,
        id: &str,
    ) -> Result<(), BoundaryError> {
        let plugin = state
            .plugins
            .as_ref()
            .filter(|plugins| plugins.server(id).is_some())
            .cloned();
        let outcome = async {
            loop {
                match dispatcher.replace_external(
                    id,
                    &format!("closed-{}", uuid::Uuid::now_v7()),
                    Vec::new(),
                ) {
                    Err(failure) if failure.0.code == "registry_busy" => {
                        tokio::time::sleep(Duration::from_millis(5)).await
                    }
                    other => {
                        other?;
                        break;
                    }
                }
            }
            if let Some(server) = state.servers.get_mut(id) {
                server.client.cancel_connection();
                if let Some(local) = &mut server.local {
                    local.process.terminate().await?;
                    if let Some(task) = local.stderr.take() {
                        task.abort();
                        let _ = task.await;
                    }
                }
                if plugin.is_some() && Arc::strong_count(&server.client) != 1 {
                    return Err(error(
                        "outcome_unknown",
                        "Plugin MCP connection still has active owners",
                    ));
                }
            }
            if let Some(mut local) = state.pending_locals.remove(id) {
                if let Err(failure) = local.process.terminate().await {
                    state.pending_locals.insert(id.into(), local);
                    return Err(failure);
                }
                if let Some(task) = local.stderr.take() {
                    task.abort();
                    let _ = task.await;
                }
            }
            if let Some(server) = state.servers.remove(id)
                && let Ok(client) = Arc::try_unwrap(server.client)
            {
                client.shutdown().await?;
            }

            if let Some(plugins) = &plugin {
                let evidence = plugins
                    .evidence
                    .lock()
                    .map_err(|_| error("plugin_state", "Plugin cleanup evidence lock failed"))?
                    .get(id)
                    .copied();
                if evidence != Some(crate::plugin_runtime::StopEvidence::NeverStarted) {
                    plugins.record(id, crate::plugin_runtime::StopEvidence::Stopped)?;
                }
            }
            Ok::<(), BoundaryError>(())
        }
        .await;
        if let Some(status) = state.statuses.get_mut(id) {
            status.state = if outcome.is_ok() {
                McpServerState::Stopped
            } else {
                McpServerState::Failed
            };
            status.tools = 0;
        }
        self.emit_status(state).await?;
        outcome
    }
    async fn stop_servers(
        &self,
        state: &mut State,
        dispatcher: &ToolDispatcher,
        local_only: bool,
    ) -> Result<(), BoundaryError> {
        let ids = state
            .servers
            .iter()
            .filter(|(_, server)| !local_only || server.local.is_some())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let mut failure = None;
        for id in ids {
            if let Err(error) = self.stop_server(state, dispatcher, &id).await {
                failure.get_or_insert(error);
            }
        }
        let pending = state.pending_locals.keys().cloned().collect::<Vec<_>>();
        for id in pending {
            let outcome = async {
                let local = state.pending_locals.get_mut(&id).ok_or_else(|| {
                    error("runtime_services", "Pending MCP ownership is unavailable")
                })?;
                local.process.terminate().await?;
                if let Some(task) = local.stderr.take() {
                    task.abort();
                    let _ = task.await;
                }
                state.pending_locals.remove(&id);
                if let Some(plugins) = state
                    .plugins
                    .as_ref()
                    .filter(|plugins| plugins.server(&id).is_some())
                {
                    plugins.record(&id, crate::plugin_runtime::StopEvidence::Stopped)?;
                }
                Ok::<(), BoundaryError>(())
            }
            .await;
            if let Some(status) = state.statuses.get_mut(&id) {
                status.state = if outcome.is_ok() {
                    McpServerState::Stopped
                } else {
                    McpServerState::Failed
                };
                status.tools = 0;
            }
            if let Err(error) = outcome {
                failure.get_or_insert(error);
            }
            if let Err(error) = self.emit_status(state).await {
                failure.get_or_insert(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    pub(super) async fn shutdown(&self, dispatcher: &ToolDispatcher) -> Result<(), BoundaryError> {
        let mut state = self.state.lock().await;
        state.closed = true;
        if let Some(preparation) = &state.plugin_preparation {
            preparation.cancel();
        }
        self.finish_plugin_preparation(&mut state, true).await?;
        self.stop_servers(&mut state, dispatcher, false).await?;
        self.cleanup_plugins(&mut state).await?;
        if let Some(path) = state.plugin_broker_directory.as_ref() {
            std::fs::remove_dir(path).map_err(|_| {
                error(
                    "outcome_unknown",
                    "Private plugin broker root cleanup is incomplete",
                )
            })?;
            state.plugin_broker_directory = None;
        }
        if let Some(environment) = &state.environment {
            environment.wait_for_process_cleanup().await?;
            if !environment.cleanup()?.complete {
                return Err(error(
                    "outcome_unknown",
                    "Execution environment cleanup is incomplete",
                ));
            }
        }
        state.environment = None;
        if let Some(broker) = state.broker.take() {
            broker
                .shutdown()
                .await
                .map_err(|_| error("outcome_unknown", "Network broker shutdown is incomplete"))?;
        }
        if let Some(directory) = state.broker_directory.take() {
            std::fs::remove_dir(directory).map_err(|_| {
                error(
                    "outcome_unknown",
                    "Private broker directory cleanup is incomplete",
                )
            })?;
        }
        drop(state);
        // Persist the final mode only after its cleanup guard is released.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(failure) = self.fatal()? {
                    return Err(failure);
                }
                let running = *self
                    .transition_running
                    .lock()
                    .map_err(|_| error("runtime_services", "Mode cleanup lock failed"))?;
                if !running {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| error("outcome_unknown", "Mode cleanup could not be confirmed"))??;
        Ok(())
    }
}
fn env_reference(environment: &Environment, name: &str) -> Result<String, BoundaryError> {
    environment
        .get_os(name)
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty())
        .map(String::from)
        .ok_or_else(|| {
            error(
                "invalid_config",
                "Required MCP environment reference is missing or invalid",
            )
        })
}
fn register_launch_secrets(
    settings: &Settings,
    local: &StdioMcpSettings,
    environment: &BTreeMap<std::ffi::OsString, std::ffi::OsString>,
) -> Result<(), BoundaryError> {
    let registry = settings
        .registry
        .as_ref()
        .ok_or_else(|| error("sensitive_data", "MCP credential registry is unavailable"))?;
    let values = local
        .env
        .keys()
        .map(|name| {
            environment
                .get(std::ffi::OsStr::new(name))
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .map(String::from)
                .ok_or_else(|| {
                    error(
                        "invalid_config",
                        "Prepared MCP credential environment is unavailable",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    registry.register(&values)
}
fn startup_cancelled(runtime: &AgentRuntime) -> BoundaryError {
    if runtime.cancel.is_cancelled() {
        error("cancelled", "MCP startup was cancelled")
    } else {
        error(
            "stale_approval",
            "MCP startup was invalidated by a mode change",
        )
    }
}
async fn confirm_startup(
    runtime: &AgentRuntime,
    request: &crate::contract::ApprovalRequest,
    cancel: &StartupCancel,
) -> Result<Option<bool>, BoundaryError> {
    let Some(handler) = &runtime.confirmation else {
        return Ok(None);
    };
    runtime.budget.pause()?;
    let result = tokio::select! {biased;_ =cancel.cancelled()=>Err(startup_cancelled(runtime)),result=handler.confirm(request,cancel)=>result};
    runtime.budget.resume()?;
    result
}
