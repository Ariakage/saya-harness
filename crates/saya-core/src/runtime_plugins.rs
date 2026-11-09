//! Per-Run plugin resources. Descriptors retain their capability provenance all
//! the way to native process preparation and current-policy authorization.
use super::*;
use crate::{
    boundary::Model,
    execution::{BackendKind, CapabilityLevel},
    hook_store::HookStore,
    hooks::{EnabledHook, HookBatchOutcome, HookContext, HookEngine, HookEvent, HookLimits},
    plugin_runtime::{
        ActivePlugins, EffectiveCeiling, PluginHookDescriptor, PluginHostBindings,
        PluginRunContext, PluginServerDescriptor, ProcessPolicy, StopEvidence, ViewPool,
    },
    plugin_store::PluginStore,
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::{ExecConfig, ExecTool},
        read::{ReadLimits, ReadTools},
    },
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[path = "runtime_plugins/hooks.rs"]
mod hook_executor;
#[cfg(test)]
#[path = "runtime_plugins/tests.rs"]
mod tests;

#[derive(Clone)]
pub(super) struct PluginConfiguration {
    pub(super) store: PluginStore,
    pub(super) pool: Arc<ViewPool>,
    pub(super) bindings: PluginHostBindings,
    pub(super) journal: Arc<HookStore>,
    pub(super) broker_root: PathBuf,
}

#[derive(Default)]
struct NativeResource {
    environment: Option<Arc<PreparedEnvironment>>,
    preparation: Option<JoinHandle<Result<PreparedEnvironment, BoundaryError>>>,
    broker: Option<BrokerHandle>,
    broker_shutdown: Option<JoinHandle<Result<(), BoundaryError>>>,
    broker_directory: Option<PathBuf>,
    cleanup_unknown: bool,
}
type NativeResources = Mutex<BTreeMap<String, Arc<AsyncMutex<NativeResource>>>>;

/// A Run keeps initial preparation owned before its blocking worker can create
/// a package lease or view. Awaiting this handle by reference is cancellation-safe.
pub(super) struct PluginPreparation {
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<PluginPreparationOutcome>>,
    unknown: bool,
}
impl PluginPreparation {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
impl Drop for PluginPreparation {
    fn drop(&mut self) {
        // Drop cannot await the worker or establish cleanup evidence.
        self.cancel();
    }
}

enum PluginPreparationOutcome {
    Ready(ActivePlugins),
    Failed {
        failure: BoundaryError,
        cleanup_pending: bool,
    },
}

fn failed_preparation(
    store: &PluginStore,
    pool: &ViewPool,
    run_id: RunId,
    failure: BoundaryError,
) -> PluginPreparationOutcome {
    // prepare() rolls back unstarted resources on error. Its original error
    // does not prove rollback succeeded, so confirm both durable inventories.
    let leases_clean = store.list().is_ok_and(|packages| {
        packages.iter().all(|package| {
            package.instances.iter().all(|lease| {
                lease.run_id != run_id || lease.state == crate::plugin_store::InstanceState::Cleaned
            })
        })
    });
    let views_clean = pool
        .inventory()
        .is_ok_and(|views| views.iter().all(|view| view.identity.run_id != run_id));
    PluginPreparationOutcome::Failed {
        failure,
        cleanup_pending: !leases_clean || !views_clean,
    }
}

pub(super) struct PluginRun {
    active: Arc<Mutex<ActivePlugins>>,
    pub(super) servers: BTreeMap<String, PluginServerDescriptor>,
    hooks: BTreeMap<String, PluginHookDescriptor>,
    resources: NativeResources,
    pub(super) evidence: Mutex<BTreeMap<String, StopEvidence>>,
    enabled_hooks: Mutex<Vec<EnabledHook>>,
    engine: Mutex<Option<Arc<HookEngine>>>,
    pub(super) processes: hook_executor::HookProcesses,
    closing: AtomicBool,
}

impl PluginRun {
    pub(super) fn check(&self, id: &str) -> Result<(), BoundaryError> {
        if self.closing.load(Ordering::Acquire) {
            return Err(error("plugin_revoked", "Plugin Run is closing"));
        }
        self.active
            .lock()
            .map_err(|_| error("plugin_state", "Plugin lease lock failed"))?
            .check_entry(id)
    }
    pub(super) fn server(&self, id: &str) -> Option<&PluginServerDescriptor> {
        self.servers.get(id)
    }
    pub(super) fn record(&self, id: &str, evidence: StopEvidence) -> Result<(), BoundaryError> {
        let mut records = self
            .evidence
            .lock()
            .map_err(|_| error("plugin_state", "Plugin cleanup evidence lock failed"))?;
        let current = records.get_mut(id).ok_or_else(|| {
            error(
                "plugin_identity",
                "Plugin entry cleanup identity is unavailable",
            )
        })?;
        *current = evidence;
        Ok(())
    }
    pub(super) async fn environment(
        &self,
        id: &str,
    ) -> Result<Arc<PreparedEnvironment>, BoundaryError> {
        let resource = self
            .resources
            .lock()
            .map_err(|_| {
                error(
                    "plugin_state",
                    "Plugin native resource registry lock failed",
                )
            })?
            .get(id)
            .cloned()
            .ok_or_else(|| {
                error(
                    "sandbox_required",
                    "Plugin native environment is unavailable",
                )
            })?;
        let mut resource = resource.lock().await;
        finish_preparation(&mut resource).await?;
        resource.environment.clone().ok_or_else(|| {
            error(
                "sandbox_required",
                "Plugin native environment is unavailable",
            )
        })
    }
    pub(super) async fn http_client(
        &self,
        id: &str,
        limits: &crate::mcp::McpLimits,
    ) -> Result<McpHttpClient, BoundaryError> {
        self.check(id)?;
        let resource = self
            .resources
            .lock()
            .map_err(|_| {
                error(
                    "plugin_state",
                    "Plugin native resource registry lock failed",
                )
            })?
            .get(id)
            .cloned()
            .ok_or_else(|| error("plugin_network_scope", "Plugin HTTP broker is unavailable"))?;
        let resource = resource.lock().await;
        let broker = resource
            .broker
            .as_ref()
            .filter(|broker| broker.is_running())
            .ok_or_else(|| error("plugin_network_scope", "Plugin HTTP broker has stopped"))?;
        McpHttpClient::new_scoped_proxy(limits, broker.tcp_endpoint())
    }
    pub(super) fn scoped_plan(
        &self,
        id: &str,
        context: &ToolContext,
        call: &ToolCall,
        environment: &PreparedEnvironment,
        base: ExecConfig,
    ) -> Result<crate::dispatch::McpLaunch, BoundaryError> {
        self.check(id)?;
        let args: crate::tools::exec::ExecArgs =
            serde_json::from_str(&call.arguments).map_err(|_| {
                error(
                    "plugin_runtime_scope",
                    "Plugin process arguments are invalid",
                )
            })?;
        let (argv, cwd) =
            if let Some(server) = self.servers.get(id) {
                let McpTransportSettings::Stdio(local) = &server.settings.transport else {
                    return Err(error(
                        "plugin_identity",
                        "Plugin entry has no process binding",
                    ));
                };
                (
                    &local.argv,
                    local.cwd.as_ref().ok_or_else(|| {
                        error("plugin_identity", "Plugin process cwd is unavailable")
                    })?,
                )
            } else {
                let hook = self.hooks.get(id).ok_or_else(|| {
                    error("plugin_identity", "Plugin process entry is unavailable")
                })?;
                (&hook.argv, &hook.cwd)
            };
        if &args.argv != argv || args.cwd.as_ref() != Some(cwd) || args.timeout_seconds.is_some() {
            return Err(error(
                "plugin_runtime_scope",
                "Plugin launch differs from its host-selected argv or cwd",
            ));
        }
        if let Some(server) = self.servers.get(id) {
            let process = server
                .process
                .as_ref()
                .ok_or_else(|| error("plugin_identity", "Plugin entry has no process binding"))?;
            return prepare_process(
                context,
                call,
                ProcessBinding {
                    process,
                    ceiling: &server.ceiling,
                    identity: json!({"identity":server.identity,"lease_id":server.lease_id,"fingerprint":server.capability_fingerprint}),
                },
                server.environment(),
                environment,
                base,
            );
        }
        let hook = self
            .hooks
            .get(id)
            .ok_or_else(|| error("plugin_identity", "Plugin process entry is unavailable"))?;
        prepare_process(
            context,
            call,
            ProcessBinding {
                process: &hook.process,
                ceiling: &hook.ceiling,
                identity: json!({"identity":hook.identity,"lease_id":hook.lease_id,"event":hook.event,"kind":hook.kind}),
            },
            hook.environment(),
            environment,
            base,
        )
    }
}

/// The native environment, explicit references, descriptor and lease identity
/// are all host data. They must never be accepted from model tool arguments.
struct ProcessBinding<'a> {
    process: &'a ProcessPolicy,
    ceiling: &'a EffectiveCeiling,
    identity: Value,
}
fn prepare_process(
    context: &ToolContext,
    call: &ToolCall,
    binding: ProcessBinding<'_>,
    environment: &Environment,
    execution: &PreparedEnvironment,
    mut config: ExecConfig,
) -> Result<crate::dispatch::McpLaunch, BoundaryError> {
    let ProcessBinding {
        process,
        ceiling,
        identity,
    } = binding;
    if process.inherit_host_environment
        || !process.native_isolation_required
        || execution.identity().backend == BackendKind::Host
        || execution.capabilities().filesystem != CapabilityLevel::Hard
        || execution.capabilities().network != CapabilityLevel::Hard
    {
        return Err(error(
            "sandbox_required",
            "Plugin processes require native filesystem and network isolation",
        ));
    }
    let mut roots = process.read_roots.clone();
    roots.extend(process.write_roots.iter().cloned());
    let files = Arc::new(
        ReadTools::new(&context.workspace, &roots, false, ReadLimits::default()).map_err(|_| {
            error(
                "plugin_runtime_scope",
                "Plugin launch roots are unavailable",
            )
        })?,
    );
    files
        .protect_paths(process.protected_roots.clone())
        .map_err(|_| {
            error(
                "plugin_runtime_scope",
                "Plugin private path protection failed",
            )
        })?;
    config.environment = environment.clone();
    config.inherit_extra.clear();
    config.secret_env_names.extend(
        environment
            .iter()
            .filter_map(|(name, _)| name.to_str().map(str::to_owned)),
    );
    config.execution = Some(execution.clone());
    // The planner needs no side-effect store or executable authorization. The
    // actual dispatcher issues a one-call permit against the scoped operation.
    let planner = ExecTool::new(
        files,
        Arc::new(crate::memory::MemoryStore::default()),
        Arc::new(NoProcessPermit),
        config.clone(),
    );
    let refs = process.environment_references.values().cloned().collect();
    let (plan, cwd, resolved) = planner.prepare_host_launch(call, context, &refs)?;
    let mut expected = process
        .environment_references
        .iter()
        .map(|(name, reference)| {
            environment
                .get_os(reference)
                .map(|value| (OsString::from(name), value.to_owned()))
                .ok_or_else(|| {
                    error(
                        "plugin_environment",
                        "Plugin explicit environment reference is unavailable",
                    )
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    execution.apply_environment(&mut expected)?;
    if plan.argv.first().map(Path::new) != Some(process.program.as_path()) || resolved != expected {
        return Err(error(
            "plugin_runtime_scope",
            "Plugin process differs from its host descriptor",
        ));
    }
    let keys = resolved
        .keys()
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let operation = crate::permissions::Operation::plugin_exec(
        &plan,
        &keys,
        json!({"entry":identity,"process":process,"ceiling":ceiling,"native_environment":execution.identity()}),
    )?;
    Ok(crate::dispatch::McpLaunch {
        plan,
        cwd,
        environment: resolved,
        config,
        operation,
    })
}
struct NoProcessPermit;
impl ExecGate for NoProcessPermit {
    fn authorize(
        &self,
        _: &crate::tools::exec::ExecPlan,
        _: &ToolCall,
        _: &ToolContext,
    ) -> Result<bool, BoundaryError> {
        Ok(false)
    }
}

fn intersect_broker(
    ceiling: &EffectiveCeiling,
    base: Option<&BrokerPolicy>,
) -> Result<Option<BrokerPolicy>, BoundaryError> {
    if !ceiling.capabilities.contains("network.outbound") || ceiling.network_origins.is_empty() {
        return Ok(None);
    }
    let Some(base) = base else {
        return Ok(None);
    };
    base.validate()
        .map_err(|_| error("plugin_network_scope", "Host network policy is invalid"))?;
    let approved = base
        .allowed_origins
        .iter()
        .map(|origin| crate::network::NormalizedOrigin::parse(origin))
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|_| error("plugin_network_scope", "Host network origin is invalid"))?;
    let mut policy = base.clone();
    policy.allowed_origins = ceiling
        .network_origins
        .iter()
        .filter_map(|origin| {
            crate::network::NormalizedOrigin::parse(origin)
                .ok()
                .filter(|origin| approved.contains(origin))
                .map(|_| origin.clone())
        })
        .collect();
    policy.localhost_development_origin = base
        .localhost_development_origin
        .as_ref()
        .filter(|origin| {
            let development = crate::network::NormalizedOrigin::parse(origin).ok();
            policy.allowed_origins.iter().any(|allowed| {
                development.is_some()
                    && crate::network::NormalizedOrigin::parse(allowed).ok() == development
            })
        })
        .cloned();
    policy.unix_socket = None;
    if policy.allowed_origins.is_empty() {
        return Ok(None);
    }
    policy.validate().map_err(|_| {
        error(
            "plugin_network_scope",
            "Plugin network intersection is invalid",
        )
    })?;
    Ok(Some(policy))
}
fn validate_http(
    server: &PluginServerDescriptor,
    broker: Option<&BrokerPolicy>,
) -> Result<(), BoundaryError> {
    let McpTransportSettings::Http(http) = &server.settings.transport else {
        return Ok(());
    };
    let policy = intersect_broker(&server.ceiling, broker)?.ok_or_else(|| {
        error(
            "plugin_network_scope",
            "Plugin HTTP endpoint is outside the host network ceiling",
        )
    })?;
    let endpoint = crate::network::normalize_target(&http.endpoint)
        .map_err(|_| error("plugin_network_scope", "Plugin HTTP endpoint is invalid"))?;
    policy.check_origin(&endpoint.origin).map_err(|_| {
        error(
            "plugin_network_scope",
            "Plugin HTTP endpoint is outside the enabled origin intersection",
        )
    })?;
    // Existing OAuth adapters may contact a separate issuer for refresh. Until
    // they accept this exact ceiling, they cannot serve a plugin credential.
    if http.credential_ref.is_some() {
        return Err(error(
            "plugin_credential_scope_unsupported",
            "Plugin stored credential transport requires an origin-bound authorization adapter",
        ));
    }
    Ok(())
}

fn validate_aggregate_resources(
    base: &SandboxPolicy,
    has_process: bool,
) -> Result<(), BoundaryError> {
    if has_process
        && (base.resources.total_memory_bytes.is_some()
            || base.resources.total_processes.is_some()
            || base.resources.cpu_quota.is_some())
    {
        return Err(error(
            "unsupported_resource_policy",
            "Plugin process environments cannot preserve the configured aggregate Run resource limit",
        ));
    }
    Ok(())
}

async fn prepare_http(
    plugins: &PluginRun,
    id: &str,
    ceiling: &EffectiveCeiling,
    base: Option<&BrokerPolicy>,
    budget: Option<Arc<crate::network::RunNetworkBudget>>,
    cancel: Arc<dyn Cancellation>,
) -> Result<(), BoundaryError> {
    let policy = intersect_broker(ceiling, base)?.ok_or_else(|| {
        error(
            "plugin_network_scope",
            "Plugin HTTP origin intersection is empty",
        )
    })?;
    let shared =
        budget.ok_or_else(|| error("plugin_network_scope", "Run network budget is unavailable"))?;
    let resource = Arc::new(AsyncMutex::new(NativeResource::default()));
    plugins
        .resources
        .lock()
        .map_err(|_| {
            error(
                "plugin_state",
                "Plugin native resource registry lock failed",
            )
        })?
        .insert(id.into(), resource.clone());
    // The host SDK uses the loopback TCP listener, which exists alongside the
    // optional Unix bridge. HTTP-only entries need no native bridge socket.
    let mut resource = resource.lock().await;
    resource.broker = Some(
        NetworkBroker::start_scoped(policy, cancel, shared)
            .await
            .map_err(|_| error("plugin_network_scope", "Plugin HTTP broker could not start"))?,
    );
    Ok(())
}

struct NativePreparation<'a> {
    id: &'a str,
    process: &'a ProcessPolicy,
    ceiling: &'a EffectiveCeiling,
    cwd: PathBuf,
    base: &'a SandboxPolicy,
    broker: Option<&'a BrokerPolicy>,
    broker_root: &'a Path,
    network_budget: Option<Arc<crate::network::RunNetworkBudget>>,
}
async fn prepare_native(
    plugins: &PluginRun,
    input: NativePreparation<'_>,
    cancel: Arc<dyn Cancellation>,
) -> Result<(), BoundaryError> {
    let NativePreparation {
        id,
        process,
        ceiling,
        cwd,
        base,
        broker,
        broker_root,
        network_budget,
    } = input;
    if base.backend == BackendKind::Host {
        return Err(error(
            "sandbox_required",
            "Plugin processes require a native backend",
        ));
    }
    let mut policy = base.clone();
    policy.workspace = cwd;
    policy.read_roots = process.read_roots.clone();
    policy.write_roots = process.write_roots.clone();
    policy.protected_paths = process.protected_roots.clone();
    policy.network = NetworkPolicy::Deny;
    let resource = Arc::new(AsyncMutex::new(NativeResource::default()));
    plugins
        .resources
        .lock()
        .map_err(|_| {
            error(
                "plugin_state",
                "Plugin native resource registry lock failed",
            )
        })?
        .insert(id.into(), resource.clone());
    // The Run owns this record before the first await that can allocate a
    // listener or native directory. Aborted initialization remains retryable.
    let mut resource = resource.lock().await;
    #[cfg(not(target_os = "linux"))]
    let _ = broker_root;
    if let Some(allowed) = intersect_broker(ceiling, broker)? {
        #[cfg(target_os = "linux")]
        let allowed = {
            use std::os::unix::fs::DirBuilderExt;
            let path = broker_root.join(format!("e-{}", uuid::Uuid::now_v7().simple()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .map_err(|_| {
                    error(
                        "sandbox_unavailable",
                        "Private plugin broker directory could not be created",
                    )
                })?;
            resource.broker_directory = Some(path.clone());
            let mut allowed = allowed;
            allowed.unix_socket = Some(path.join("p"));
            allowed
        };
        let shared = network_budget
            .ok_or_else(|| error("plugin_network_scope", "Run network budget is unavailable"))?;
        let broker = NetworkBroker::start_scoped(allowed, cancel, shared)
            .await
            .map_err(|_| {
                error(
                    "sandbox_unavailable",
                    "Plugin network broker could not start",
                )
            })?;
        policy.network = NetworkPolicy::Proxy(ProxyEndpoint {
            endpoint: broker.tcp_endpoint(),
            unix_socket: broker.unix_socket().map(Path::to_path_buf),
        });
        resource.broker = Some(broker);
    }
    resource.preparation = Some(tokio::task::spawn_blocking(move || {
        crate::execution::prepare(&policy)
    }));
    finish_preparation(&mut resource).await
}
async fn finish_preparation(resource: &mut NativeResource) -> Result<(), BoundaryError> {
    if let Some(worker) = &mut resource.preparation {
        let result = worker.await;
        resource.preparation = None;
        match result {
            Ok(Ok(environment)) => resource.environment = Some(Arc::new(environment)),
            Ok(Err(failure)) => return Err(failure),
            Err(_) => {
                resource.cleanup_unknown = true;
                return Err(error(
                    "outcome_unknown",
                    "Plugin native preparation worker failed",
                ));
            }
        }
    }
    Ok(())
}

impl RuntimeServices {
    pub(super) async fn finish_plugin_preparation(
        &self,
        state: &mut State,
        shutdown: bool,
    ) -> Result<(), BoundaryError> {
        let Some(preparation) = state.plugin_preparation.as_mut() else {
            return Ok(());
        };
        if preparation.unknown {
            return Err(error(
                "outcome_unknown",
                "Initial plugin preparation requires independent reconciliation",
            ));
        }
        let Some(worker) = &mut preparation.worker else {
            preparation.unknown = true;
            return Err(error(
                "outcome_unknown",
                "Initial plugin preparation ownership is unavailable",
            ));
        };
        let completed = worker.await;
        preparation.worker = None;
        match completed {
            Ok(PluginPreparationOutcome::Ready(active)) => {
                let servers = active.servers().clone();
                let hooks = active.hooks().clone();
                let evidence = servers
                    .keys()
                    .chain(hooks.keys())
                    .map(|id| (id.clone(), StopEvidence::NeverStarted))
                    .collect();
                // No await separates taking the worker result and adopting
                // its resources. Abort cleanup can always find the next owner.
                state.plugins = Some(Arc::new(PluginRun {
                    active: Arc::new(Mutex::new(active)),
                    servers,
                    hooks,
                    resources: Mutex::new(BTreeMap::new()),
                    evidence: Mutex::new(evidence),
                    enabled_hooks: Mutex::new(Vec::new()),
                    engine: Mutex::new(None),
                    processes: Default::default(),
                    closing: AtomicBool::new(false),
                }));
                state.plugin_preparation = None;
                Ok(())
            }
            Ok(PluginPreparationOutcome::Failed {
                failure,
                cleanup_pending: false,
            }) => {
                state.plugin_preparation = None;
                if shutdown { Ok(()) } else { Err(failure) }
            }
            Ok(PluginPreparationOutcome::Failed {
                cleanup_pending: true,
                ..
            })
            | Err(_) => {
                preparation.unknown = true;
                Err(error(
                    "outcome_unknown",
                    "Initial plugin preparation cleanup remains unconfirmed",
                ))
            }
        }
    }

    pub(in crate::runtime) fn configure_plugins(
        &self,
        store: PluginStore,
        pool: Arc<ViewPool>,
        bindings: PluginHostBindings,
        journal: Arc<HookStore>,
    ) -> Result<(), BoundaryError> {
        if self.begun.load(Ordering::Acquire) {
            return Err(error(
                "invalid_run",
                "Plugin settings cannot change during a Run",
            ));
        }
        let mut settings = self
            .settings
            .lock()
            .map_err(|_| error("runtime_services", "Service settings lock failed"))?;
        if settings.plugins.is_some() {
            return Err(error(
                "invalid_config",
                "Plugin runtime is already configured",
            ));
        }
        let parent = pool
            .path()
            .parent()
            .ok_or_else(|| error("plugin_view_scope", "Plugin view parent is unavailable"))?;
        let broker_root = parent.join(format!("spb-{}", uuid::Uuid::now_v7().simple()));
        settings.plugins = Some(PluginConfiguration {
            store,
            pool,
            bindings,
            journal,
            broker_root,
        });
        self.local_configured.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) async fn initialize_plugins(
        self: &Arc<Self>,
        runtime: &AgentRuntime,
        session: &Session,
        settings: &Settings,
        state: &mut State,
    ) -> Result<(), BoundaryError> {
        let Some(config) = &settings.plugins else {
            return Ok(());
        };
        let (base, broker) = settings.execution.as_ref().ok_or_else(|| {
            error(
                "sandbox_required",
                "Plugin runtime requires host execution policy",
            )
        })?;
        let registry = settings.registry.clone().ok_or_else(|| {
            error(
                "sensitive_data",
                "Plugin credential registry is unavailable",
            )
        })?;
        let mut protected = base.protected_paths.clone();
        for path in [
            config.store.protected_path(),
            config.journal.protected_path(),
            config.broker_root.as_path(),
        ] {
            if !protected.contains(&path.to_path_buf()) {
                protected.push(path.to_path_buf());
            }
        }
        runtime.dispatcher.protect_host_paths(vec![
            config.store.protected_path().into(),
            config.journal.protected_path().into(),
            config.pool.path().into(),
            config.broker_root.clone(),
        ])?;
        let mut features = BTreeSet::from([
            "plugin-manifest-v1".into(),
            "mcp-stdio-v1".into(),
            "mcp-http-v1".into(),
            "hook-events-v1".into(),
        ]);
        if runtime.skill_state.is_some() {
            features.insert("skill-directory-v1".into());
        }
        let context = PluginRunContext {
            workspace: session.workspace.clone(),
            session_id: session.id,
            run_id: runtime.run.id,
            host_version: env!("CARGO_PKG_VERSION").into(),
            features,
            native_isolation_available: state.environment.as_ref().is_some_and(|native| {
                native.identity().backend != BackendKind::Host
                    && native.capabilities().filesystem == CapabilityLevel::Hard
                    && native.capabilities().network == CapabilityLevel::Hard
            }),
            protected_roots: protected,
            environment: settings.environment.clone(),
            secrets: registry.clone(),
            host_write_roots: base.write_roots.clone(),
        };
        let store = config.store.clone();
        let pool = config.pool.clone();
        let bindings = config.bindings.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let worker = tokio::task::spawn_blocking(move || {
            match ActivePlugins::prepare(&store, &context, &bindings, pool.clone(), &worker_cancel)
            {
                Ok(active) => PluginPreparationOutcome::Ready(active),
                Err(failure) => failed_preparation(&store, &pool, context.run_id, failure),
            }
        });
        state.plugin_preparation = Some(PluginPreparation {
            cancelled: cancelled.clone(),
            worker: Some(worker),
            unknown: false,
        });
        {
            let completion = self.finish_plugin_preparation(state, false);
            tokio::pin!(completion);
            tokio::select! {
                biased;
                _ = runtime.cancel.cancelled() => {
                    cancelled.store(true, Ordering::Release);
                    completion.await?;
                }
                result = &mut completion => result?,
            }
        }
        runtime.check()?;
        let plugins = state.plugins.clone().ok_or_else(|| {
            error(
                "plugin_state",
                "Prepared plugin resources have no Run owner",
            )
        })?;
        validate_aggregate_resources(
            base,
            plugins
                .servers
                .values()
                .any(|server| server.process.is_some())
                || !plugins.hooks.is_empty(),
        )?;
        let leases = plugins
            .active
            .lock()
            .map_err(|_| error("plugin_state", "Plugin lease lock failed"))?
            .skill_leases();
        let enabled = plugins
            .hooks
            .values()
            .map(|hook| {
                let lease = leases
                    .iter()
                    .find(|lease| lease.record().lease_id == hook.lease_id)
                    .ok_or_else(|| error("plugin_identity", "Hook lease is unavailable"))?;
                EnabledHook::from_lease(lease.clone(), &hook.identity.entry_id)
            })
            .collect::<Result<Vec<_>, _>>()?;
        *plugins
            .enabled_hooks
            .lock()
            .map_err(|_| error("plugin_state", "Plugin hook catalog lock failed"))? = enabled;
        if runtime.skill_state.is_some() {
            runtime.set_plugin_skill_leases(leases)?;
        } else {
            drop(leases);
        }
        for (id, server) in &plugins.servers {
            validate_http(server, broker.as_ref())?;
            if let Some(process) = &server.process {
                let McpTransportSettings::Stdio(local) = &server.settings.transport else {
                    return Err(error(
                        "plugin_identity",
                        "Plugin process transport binding is invalid",
                    ));
                };
                let cwd = local
                    .cwd
                    .clone()
                    .ok_or_else(|| error("plugin_identity", "Plugin process cwd is unavailable"))?;
                prepare_native(
                    &plugins,
                    NativePreparation {
                        id,
                        process,
                        ceiling: &server.ceiling,
                        cwd,
                        base,
                        broker: broker.as_ref(),
                        broker_root: &config.broker_root,
                        network_budget: state.network_budget.clone(),
                    },
                    runtime.cancel.clone(),
                )
                .await?;
            } else {
                prepare_http(
                    &plugins,
                    id,
                    &server.ceiling,
                    broker.as_ref(),
                    state.network_budget.clone(),
                    runtime.cancel.clone(),
                )
                .await?;
            }
        }
        for (id, hook) in &plugins.hooks {
            prepare_native(
                &plugins,
                NativePreparation {
                    id,
                    process: &hook.process,
                    ceiling: &hook.ceiling,
                    cwd: hook.cwd.clone(),
                    base,
                    broker: broker.as_ref(),
                    broker_root: &config.broker_root,
                    network_budget: state.network_budget.clone(),
                },
                runtime.cancel.clone(),
            )
            .await?;
        }
        let executor = Arc::new(hook_executor::NativeHookExecutor::new(
            runtime,
            session,
            Arc::downgrade(self),
            plugins.clone(),
        ));
        let engine = Arc::new(HookEngine::new(
            runtime.run.clone(),
            HookLimits::default(),
            registry,
            executor,
            config.journal.clone(),
        )?);
        *plugins
            .engine
            .lock()
            .map_err(|_| error("plugin_state", "Plugin hook engine lock failed"))? = Some(engine);
        Ok(())
    }

    pub(in crate::runtime) async fn fire_hooks(
        self: &Arc<Self>,
        runtime: &AgentRuntime,
        session: &Session,
        event: HookEvent,
        event_id: String,
        parent_call_id: Option<String>,
        data: Value,
    ) -> Result<HookBatchOutcome, BoundaryError> {
        let plugins = self.state.lock().await.plugins.clone();
        let Some(plugins) = plugins else {
            return Ok(HookBatchOutcome::default());
        };
        self.reconcile_plugins(runtime, &plugins).await?;
        let engine = plugins
            .engine
            .lock()
            .map_err(|_| error("plugin_state", "Plugin hook engine lock failed"))?
            .clone();
        let Some(engine) = engine else {
            return Ok(HookBatchOutcome::default());
        };
        let hooks = plugins
            .enabled_hooks
            .lock()
            .map_err(|_| error("plugin_state", "Plugin hook catalog lock failed"))?
            .clone();
        let snapshot = runtime.modes.snapshot()?;
        let hook_cancel = StartupCancel {
            root: runtime.cancel.clone(),
            modes: runtime.modes.clone(),
            revision: snapshot.revision,
            current: snapshot.current,
        };
        let mut run = runtime.run.clone();
        run.work_mode = snapshot.current;
        if session.id != run.session_id {
            return Err(error("hook_identity", "Hook session differs from its Run"));
        }
        let context = HookContext {
            root_event_id: event_id.clone(),
            event_id,
            event,
            run,
            parent_call_id,
            depth: 0,
            ancestry: Vec::new(),
            data,
        };
        engine.fire(&context, &hooks, &hook_cancel).await
    }
    pub(super) async fn check_plugin_entry(&self, id: &str) -> Result<(), BoundaryError> {
        let plugins = self.state.lock().await.plugins.clone();
        if let Some(plugins) = plugins
            && (plugins.servers.contains_key(id) || plugins.hooks.contains_key(id))
        {
            plugins.check(id)?;
        }
        Ok(())
    }
    pub(super) async fn reconcile_plugins(
        &self,
        runtime: &AgentRuntime,
        plugins: &Arc<PluginRun>,
    ) -> Result<(), BoundaryError> {
        let revoked = plugins
            .active
            .lock()
            .map_err(|_| error("plugin_state", "Plugin lease lock failed"))?
            .reconcile()?
            .revoked;
        if revoked.is_empty() {
            return Ok(());
        }
        let ids = revoked
            .iter()
            .flat_map(|plugin| plugin.server_ids.iter().chain(&plugin.hook_ids))
            .cloned()
            .collect::<BTreeSet<_>>();
        plugins
            .enabled_hooks
            .lock()
            .map_err(|_| error("plugin_state", "Plugin hook catalog lock failed"))?
            .retain(|hook| {
                !revoked
                    .iter()
                    .any(|plugin| plugin.lease.lease_id == hook.lease().record().lease_id)
            });
        if runtime.skill_state.is_some() {
            runtime.set_plugin_skill_leases(
                plugins
                    .active
                    .lock()
                    .map_err(|_| error("plugin_state", "Plugin lease lock failed"))?
                    .skill_leases(),
            )?;
        }
        let mut state = self.state.lock().await;
        for id in &ids {
            if plugins.servers.contains_key(id) {
                self.stop_server(&mut state, &runtime.dispatcher, id)
                    .await?;
            }
        }
        drop(state);
        plugins.processes.stop_entries(&ids).await?;
        let resources = plugins
            .resources
            .lock()
            .map_err(|_| {
                error(
                    "plugin_state",
                    "Plugin native resource registry lock failed",
                )
            })?
            .iter()
            .filter(|(id, _)| ids.contains(*id))
            .map(|(id, resource)| (id.clone(), resource.clone()))
            .collect::<Vec<_>>();
        for (id, resource) in resources {
            cleanup_resource(&mut *resource.lock().await).await?;
            plugins
                .resources
                .lock()
                .map_err(|_| {
                    error(
                        "plugin_state",
                        "Plugin native resource registry lock failed",
                    )
                })?
                .remove(&id);
        }
        let evidence = plugins
            .evidence
            .lock()
            .map_err(|_| error("plugin_state", "Plugin cleanup evidence lock failed"))?
            .clone();
        let cleanup = plugins
            .active
            .lock()
            .map_err(|_| error("plugin_state", "Plugin lease lock failed"))?
            .cleanup(&evidence, &AtomicBool::new(false))?;
        if cleanup.cleanup_pending.iter().any(|lease| {
            revoked
                .iter()
                .any(|revoked| revoked.lease.lease_id == lease.lease_id)
        }) {
            return Err(error(
                "outcome_unknown",
                "Revoked plugin cleanup remains pinned",
            ));
        }
        Ok(())
    }
    pub(super) async fn cleanup_plugins(&self, state: &mut State) -> Result<(), BoundaryError> {
        let Some(plugins) = state.plugins.clone() else {
            return Ok(());
        };
        plugins.closing.store(true, Ordering::Release);
        plugins.processes.stop_all().await?;
        plugins
            .enabled_hooks
            .lock()
            .map_err(|_| error("plugin_state", "Plugin hook catalog lock failed"))?
            .clear();
        plugins
            .engine
            .lock()
            .map_err(|_| error("plugin_state", "Plugin hook engine lock failed"))?
            .take();
        let resources = plugins
            .resources
            .lock()
            .map_err(|_| {
                error(
                    "plugin_state",
                    "Plugin native resource registry lock failed",
                )
            })?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for resource in resources {
            cleanup_resource(&mut *resource.lock().await).await?;
        }
        plugins
            .resources
            .lock()
            .map_err(|_| {
                error(
                    "plugin_state",
                    "Plugin native resource registry lock failed",
                )
            })?
            .clear();
        let evidence = plugins
            .evidence
            .lock()
            .map_err(|_| error("plugin_state", "Plugin cleanup evidence lock failed"))?
            .clone();
        let active = plugins.active.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            active
                .lock()
                .map_err(|_| error("plugin_state", "Plugin lease lock failed"))?
                .cleanup(&evidence, &AtomicBool::new(false))
        })
        .await
        .map_err(|_| error("outcome_unknown", "Plugin lease cleanup worker failed"))??;
        if !cleanup.cleanup_pending.is_empty() {
            return Err(error(
                "outcome_unknown",
                "Plugin lease or view cleanup is incomplete",
            ));
        }
        state.plugins = None;
        Ok(())
    }
}

async fn cleanup_resource(resource: &mut NativeResource) -> Result<(), BoundaryError> {
    // A failed prepare with no dispatched payload has no process outcome to
    // replay. A panicked worker is retained as Unknown after cleanup attempts.
    let _ = finish_preparation(resource).await;
    if let Some(environment) = &resource.environment {
        environment.wait_for_process_cleanup().await?;
        if !environment.cleanup()?.complete {
            return Err(error(
                "outcome_unknown",
                "Plugin native environment cleanup is incomplete",
            ));
        }
    }
    if let Some(broker) = resource.broker.take() {
        resource.broker_shutdown = Some(tokio::spawn(async move {
            broker.shutdown().await.map_err(|_| {
                error(
                    "outcome_unknown",
                    "Plugin network broker cleanup is incomplete",
                )
            })
        }));
    }
    if let Some(shutdown) = &mut resource.broker_shutdown {
        let result = shutdown.await;
        resource.broker_shutdown = None;
        if !matches!(result, Ok(Ok(()))) {
            resource.cleanup_unknown = true;
        }
    }
    if let Some(directory) = resource.broker_directory.as_ref() {
        std::fs::remove_dir(directory).map_err(|_| {
            error(
                "outcome_unknown",
                "Plugin broker directory cleanup is incomplete",
            )
        })?;
        resource.broker_directory = None;
    }
    if resource.cleanup_unknown {
        return Err(error(
            "outcome_unknown",
            "Plugin native resource cleanup requires independent reconciliation",
        ));
    }
    Ok(())
}
