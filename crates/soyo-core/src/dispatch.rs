//! Coordinated local-tool authorization and dispatch. The caller owns the agent
//! loop, passes the same run budget to review, and handles confirmation outcomes.
//! Volatile stores are appropriate only for controlled tests, not real workspace
//! side effects or recovery.
use crate::{
    approvals::{
        ApprovalManager, ApprovalSetup, Authorization, AuthorizationInput, OneShotPermit,
        PermissionRevision,
    },
    boundary::{
        Cancellation, Model, Operation as StoreOperation, Store, StoreChange, Tool, ToolContext,
    },
    contract::{BoundaryError, ErrorInfo, HistoryItem, Session, SessionId, ToolCall, ToolResult},
    mode::ModeController,
    permissions::{Operation, Policy, PolicyGate},
    registry::{ExternalAccess, RegisteredTool, ToolEffect, ToolRegistry, ToolSource},
    review::{ReviewConfig, SharedBudget},
    tools::{
        discovery::{
            ToolDiscoveryKind, execute_describe, execute_search, is_eager_mcp, validate_describe,
            validate_search,
        },
        exec::{ExecConfig, ExecTool},
        read::{ReadTool, ReadToolKind, ReadTools},
        todo::TodoTool,
        write::{DEFAULT_WRITE_MAX_BYTES, WriteTool, WriteToolKind, save_result},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct ObservedStore {
    inner: Arc<dyn Store>,
    call_id: String,
    intent: AtomicBool,
    finished: AtomicBool,
}
impl Store for ObservedStore {
    fn memory_exposures(
        &self,
        id: crate::contract::SessionId,
    ) -> StoreOperation<'_, Vec<crate::memory_context::MemoryExposure>> {
        self.inner.memory_exposures(id)
    }
    fn context_checkpoint(
        &self,
        id: crate::contract::SessionId,
    ) -> StoreOperation<'_, Option<crate::context_store::ContextCheckpoint>> {
        self.inner.context_checkpoint(id)
    }

    fn output_references(
        &self,
        id: SessionId,
    ) -> StoreOperation<'_, Vec<crate::output_store::OutputMetadata>> {
        self.inner.output_references(id)
    }

    fn load_session(&self, id: SessionId) -> StoreOperation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> StoreOperation<'a, ()> {
        Box::pin(async move {
            self.inner.commit(changes).await?;
            for change in changes {
                match change {
                    StoreChange::ToolIntent { call, .. } if call.call_id == self.call_id => {
                        self.intent.store(true, Ordering::Release)
                    }
                    StoreChange::ToolFinished {
                        call_id, result, ..
                    } if call_id == &self.call_id => {
                        self.finished.store(true, Ordering::Release);
                        tracing::info!(
                            target: "soyo::tools",
                            event = "tool.finished",
                            status = if result.ok { "completed" } else { "failed" },
                            error_code = result
                                .error
                                .as_ref()
                                .map(|error| error.code.as_str())
                                .unwrap_or("")
                        );
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    }
}

fn failure(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError::new(code, message)
}

/// Buffers per-call `StoreChange` records during concurrent read execution so
/// the caller can commit intents and results in deterministic call order.
pub(crate) struct BufferedReadStore {
    inner: Arc<dyn Store>,
    pending: std::sync::Mutex<Vec<StoreChange>>,
}
impl BufferedReadStore {
    pub(crate) fn new(inner: Arc<dyn Store>) -> Self {
        Self {
            inner,
            pending: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn take_changes(&self) -> Result<Vec<StoreChange>, BoundaryError> {
        let mut guard = self.pending.lock().map_err(|_| {
            BoundaryError::new("storage_error", "buffered read store lock poisoned")
        })?;
        Ok(std::mem::take(&mut *guard))
    }
}
impl Store for BufferedReadStore {
    fn memory_exposures(
        &self,
        id: crate::contract::SessionId,
    ) -> StoreOperation<'_, Vec<crate::memory_context::MemoryExposure>> {
        self.inner.memory_exposures(id)
    }
    fn context_checkpoint(
        &self,
        id: crate::contract::SessionId,
    ) -> StoreOperation<'_, Option<crate::context_store::ContextCheckpoint>> {
        self.inner.context_checkpoint(id)
    }
    fn output_references(
        &self,
        id: SessionId,
    ) -> StoreOperation<'_, Vec<crate::output_store::OutputMetadata>> {
        self.inner.output_references(id)
    }
    fn load_session(&self, id: SessionId) -> StoreOperation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> StoreOperation<'a, ()> {
        Box::pin(async move {
            let mut guard = self.pending.lock().map_err(|_| {
                BoundaryError::new("storage_error", "buffered read store lock poisoned")
            })?;
            guard.extend(changes.iter().cloned());
            Ok(())
        })
    }
}
/// A background child operation is not a provider tool call. Persist its
/// intent/result but never inject its synthetic call ID into model history.
struct BackgroundStore {
    inner: Arc<dyn Store>,
    call_id: String,
}
impl Store for BackgroundStore {
    fn memory_exposures(
        &self,
        id: crate::contract::SessionId,
    ) -> StoreOperation<'_, Vec<crate::memory_context::MemoryExposure>> {
        self.inner.memory_exposures(id)
    }
    fn context_checkpoint(
        &self,
        id: crate::contract::SessionId,
    ) -> StoreOperation<'_, Option<crate::context_store::ContextCheckpoint>> {
        self.inner.context_checkpoint(id)
    }

    fn output_references(
        &self,
        id: SessionId,
    ) -> StoreOperation<'_, Vec<crate::output_store::OutputMetadata>> {
        self.inner.output_references(id)
    }

    fn load_session(&self, id: SessionId) -> StoreOperation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> StoreOperation<'a, ()> {
        Box::pin(async move {
            let filtered: Vec<_> = changes.iter().filter(|change| !matches!(change,
                StoreChange::History { item: HistoryItem::ToolResult { call_id, .. }, .. } if call_id == &self.call_id
            )).cloned().collect();
            self.inner.commit(&filtered).await
        })
    }
}

/// A `js_repl` child operation follows the background layering rule: durable
/// intent/result records, but its synthetic call ID never enters model
/// history. The flags let the caller backfill evidence when a gate denied the
/// call before dispatch committed either record.
pub(crate) struct ReplStore {
    inner: Arc<dyn Store>,
    call_id: String,
    intent: AtomicBool,
    finished: AtomicBool,
}
impl ReplStore {
    pub(crate) fn new(inner: Arc<dyn Store>, call_id: String) -> Self {
        Self {
            inner,
            call_id,
            intent: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        }
    }
    pub(crate) fn intent_recorded(&self) -> bool {
        self.intent.load(Ordering::Acquire)
    }
    pub(crate) fn finished_recorded(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}
impl Store for ReplStore {
    fn memory_exposures(
        &self,
        id: crate::contract::SessionId,
    ) -> StoreOperation<'_, Vec<crate::memory_context::MemoryExposure>> {
        self.inner.memory_exposures(id)
    }
    fn context_checkpoint(
        &self,
        id: crate::contract::SessionId,
    ) -> StoreOperation<'_, Option<crate::context_store::ContextCheckpoint>> {
        self.inner.context_checkpoint(id)
    }

    fn output_references(
        &self,
        id: SessionId,
    ) -> StoreOperation<'_, Vec<crate::output_store::OutputMetadata>> {
        self.inner.output_references(id)
    }

    fn load_session(&self, id: SessionId) -> StoreOperation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> StoreOperation<'a, ()> {
        Box::pin(async move {
            for change in changes {
                match change {
                    StoreChange::ToolIntent { call, .. } if call.call_id == self.call_id => {
                        self.intent.store(true, Ordering::Release);
                    }
                    StoreChange::ToolFinished { call_id, .. } if call_id == &self.call_id => {
                        self.finished.store(true, Ordering::Release);
                    }
                    _ => {}
                }
            }
            let filtered: Vec<_> = changes.iter().filter(|change| !matches!(change,
                StoreChange::History { item: HistoryItem::ToolResult { call_id, .. }, .. } if call_id == &self.call_id
            )).cloned().collect();
            self.inner.commit(&filtered).await
        })
    }
}

/// Tool components for one run. Approval grants are never persisted or reused
/// on resume. The surrounding runtime must enforce durability before exposing
/// side-effecting operations to a real workspace.
pub struct ToolDispatcher {
    manager: ApprovalManager,
    policy: Arc<Policy>,
    files: Arc<ReadTools>,
    store: Arc<dyn Store>,
    cancel: Arc<dyn Cancellation>,
    exec_config: ExecConfig,
    write_max_bytes: usize,
    modes: ModeController,
    read_only: bool,
    context_guard: OnceLock<Arc<ContextGuard>>,
    output_store: OnceLock<Arc<crate::output_store::OutputStore>>,
    output_access_guard: OnceLock<Arc<OutputAccessGuard>>,
    known_secrets: Vec<String>,
    registry: RwLock<ToolRegistry>,
    mcp_discovery: RwLock<McpDiscoveryConfig>,
    expanded_tools: RwLock<Vec<String>>,
    external_active: Arc<AtomicUsize>,
    execution: RwLock<Option<Arc<crate::execution::PreparedEnvironment>>>,
    secret_registry: OnceLock<Arc<crate::credentials::SecretRegistry>>,
    js_repl: OnceLock<crate::js_repl::ReplLimits>,
}
#[derive(Debug, Clone)]
struct McpDiscoveryConfig {
    defer_loading: crate::mcp::config::McpDeferMode,
    defer_threshold: usize,
    eager_tools: BTreeMap<String, BTreeSet<String>>,
}
impl Default for McpDiscoveryConfig {
    fn default() -> Self {
        Self {
            defer_loading: crate::mcp::config::McpDeferMode::Auto,
            defer_threshold: crate::mcp::config::DEFAULT_MCP_DEFER_THRESHOLD,
            eager_tools: BTreeMap::new(),
        }
    }
}
pub(crate) type OutputAccessGuard =
    dyn Fn(ToolCall, ToolContext) -> StoreOperation<'static, Option<ToolResult>> + Send + Sync;
type ContextGuard = dyn Fn(&ToolCall) -> Result<bool, BoundaryError> + Send + Sync;
struct TodoLease<'a>(&'a OneShotPermit);
struct ExternalLease<'a>(&'a OneShotPermit);
struct FileLease<'a>(&'a OneShotPermit);
impl Drop for FileLease<'_> {
    fn drop(&mut self) {
        let _ = self.0.finish_file();
    }
}
struct RegistryLease(Arc<AtomicUsize>);
impl Drop for RegistryLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Drop for ExternalLease<'_> {
    fn drop(&mut self) {
        let _ = self.0.finish_external();
    }
}
impl Drop for TodoLease<'_> {
    fn drop(&mut self) {
        let _ = self.0.finish_todo();
    }
}
/// Review dependencies shared with the surrounding execution loop.
pub struct DispatchReview<'a> {
    pub model: &'a dyn Model,
    pub budget: &'a SharedBudget,
    pub config: &'a ReviewConfig,
}
pub(crate) struct McpLaunch {
    pub plan: crate::tools::exec::ExecPlan,
    pub cwd: cap_std::fs::Dir,
    pub environment: std::collections::BTreeMap<OsString, OsString>,
    pub config: ExecConfig,
    pub operation: Operation,
}
impl ToolDispatcher {
    pub(crate) fn set_output_access_guard(
        &self,
        guard: Arc<OutputAccessGuard>,
    ) -> Result<(), BoundaryError> {
        self.output_access_guard
            .set(guard)
            .map_err(|_| failure("invalid_config", "Output access guard is already installed"))
    }
    async fn guard_output_access(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<Option<ToolResult>, BoundaryError> {
        match self.output_access_guard.get() {
            Some(guard) => guard(call.clone(), context.clone()).await,
            None => Ok(None),
        }
    }

    pub(crate) fn permission_fingerprint(&self) -> String {
        self.policy.fingerprint()
    }
    pub(crate) fn protect_host_paths(
        &self,
        paths: Vec<std::path::PathBuf>,
    ) -> Result<(), BoundaryError> {
        self.policy.protect_host_paths(paths.clone())?;
        self.files
            .protect_paths(paths)
            .map_err(|_| failure("permission_error", "Cannot protect managed store paths"))
    }
    pub(crate) fn managed_context_decision(
        &self,
        mode: crate::contract::WorkMode,
        permissions: crate::contract::PermissionMode,
        operation: &Operation,
    ) -> crate::permissions::PolicyDecision {
        self.policy.evaluate(mode, permissions, operation)
    }
    /// Prepare only the explicit host-configured startup references. This does
    /// not broaden the normal model-facing exec environment policy.
    pub(crate) fn prepare_mcp_launch(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        allowed_refs: &BTreeSet<String>,
    ) -> Result<McpLaunch, BoundaryError> {
        if self.read_only {
            return Err(failure(
                "durability_required",
                "server startup requires durable storage",
            ));
        }
        let config = self.effective_exec_config()?;
        let planner = ExecTool::new(
            self.files.clone(),
            self.store.clone(),
            Arc::new(PolicyGate::new(self.policy.clone())),
            config.clone(),
        );
        let (plan, cwd, environment) = planner.prepare_host_launch(call, context, allowed_refs)?;
        let keys = environment
            .keys()
            .map(|key| key.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let operation = Operation::exec_with_environment_keys(&plan, &keys)?;
        Ok(McpLaunch {
            plan,
            cwd,
            environment,
            config,
            operation,
        })
    }
    pub(crate) async fn authorize_mcp_launch(
        &self,
        input: AuthorizationInput<'_>,
        review: DispatchReview<'_>,
    ) -> Result<Authorization, BoundaryError> {
        if self.read_only || input.call.name != "exec" || input.operation.tool_name() != "exec" {
            return Err(failure(
                "permission_denied",
                "invalid server startup operation",
            ));
        }
        let mut config = review.config.clone();
        config.known_secrets.extend(self.current_secrets()?);
        self.manager
            .authorize(input, review.model, review.budget, &config)
            .await
    }
    pub(crate) async fn authorize_managed(
        &self,
        input: AuthorizationInput<'_>,
        review: DispatchReview<'_>,
    ) -> Result<Authorization, BoundaryError> {
        let mut config = review.config.clone();
        config.known_secrets.extend(self.current_secrets()?);
        self.manager
            .authorize(input, review.model, review.budget, &config)
            .await
    }
    /// Enable the experimental read-only JS REPL with validated limits. When
    /// unset, `js_repl` calls are treated as an unknown tool.
    pub(crate) fn set_js_repl(
        &self,
        limits: crate::js_repl::ReplLimits,
    ) -> Result<(), BoundaryError> {
        self.js_repl
            .set(limits)
            .map_err(|_| failure("invalid_config", "js_repl is already configured"))
    }
    /// The prepared native environment, when one was installed for this run.
    pub(crate) fn execution_environment(
        &self,
    ) -> Result<Option<crate::execution::PreparedEnvironment>, BoundaryError> {
        Ok(self
            .execution
            .read()
            .map_err(|_| failure("sandbox_error", "execution environment lock failed"))?
            .as_ref()
            .map(|environment| environment.as_ref().clone()))
    }
    /// Install a prepared environment at a runtime-controlled safe boundary.
    pub(crate) fn set_execution_environment(
        &self,
        environment: Arc<crate::execution::PreparedEnvironment>,
    ) -> Result<(), BoundaryError> {
        *self
            .execution
            .write()
            .map_err(|_| failure("sandbox_error", "execution environment lock failed"))? =
            Some(environment);
        Ok(())
    }
    pub(crate) fn effective_exec_config(&self) -> Result<ExecConfig, BoundaryError> {
        let mut config = self.exec_config.clone();
        if let Some(environment) = self
            .execution
            .read()
            .map_err(|_| failure("sandbox_error", "execution environment lock failed"))?
            .as_ref()
        {
            config.execution = Some(environment.as_ref().clone());
        }
        Ok(config)
    }
    pub(crate) fn set_secret_registry(
        &self,
        registry: Arc<crate::credentials::SecretRegistry>,
    ) -> Result<(), BoundaryError> {
        self.manager.set_secret_registry(registry.clone())?;
        match self.secret_registry.set(registry.clone()) {
            Ok(()) => Ok(()),
            Err(_)
                if self
                    .secret_registry
                    .get()
                    .is_some_and(|current| Arc::ptr_eq(current, &registry)) =>
            {
                Ok(())
            }
            Err(_) => Err(failure(
                "credential_registry",
                "secret registry is already configured",
            )),
        }
    }
    fn current_secrets(&self) -> Result<Vec<String>, BoundaryError> {
        let mut values = self.known_secrets.clone();
        if let Some(registry) = self.secret_registry.get() {
            values.extend(registry.snapshot()?);
        }
        values.sort();
        values.dedup();
        Ok(values)
    }
    /// Redact one tool result with the same dynamic registry used for
    /// foreground dispatch. Background jobs call this before persisting or
    /// returning a result.
    pub(crate) fn redact_tool_result(&self, result: &mut ToolResult) -> Result<(), BoundaryError> {
        let redacted = crate::redaction::tool_result(result, &self.current_secrets()?)?;
        *result = redacted;
        Ok(())
    }
    /// Redact an error message and any preserved observed result.
    pub(crate) fn redact_error(&self, error: &mut BoundaryError) -> Result<(), BoundaryError> {
        let mut value = serde_json::json!({"code":error.0.code,"message":error.0.message});
        crate::redaction::redact(&mut value, &self.current_secrets()?)?;
        if let Some(code) = value.get("code").and_then(serde_json::Value::as_str) {
            error.0.code = code.to_owned();
        }
        if let Some(message) = value.get("message").and_then(serde_json::Value::as_str) {
            error.0.message = message.to_owned();
        }
        if let Some(result) = error.2.as_mut() {
            self.redact_tool_result(result)?;
        }
        Ok(())
    }
    pub(crate) fn register_control_definition(
        &self,
        definition: crate::boundary::ToolDefinition,
    ) -> Result<(), BoundaryError> {
        self.registry
            .write()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?
            .register_builtin(definition)
    }
    /// Register a host-owned adapter whose classification and schema are fixed
    /// by this build. It follows the same approval, capture and persistence
    /// path as a discovered MCP adapter.
    pub(crate) fn register_builtin_adapter(
        &self,
        definition: crate::boundary::ToolDefinition,
        adapter: Arc<dyn Tool>,
    ) -> Result<(), BoundaryError> {
        self.registry
            .write()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?
            .register_builtin_adapter(definition, adapter)
    }
    /// Configure MCP deferred-discovery thresholds and per-server eager tools.
    pub fn configure_mcp_discovery(
        &self,
        settings: &crate::mcp::config::McpSettings,
    ) -> Result<(), BoundaryError> {
        settings.validate()?;
        let mut eager_tools = BTreeMap::new();
        for (server, entry) in &settings.servers {
            if !entry.eager_tools.is_empty() {
                eager_tools.insert(server.clone(), entry.eager_tools.clone());
            }
        }
        *self
            .mcp_discovery
            .write()
            .map_err(|_| failure("registry_error", "tool discovery lock failed"))? =
            McpDiscoveryConfig {
                defer_loading: settings.defer_loading,
                defer_threshold: settings.defer_threshold,
                eager_tools,
            };
        Ok(())
    }
    /// Return native and external tool schemas split into `(stable_eager, expanded)`.
    /// When deferred loading is inactive, all eligible tools stay in `stable_eager`.
    pub fn definitions_split(
        &self,
        mode: crate::contract::WorkMode,
    ) -> Result<
        (
            Vec<crate::boundary::ToolDefinition>,
            Vec<crate::boundary::ToolDefinition>,
        ),
        BoundaryError,
    > {
        let registry = self
            .registry
            .read()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?;
        registry.validate_current_secrets(&self.current_secrets()?)?;
        let eligible: Vec<&RegisteredTool> = registry
            .eligible_entries(mode, !self.read_only)
            .into_iter()
            .filter(|entry| {
                !self.read_only
                    || !matches!(
                        crate::registry::builtin_effect(&entry.descriptor().name),
                        Some(ToolEffect::BrowserRead | ToolEffect::BrowserWrite)
                    )
            })
            .collect();
        let mcp_count = eligible
            .iter()
            .filter(|entry| matches!(entry.descriptor().source, ToolSource::Mcp { .. }))
            .count();
        let discovery = self
            .mcp_discovery
            .read()
            .map_err(|_| failure("registry_error", "tool discovery lock failed"))?;
        let defer_active = match discovery.defer_loading {
            crate::mcp::config::McpDeferMode::Always => mcp_count > 0,
            crate::mcp::config::McpDeferMode::Never => false,
            crate::mcp::config::McpDeferMode::Auto => mcp_count > discovery.defer_threshold,
        };
        if !defer_active {
            return Ok((
                eligible
                    .into_iter()
                    .map(RegisteredTool::definition)
                    .collect(),
                Vec::new(),
            ));
        }
        let mut stable_eager = Vec::new();
        let mut eager_names = BTreeSet::new();
        for entry in &eligible {
            if !matches!(entry.descriptor().source, ToolSource::Mcp { .. }) {
                let def = entry.definition();
                eager_names.insert(def.name.clone());
                stable_eager.push(def);
            }
        }
        stable_eager.push(ToolDiscoveryKind::Search.definition());
        stable_eager.push(ToolDiscoveryKind::Describe.definition());
        for entry in &eligible {
            if matches!(entry.descriptor().source, ToolSource::Mcp { .. })
                && is_eager_mcp(entry, &discovery.eager_tools)
            {
                let def = entry.definition();
                eager_names.insert(def.name.clone());
                stable_eager.push(def);
            }
        }
        let expanded_names = self
            .expanded_tools
            .read()
            .map_err(|_| failure("registry_error", "tool discovery lock failed"))?;
        let mut expanded = Vec::new();
        for name in expanded_names.iter() {
            if eager_names.contains(name) {
                continue;
            }
            if let Some(entry) = eligible
                .iter()
                .copied()
                .find(|entry| entry.descriptor().name == *name)
            {
                expanded.push(entry.definition());
            }
        }
        Ok((stable_eager, expanded))
    }
    /// Return native tool schemas without executing or authorizing a call.
    pub fn definitions(
        &self,
        mode: crate::contract::WorkMode,
    ) -> Result<Vec<crate::boundary::ToolDefinition>, BoundaryError> {
        let (mut stable, expanded) = self.definitions_split(mode)?;
        stable.extend(expanded);
        Ok(stable)
    }
    /// Whether this dispatcher hides every side-effecting and browser tool.
    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only
    }
    fn registered(&self, name: &str) -> Result<Option<RegisteredTool>, BoundaryError> {
        Ok(self
            .registry
            .read()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?
            .get(name)
            .cloned())
    }
    /// Snapshot the bound descriptor for a registered tool, or `None` when the
    /// name is not registered. Callers must not infer capability from the
    /// descriptor alone; authorization still applies per call.
    pub(crate) fn registered_descriptor(
        &self,
        name: &str,
    ) -> Result<Option<crate::registry::OperationDescriptor>, BoundaryError> {
        Ok(self
            .registered(name)?
            .map(|entry| entry.descriptor().clone()))
    }
    /// Add a host-configured external adapter. The descriptor remains bound to
    /// this discovery revision for the complete approval and execution path.
    pub fn register_external(
        &self,
        server: &str,
        revision: &str,
        adapter: Arc<dyn Tool>,
        access: ExternalAccess,
    ) -> Result<String, BoundaryError> {
        let secrets = self.current_secrets()?;
        let mut registry = self
            .registry
            .write()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?;
        if self.external_active.load(Ordering::Acquire) != 0 {
            return Err(failure("registry_busy", "external calls are still active"));
        }
        registry.set_known_secrets(secrets);
        registry.register_external(server, revision, adapter, access)
    }
    pub fn replace_external(
        &self,
        server: &str,
        revision: &str,
        tools: Vec<(Arc<dyn Tool>, ExternalAccess)>,
    ) -> Result<Vec<String>, BoundaryError> {
        let secrets = self.current_secrets()?;
        let mut registry = self
            .registry
            .write()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?;
        if self.external_active.load(Ordering::Acquire) != 0 {
            return Err(failure("registry_busy", "external calls are still active"));
        }
        registry.set_known_secrets(secrets);
        let names = registry.replace_external(server, revision, tools)?;
        let mut expanded = self
            .expanded_tools
            .write()
            .map_err(|_| failure("registry_error", "tool discovery lock failed"))?;
        expanded.retain(|name| registry.get(name).is_some());
        Ok(names)
    }
    fn begin_external(
        &self,
        entry: &RegisteredTool,
    ) -> Result<(Arc<dyn Tool>, RegistryLease), BoundaryError> {
        let registry = self
            .registry
            .read()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?;
        let current = registry
            .get(&entry.descriptor().name)
            .filter(|current| current.descriptor() == entry.descriptor())
            .ok_or_else(|| {
                failure(
                    "stale_approval",
                    "external tool discovery changed before dispatch",
                )
            })?;
        let adapter = current
            .adapter()
            .ok_or_else(|| failure("unknown_tool", "tool adapter is unavailable"))?;
        self.external_active.fetch_add(1, Ordering::AcqRel);
        Ok((adapter, RegistryLease(self.external_active.clone())))
    }
    fn native_definitions(&self) -> Vec<crate::boundary::ToolDefinition> {
        use crate::tools::read::ReadToolKind;
        let mut definitions = Vec::new();
        for kind in [
            ReadToolKind::ReadFile,
            ReadToolKind::ListDir,
            ReadToolKind::FindFiles,
            ReadToolKind::SearchText,
        ] {
            definitions.push(ReadTool::new(self.files.clone(), kind).definition());
        }
        definitions.push(TodoTool.definition());
        if self.read_only {
            return definitions;
        }
        for kind in [
            WriteToolKind::WriteFile,
            WriteToolKind::EditFile,
            WriteToolKind::ApplyPatch,
        ] {
            definitions.push(
                WriteTool::new(
                    self.files.clone(),
                    self.store.clone(),
                    Arc::new(PolicyGate::new(self.policy.clone())),
                    kind,
                    self.write_max_bytes,
                )
                .definition(),
            );
        }
        definitions.push(
            ExecTool::new(
                self.files.clone(),
                self.store.clone(),
                Arc::new(PolicyGate::new(self.policy.clone())),
                self.exec_config.clone(),
            )
            .definition(),
        );
        definitions
    }
    pub fn new(
        policy: Arc<Policy>,
        modes: ModeController,
        permissions: PermissionRevision,
        store: Arc<dyn Store>,
        setup: ApprovalSetup,
        files: Arc<ReadTools>,
        exec_config: ExecConfig,
    ) -> Result<Self, BoundaryError> {
        let cancel = setup.cancellation.clone();
        let known_secrets = setup.known_secrets.clone();
        let manager = ApprovalManager::new(
            policy.clone(),
            modes.clone(),
            permissions,
            store.clone(),
            setup,
        )?;
        let dispatcher = Self {
            manager,
            policy,
            files,
            store,
            cancel,
            exec_config,
            write_max_bytes: DEFAULT_WRITE_MAX_BYTES,
            modes,
            read_only: false,
            context_guard: OnceLock::new(),
            output_store: OnceLock::new(),
            output_access_guard: OnceLock::new(),
            known_secrets: known_secrets.clone(),
            registry: RwLock::new(ToolRegistry::with_known_secrets(known_secrets)),
            mcp_discovery: RwLock::new(McpDiscoveryConfig::default()),
            expanded_tools: RwLock::new(Vec::new()),
            external_active: Arc::new(AtomicUsize::new(0)),
            execution: RwLock::new(None),
            secret_registry: OnceLock::new(),
            js_repl: OnceLock::new(),
        };
        for definition in dispatcher.native_definitions() {
            dispatcher
                .registry
                .write()
                .map_err(|_| failure("registry_error", "tool registry lock failed"))?
                .register_builtin(definition)?;
        }
        Ok(dispatcher)
    }
    pub(crate) async fn recover_outputs(&self, session_id: SessionId) -> Result<(), BoundaryError> {
        if let Some(store) = self.output_store.get() {
            let records = self.store.output_references(session_id).await?;
            let registered: std::collections::BTreeMap<_, _> = records
                .iter()
                .map(|metadata| (metadata.output_ref.as_str(), metadata))
                .collect();
            for item in store.inventory()? {
                if item.state == crate::output_store::OutputObjectState::Sealed
                    && let Some(reference) = item.output_ref
                    && let Some(metadata) = registered.get(reference.as_str())
                    && metadata.scope == item.scope
                {
                    store.publish(session_id, &reference)?;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn set_output_store(
        &self,
        store: Arc<crate::output_store::OutputStore>,
    ) -> Result<(), BoundaryError> {
        use crate::tools::output::OutputToolKind;
        let limits = store.limits();
        self.output_store
            .set(store)
            .map_err(|_| failure("output_config", "Output storage is already configured"))?;
        for kind in [OutputToolKind::Read, OutputToolKind::Search] {
            self.register_control_definition(kind.definition(limits))?;
        }
        Ok(())
    }
    pub(crate) fn with_write_limit(mut self, max_bytes: usize) -> Self {
        self.write_max_bytes = max_bytes;
        self
    }
    pub(crate) fn set_context_guard(&self, guard: Arc<ContextGuard>) -> Result<(), BoundaryError> {
        self.context_guard.set(guard).map_err(|_| {
            failure(
                "project_context_error",
                "project context is already configured",
            )
        })
    }
    /// Remove side-effecting operations before exposing a non-durable store.
    pub(crate) fn restrict_to_read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
    pub(crate) fn mode_controller(&self) -> ModeController {
        self.modes.clone()
    }
    pub(crate) fn root_cancellation(&self) -> Arc<dyn Cancellation> {
        self.cancel.clone()
    }
    pub(crate) fn owns_permit(&self, permit: &OneShotPermit) -> bool {
        self.manager.owns_permit(permit)
    }
    pub(crate) fn owns_context(&self, context: &ToolContext) -> bool {
        self.manager.owns_context(context)
    }
    /// Normalize and authorize a single call. A `Pending` or
    /// `NeedsConfirmation` result never executes the operation. The caller
    /// must use the returned permit exactly once, or resolve a pending request.
    #[tracing::instrument(
        target = "soyo::tools",
        name = "authorize",
        skip_all,
        fields(
            session_id = %context.run.session_id.0,
            run_id = %context.run.id.0,
            call_id = call.call_id.as_str()
        )
    )]
    pub async fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        summary: &str,
        interactive: bool,
        review: DispatchReview<'_>,
    ) -> Result<Authorization, BoundaryError> {
        if self.read_only && crate::registry::is_local_mutation_tool(&call.name) {
            return Err(failure(
                "durability_required",
                "side effects require durable storage",
            ));
        }
        let operation = if call.name == "exec" {
            let planner = ExecTool::new(
                self.files.clone(),
                self.store.clone(),
                Arc::new(PolicyGate::new(self.policy.clone())),
                self.effective_exec_config()?,
            );
            let (plan, keys) = planner.plan_with_environment_keys(call, context)?;
            Operation::exec_with_environment_keys(&plan, &keys)?
        } else if call.name == "ask_user" {
            crate::input::InputRequest::parse(call)?;
            Operation::ask_user()
        } else if call.name == crate::js_repl::TOOL_NAME {
            let limits = self
                .js_repl
                .get()
                .ok_or_else(|| failure("unknown_tool", "js_repl is not enabled"))?;
            crate::js_repl::parse_args(call, limits)?;
            Operation::js_repl()
        } else if matches!(call.name.as_str(), "output_read" | "output_search") {
            let store = self
                .output_store
                .get()
                .ok_or_else(|| failure("unknown_tool", "Output storage is unavailable"))?;
            crate::tools::output::validate_call(call, store.limits())?;
            Operation::output_query(&call.name)?
        } else if call.name == "tool_search" {
            validate_search(call)?;
            Operation::tool_discovery(&call.name)?
        } else if call.name == "tool_describe" {
            validate_describe(call)?;
            Operation::tool_discovery(&call.name)?
        } else if call.name == "todo_write" {
            crate::tools::todo::parse(call)?;
            Operation::todo_write()
        } else if crate::registry::is_local_file_tool(&call.name) {
            let args: serde_json::Value = serde_json::from_str(&call.arguments)
                .map_err(|e| failure("invalid_args", e.to_string()))?;
            let path = args
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| failure("invalid_args", "path must be a string"))?;
            Operation::file(&context.workspace, &call.name, Path::new(path))?
        } else if matches!(
            call.name.as_str(),
            "background_status" | "background_cancel"
        ) {
            let args: serde_json::Value = serde_json::from_str(&call.arguments)
                .map_err(|_| failure("invalid_args", "invalid background control arguments"))?;
            if args.as_object().is_none_or(|map| {
                map.len() != 1
                    || map
                        .get("job_id")
                        .and_then(serde_json::Value::as_str)
                        .is_none()
            }) {
                return Err(failure(
                    "invalid_args",
                    "background control requires a job ID",
                ));
            }
            Operation::background_control(&call.name)?
        } else if let Some(entry) = self.registered(&call.name)? {
            entry.validate_arguments(call)?;
            if self.read_only
                && (entry.descriptor().requires_durability()
                    || matches!(
                        entry.descriptor().effect,
                        crate::registry::ToolEffect::BrowserRead
                            | crate::registry::ToolEffect::BrowserWrite
                    ))
            {
                return Err(failure(
                    "durability_required",
                    "side effects require durable storage",
                ));
            }
            Operation::registered(entry.descriptor())?
        } else {
            return Err(failure("unknown_tool", "unsupported local tool"));
        };
        self.manager
            .authorize(
                AuthorizationInput {
                    operation: &operation,
                    call,
                    context,
                    summary,
                    interactive,
                },
                review.model,
                review.budget,
                review.config,
            )
            .await
    }
    /// Complete a stored confirmation. The returned permit remains tied to the
    /// exact operation, mode, permission version, and run.
    pub fn resolve(
        &self,
        approval_id: &str,
        approve: bool,
    ) -> Result<Authorization, BoundaryError> {
        self.manager.resolve(approval_id, approve)
    }
    /// Execute one previously authorized call. Adapters recheck the permit;
    /// writes and commands save their own intent/result, and reads are paired
    /// here. Store failure stops dispatch and a failed result save returns any
    /// already observed result through `BoundaryError`.
    pub async fn execute(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        permit: Arc<OneShotPermit>,
    ) -> Result<ToolResult, BoundaryError> {
        self.execute_with_cancel(call, context, permit, self.cancel.as_ref(), false)
            .await
    }
    /// Check in-memory whether a pure read-only tool call is immediately allowed
    /// without persisting any confirmation or approval state.
    pub(crate) fn is_parallel_read_preapproved(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> bool {
        if context.run.permissions == crate::contract::PermissionMode::Manual {
            return false;
        }
        let operation = match call.name.as_str() {
            "read_file" | "list_dir" | "find_files" | "search_text" => {
                let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments) else {
                    return false;
                };
                let Some(path) = args.get("path").and_then(serde_json::Value::as_str) else {
                    return false;
                };
                Operation::file(&context.workspace, &call.name, Path::new(path))
            }
            "output_read" | "output_search" => {
                let Some(store) = self.output_store.get() else {
                    return false;
                };
                if crate::tools::output::validate_call(call, store.limits()).is_err() {
                    return false;
                }
                Operation::output_query(&call.name)
            }
            "tool_search" => {
                if validate_search(call).is_err() {
                    return false;
                }
                Operation::tool_discovery(&call.name)
            }
            "tool_describe" => {
                if validate_describe(call).is_err() {
                    return false;
                }
                Operation::tool_discovery(&call.name)
            }
            _ => return false,
        };
        let Ok(op) = operation else {
            return false;
        };
        let policy = self
            .policy
            .evaluate(context.run.work_mode, context.run.permissions, &op);
        policy.decision == crate::permissions::Decision::Allow
    }

    #[tracing::instrument(
        target = "soyo::tools",
        name = "tool",
        skip_all,
        fields(
            session_id = %context.run.session_id.0,
            run_id = %context.run.id.0,
            call_id = call.call_id.as_str()
        )
    )]
    pub(crate) async fn execute_with_cancel(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        permit: Arc<OneShotPermit>,
        cancel: &dyn Cancellation,
        background: bool,
    ) -> Result<ToolResult, BoundaryError> {
        let store: Arc<dyn Store> = if background {
            Arc::new(BackgroundStore {
                inner: self.store.clone(),
                call_id: call.call_id.clone(),
            })
        } else {
            self.store.clone()
        };
        self.execute_with_cancel_on_store(call, context, permit, cancel, background, store)
            .await
    }

    #[tracing::instrument(
        target = "soyo::tools",
        name = "tool",
        skip_all,
        fields(
            session_id = %context.run.session_id.0,
            run_id = %context.run.id.0,
            call_id = call.call_id.as_str()
        )
    )]
    pub(crate) async fn execute_with_cancel_on_store(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        permit: Arc<OneShotPermit>,
        cancel: &dyn Cancellation,
        background: bool,
        store: Arc<dyn Store>,
    ) -> Result<ToolResult, BoundaryError> {
        let known_secrets = self.current_secrets()?;
        if self.read_only && crate::registry::is_local_mutation_tool(&call.name) {
            return Err(failure(
                "durability_required",
                "side effects require durable storage",
            ));
        }
        if !self.manager.owns_permit(&permit) {
            return Err(failure(
                "permission_denied",
                "permit belongs to another dispatcher",
            ));
        }
        if let Some(guard) = self.context_guard.get()
            && !guard(call)?
        {
            let error = failure(
                "project_context_changed",
                "applicable project context changed before dispatch",
            );
            if !background {
                return Err(error);
            }
            let result = ToolResult {
                ok: false,
                data: None,
                error: Some(error.0),
                truncated: false,
                media_refs: Vec::new(),
            };
            store
                .commit(&[StoreChange::ToolIntent {
                    run_id: context.run.id,
                    call: call.clone(),
                }])
                .await?;
            save_result(store.as_ref(), call, context, &result).await?;
            return Ok(result);
        }
        tracing::info!(target: "soyo::tools", event = "tool.dispatched");
        if crate::registry::is_local_file_tool(&call.name)
            && let Some(environment) = self.effective_exec_config()?.execution
        {
            return self
                .execute_file_worker(call, context, &permit, store.as_ref(), cancel, &environment)
                .await;
        }
        match call.name.as_str() {
            crate::js_repl::TOOL_NAME => Err(failure(
                "permission_denied",
                "js_repl is dispatched by the runtime; recursion is not supported",
            )),
            "output_read" | "output_search" => {
                let output_store = self
                    .output_store
                    .get()
                    .ok_or_else(|| failure("unknown_tool", "Output storage is unavailable"))?;
                crate::tools::output::validate_call(call, output_store.limits())?;
                if !permit.authorize_output(call, context)? {
                    return Err(failure(
                        "stale_approval",
                        "Output query authorization is stale",
                    ));
                }
                let _lease = TodoLease(&permit);
                store
                    .commit(&[StoreChange::ToolIntent {
                        run_id: context.run.id,
                        call: call.clone(),
                    }])
                    .await?;
                let kind = if call.name == "output_read" {
                    crate::tools::output::OutputToolKind::Read
                } else {
                    crate::tools::output::OutputToolKind::Search
                };
                let mut outcome = if permit.authorize_output(call, context)? {
                    match self.guard_output_access(call, context).await {
                        Ok(Some(projection)) => Ok(projection),
                        Ok(None) => {
                            crate::tools::output::OutputTool::new(output_store.clone(), kind)
                                .execute(call, context, cancel)
                                .await
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    Err(failure(
                        "stale_approval",
                        "Output query authorization changed",
                    ))
                };
                // The memory ledger may change during an approved retrieval.
                // Check again before persisting or exposing the observed body.
                match self.guard_output_access(call, context).await {
                    Ok(Some(projection)) => outcome = Ok(projection),
                    Ok(None) => {}
                    Err(error) => outcome = Err(error),
                }
                let result = match &outcome {
                    Ok(result) => crate::redaction::tool_result(result, &self.current_secrets()?)?,
                    Err(error) => ToolResult {
                        ok: false,
                        data: None,
                        error: Some(error.0.clone()),
                        truncated: false,
                        media_refs: Vec::new(),
                    },
                };
                save_result(store.as_ref(), call, context, &result).await?;
                outcome.map(|_| result)
            }
            "tool_search" | "tool_describe" => {
                if !permit.authorize_tool_discovery(call, context)? {
                    return Err(failure(
                        "stale_approval",
                        "Tool discovery authorization is stale",
                    ));
                }
                let _lease = TodoLease(&permit);
                store
                    .commit(&[StoreChange::ToolIntent {
                        run_id: context.run.id,
                        call: call.clone(),
                    }])
                    .await?;
                let outcome =
                    if permit.authorize_tool_discovery(call, context)? {
                        let secrets = self.current_secrets()?;
                        let registry = self
                            .registry
                            .read()
                            .map_err(|_| failure("registry_error", "tool registry lock failed"))?;
                        registry.validate_current_secrets(&secrets)?;
                        let discovery = self
                            .mcp_discovery
                            .read()
                            .map_err(|_| failure("registry_error", "tool discovery lock failed"))?;
                        if call.name == "tool_search" {
                            let args = validate_search(call)?;
                            let expanded = self.expanded_tools.read().map_err(|_| {
                                failure("registry_error", "tool discovery lock failed")
                            })?;
                            execute_search(
                                &registry,
                                &args,
                                context.run.work_mode,
                                !self.read_only,
                                &discovery.eager_tools,
                                &expanded,
                                &secrets,
                            )
                        } else {
                            let args = validate_describe(call)?;
                            let mut expanded = self.expanded_tools.write().map_err(|_| {
                                failure("registry_error", "tool discovery lock failed")
                            })?;
                            execute_describe(
                                &registry,
                                &args,
                                context.run.work_mode,
                                !self.read_only,
                                &discovery.eager_tools,
                                &mut expanded,
                                &secrets,
                            )
                        }
                    } else {
                        Err(failure(
                            "stale_approval",
                            "Tool discovery authorization changed",
                        ))
                    };
                let result = match &outcome {
                    Ok(result) => crate::redaction::tool_result(result, &self.current_secrets()?)?,
                    Err(error) => ToolResult {
                        ok: false,
                        data: None,
                        error: Some(error.0.clone()),
                        truncated: false,
                        media_refs: Vec::new(),
                    },
                };
                save_result(store.as_ref(), call, context, &result).await?;
                outcome.map(|_| result)
            }
            "todo_write" => {
                if !permit.authorize_todo(call, context)? {
                    return Err(failure(
                        if cancel.is_cancelled() {
                            "cancelled"
                        } else {
                            "stale_approval"
                        },
                        "todo update cancelled or approval is stale",
                    ));
                }
                let _lease = TodoLease(&permit);
                store
                    .commit(&[StoreChange::ToolIntent {
                        run_id: context.run.id,
                        call: call.clone(),
                    }])
                    .await?;
                let outcome = if permit.authorize_todo(call, context)? {
                    TodoTool.execute(call, context, cancel).await
                } else {
                    Err(failure(
                        if cancel.is_cancelled() {
                            "cancelled"
                        } else {
                            "stale_approval"
                        },
                        "todo update cancelled or approval is stale",
                    ))
                };
                let mut result = match &outcome {
                    Ok(result) => crate::redaction::tool_result(result, &self.current_secrets()?)?,
                    Err(error) => ToolResult {
                        ok: false,
                        data: None,
                        error: Some(error.0.clone()),
                        truncated: false,
                        media_refs: Vec::new(),
                    },
                };
                if result.ok && crate::tools::todo::result_todos(&result).is_none() {
                    result = ToolResult {
                        ok: false,
                        data: None,
                        error: Some(ErrorInfo {
                            code: "invalid_args".into(),
                            message:
                                "todo list violates limits or unique IDs after credential redaction"
                                    .into(),
                        }),
                        truncated: false,
                        media_refs: Vec::new(),
                    };
                }
                save_result(store.as_ref(), call, context, &result).await?;
                outcome.map(|_| result)
            }
            "read_file" | "list_dir" | "find_files" | "search_text" => {
                let kind = match call.name.as_str() {
                    "read_file" => ReadToolKind::ReadFile,
                    "list_dir" => ReadToolKind::ListDir,
                    "find_files" => ReadToolKind::FindFiles,
                    _ => ReadToolKind::SearchText,
                };
                store
                    .commit(&[StoreChange::ToolIntent {
                        run_id: context.run.id,
                        call: call.clone(),
                    }])
                    .await?;
                let tool = ReadTool::with_gate(self.files.clone(), kind, permit);
                match tool.execute(call, context, cancel).await {
                    Ok(result) => {
                        let result =
                            crate::redaction::tool_result(&result, &self.current_secrets()?)?;
                        save_result(store.as_ref(), call, context, &result).await?;
                        tracing::info!(target: "soyo::tools", event = "tool.finished", status = if result.ok { "completed" } else { "failed" });
                        Ok(result)
                    }
                    Err(error) => {
                        let result = ToolResult {
                            ok: false,
                            data: None,
                            error: Some(error.0.clone()),
                            truncated: false,
                            media_refs: Vec::new(),
                        };
                        save_result(store.as_ref(), call, context, &result).await?;
                        Err(error)
                    }
                }
            }
            name if crate::registry::is_local_mutation_tool(name) => {
                let observed = Arc::new(ObservedStore {
                    inner: store.clone(),
                    call_id: call.call_id.clone(),
                    intent: AtomicBool::new(false),
                    finished: AtomicBool::new(false),
                });
                let tool: Box<dyn Tool> = match name {
                    "exec" => Box::new(
                        ExecTool::new(
                            self.files.clone(),
                            observed.clone(),
                            permit,
                            self.effective_exec_config()?,
                        )
                        .with_output_capture(self.output_store.get().cloned()),
                    ),
                    "write_file" => Box::new(
                        WriteTool::new(
                            self.files.clone(),
                            observed.clone(),
                            permit,
                            WriteToolKind::WriteFile,
                            self.write_max_bytes,
                        )
                        .with_known_secrets(known_secrets.clone()),
                    ),
                    "edit_file" => Box::new(
                        WriteTool::new(
                            self.files.clone(),
                            observed.clone(),
                            permit,
                            WriteToolKind::EditFile,
                            self.write_max_bytes,
                        )
                        .with_known_secrets(known_secrets.clone()),
                    ),
                    _ => Box::new(
                        WriteTool::new(
                            self.files.clone(),
                            observed.clone(),
                            permit,
                            WriteToolKind::ApplyPatch,
                            self.write_max_bytes,
                        )
                        .with_known_secrets(known_secrets.clone()),
                    ),
                };
                let result = tool.execute(call, context, cancel).await?;
                let result = crate::redaction::tool_result(&result, &self.current_secrets()?)?;
                if !observed.intent.load(Ordering::Acquire) {
                    // No side effect was dispatched. Avoid storing unvalidated,
                    // potentially credential-bearing arguments in this record.
                    let mut redacted = call.clone();
                    redacted.arguments = "{\"redacted\":true}".into();
                    observed
                        .commit(&[StoreChange::ToolIntent {
                            run_id: context.run.id,
                            call: redacted,
                        }])
                        .await?;
                }
                if !observed.finished.load(Ordering::Acquire) {
                    save_result(observed.as_ref(), call, context, &result).await?;
                }
                Ok(result)
            }
            _ => {
                let entry = self
                    .registered(&call.name)?
                    .ok_or_else(|| failure("unknown_tool", "unsupported tool"))?;
                entry.validate_arguments(call)?;
                if self.read_only
                    && (entry.descriptor().requires_durability()
                        || matches!(
                            entry.descriptor().effect,
                            crate::registry::ToolEffect::BrowserRead
                                | crate::registry::ToolEffect::BrowserWrite
                        ))
                {
                    return Err(failure(
                        "durability_required",
                        "side effects require durable storage",
                    ));
                }
                if !permit.authorize_registered(call, context, entry.descriptor())? {
                    return Err(failure(
                        "stale_approval",
                        "registered tool approval is stale or cancelled",
                    ));
                }
                let _lease = ExternalLease(&permit);
                let mut recorded = call.clone();
                let mut arguments = serde_json::from_str(&recorded.arguments)
                    .map_err(|_| failure("invalid_args", "invalid registered arguments"))?;
                arguments =
                    Operation::registered(entry.descriptor())?.normalized_arguments(arguments)?;
                crate::redaction::redact(&mut arguments, &known_secrets)?;
                recorded.arguments = serde_json::to_string(&arguments)
                    .map_err(|_| failure("invalid_args", "invalid registered arguments"))?;
                store
                    .commit(&[StoreChange::ToolIntent {
                        run_id: context.run.id,
                        call: recorded,
                    }])
                    .await?;
                let outcome = match self.begin_external(&entry) {
                    Ok((adapter, _registry_lease)) => {
                        if !cancel.is_cancelled()
                            && permit.authorize_registered(call, context, entry.descriptor())?
                        {
                            let mut remote = call.clone();
                            if let crate::registry::ToolSource::Mcp { remote_name, .. } =
                                &entry.descriptor().source
                            {
                                remote.name = remote_name.clone();
                            }
                            adapter
                                .execute_captured(
                                    &remote,
                                    context,
                                    cancel,
                                    self.output_store.get().map(Arc::as_ref),
                                )
                                .await
                        } else {
                            Err(failure(
                                "stale_approval",
                                "registered tool approval is stale or cancelled",
                            ))
                        }
                    }
                    Err(error) => Err(error),
                };
                let capture = match outcome {
                    Ok(capture) => capture,
                    Err(error) => crate::output_store::CapturedToolResult {
                        result: ToolResult {
                            ok: false,
                            data: None,
                            error: Some(error.0),
                            truncated: false,
                            media_refs: Vec::new(),
                        },
                        outputs: Vec::new(),
                        media: Vec::new(),
                        capture_errors: Vec::new(),
                    },
                };
                let mut result =
                    crate::redaction::tool_result(&capture.result, &self.current_secrets()?)?;
                let outputs: Vec<_> = capture
                    .outputs
                    .iter()
                    .map(|output| output.metadata.clone())
                    .collect();
                if let Some(data) = result
                    .data
                    .as_mut()
                    .and_then(serde_json::Value::as_object_mut)
                {
                    // Reserve this field even when a server invents its own refs.
                    data.remove("output_refs");
                    if !outputs.is_empty() {
                        data.insert(
                            "output_refs".into(),
                            serde_json::to_value(&outputs).map_err(|_| {
                                failure(
                                    "serialization_error",
                                    "Output references cannot be encoded",
                                )
                            })?,
                        );
                        data.insert(
                            "output_capture_format".into(),
                            serde_json::json!(match &entry.descriptor().source {
                                crate::registry::ToolSource::Mcp { .. } =>
                                    "mcp_text_projection_ndjson_v1",
                                crate::registry::ToolSource::Builtin => "sealed_text_v1",
                            }),
                        );
                    }
                    if !capture.capture_errors.is_empty() {
                        data.insert(
                            "output_capture_errors".into(),
                            serde_json::to_value(&capture.capture_errors).map_err(|_| {
                                failure(
                                    "serialization_error",
                                    "Output capture errors cannot be encoded",
                                )
                            })?,
                        );
                    }
                }
                crate::tools::write::save_result_with_outputs(
                    store.as_ref(),
                    call,
                    context,
                    &result,
                    &outputs,
                )
                .await?;
                if let Some(store) = self.output_store.get() {
                    for output in &capture.outputs {
                        store.publish(context.run.session_id, &output.metadata.output_ref).map_err(|mut error| {
                        error.0.code = "storage_error".into();
                        error.0.message = "Tool result committed, but output publication requires recovery".into();
                        error.2 = Some(Box::new(result.clone()));
                        error
                    })?;
                    }
                }
                Ok(result)
            }
        }
    }
    async fn execute_file_worker(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        permit: &OneShotPermit,
        store: &dyn Store,
        cancel: &dyn Cancellation,
        environment: &crate::execution::PreparedEnvironment,
    ) -> Result<ToolResult, BoundaryError> {
        let arguments: serde_json::Value = serde_json::from_str(&call.arguments)
            .map_err(|_| failure("invalid_args", "file arguments must be valid JSON"))?;
        let path = arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| failure("invalid_args", "file arguments require a path"))?;
        let operation = Operation::file(&context.workspace, &call.name, Path::new(path))?;
        if !permit.authorize_file(call, context, &operation)? {
            return Err(failure(
                "stale_approval",
                "file approval is stale or cancelled",
            ));
        }
        let _lease = FileLease(permit);
        let target = operation
            .file_target()
            .ok_or_else(|| failure("invalid_args", "file operation requires a target"))?
            .to_owned();
        let read_roots = if self.files.bypass {
            let root = if target.is_dir() {
                target.clone()
            } else {
                target
                    .parent()
                    .ok_or_else(|| failure("invalid_path", "file target has no parent"))?
                    .to_owned()
            };
            vec![root]
        } else {
            self.files.root_paths()
        };
        let files = self.files.clone();
        let snapshot_tool = call.name.clone();
        let snapshot_target = target.clone();
        let protected_paths = tokio::task::spawn_blocking(move || {
            files.worker_protection_snapshot(&snapshot_tool, &snapshot_target)
        })
        .await
        .map_err(|_| {
            failure(
                "host_protection_unavailable",
                "Private protection snapshot worker failed",
            )
        })?
        .map_err(|error| failure(error.code, error.message));
        let mut recorded = call.clone();
        let mut normalized = operation.normalized_arguments(arguments)?;
        crate::redaction::redact(&mut normalized, &self.current_secrets()?)?;
        recorded.arguments = serde_json::to_string(&normalized)
            .map_err(|_| failure("invalid_args", "file arguments cannot be encoded"))?;
        store
            .commit(&[StoreChange::ToolIntent {
                run_id: context.run.id,
                call: recorded,
            }])
            .await?;
        let outcome =
            if !cancel.is_cancelled() && permit.authorize_file(call, context, &operation)? {
                match protected_paths {
                    Ok(protected_paths) => {
                        crate::tools::isolated::execute_isolated_file(
                            environment,
                            crate::execution::FileWorkerRequest {
                                schema_version: 1,
                                call: call.clone(),
                                context: context.clone(),
                                approved_target: target,
                                read_roots,
                                read_limits: self.files.limits.clone(),
                                write_max_bytes: self.write_max_bytes,
                                protected_paths,
                            },
                            cancel,
                            self.exec_config.default_timeout,
                        )
                        .await
                    }
                    Err(failure) => Err(failure),
                }
            } else {
                Err(failure(
                    "stale_approval",
                    "file approval is stale or cancelled",
                ))
            };
        let result = match outcome {
            Ok(result) => result,
            Err(error) => ToolResult {
                ok: false,
                data: error
                    .2
                    .map(|observed| serde_json::json!({"observed_result":observed})),
                error: Some(error.0),
                truncated: false,
                media_refs: Vec::new(),
            },
        };
        let result = crate::tools::write::sanitize_result(&result, &self.current_secrets()?)?;
        save_result(store, call, context, &result).await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        contract::{PermissionMode, SessionId, WorkMode},
        memory::MemoryStore,
    };
    use std::sync::Arc;

    fn session(workspace: &str) -> (crate::contract::Session, crate::contract::Run) {
        let session = crate::contract::Session {
            id: SessionId::new(),
            workspace: workspace.into(),
            work_mode: WorkMode::Work,
            profile: "fake".into(),
            protocol: crate::contract::ModelProtocol::Responses,
            model: "fake".into(),
            history: vec![],
        };
        let run = crate::contract::Run {
            id: crate::contract::RunId::new(),
            session_id: session.id,
            work_mode: WorkMode::Work,
            permissions: PermissionMode::Bypass,
        };
        (session, run)
    }

    #[tokio::test]
    async fn repl_store_filters_child_history_but_keeps_durable_evidence() {
        let (session, run) = session("workspace");
        let inner: Arc<MemoryStore> = Arc::new(MemoryStore::default());
        inner
            .commit(&[
                StoreChange::Session(session.clone()),
                StoreChange::RunStarted(run.clone()),
            ])
            .await
            .unwrap();
        let store = ReplStore::new(inner.clone(), "outer:repl:0".into());
        let child = ToolCall {
            call_id: "outer:repl:0".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        };
        let other = ToolCall {
            call_id: "outer:repl:1".into(),
            name: "list_dir".into(),
            arguments: "{}".into(),
        };
        store
            .commit(&[
                StoreChange::ToolIntent {
                    run_id: run.id,
                    call: child.clone(),
                },
                StoreChange::ToolIntent {
                    run_id: run.id,
                    call: other.clone(),
                },
                StoreChange::ToolFinished {
                    run_id: run.id,
                    call_id: child.call_id.clone(),
                    result: ToolResult::success(None),
                },
                StoreChange::History {
                    session_id: session.id,
                    item: HistoryItem::ToolResult {
                        call_id: child.call_id.clone(),
                        result: ToolResult::success(None),
                    },
                },
                StoreChange::History {
                    session_id: session.id,
                    item: HistoryItem::ToolResult {
                        call_id: other.call_id.clone(),
                        result: ToolResult::success(None),
                    },
                },
            ])
            .await
            .unwrap();
        assert!(store.intent_recorded());
        assert!(store.finished_recorded());
        let stored = inner.load_session(session.id).await.unwrap().unwrap();
        assert_eq!(stored.history.len(), 1);
        match &stored.history[0] {
            HistoryItem::ToolResult { call_id, .. } => {
                assert_eq!(call_id, "outer:repl:1");
            }
            _ => panic!("unexpected history item"),
        }
        let changes = inner.changes().unwrap();
        assert!(changes.iter().any(|change| matches!(
            change,
            StoreChange::ToolIntent { call, .. } if call.call_id == "outer:repl:0"
        )));
        assert!(changes.iter().any(|change| matches!(
            change,
            StoreChange::ToolFinished { call_id, .. } if call_id == "outer:repl:0"
        )));
    }
}
