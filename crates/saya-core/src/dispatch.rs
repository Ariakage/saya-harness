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
    registry::{ExternalAccess, RegisteredTool, ToolRegistry},
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::{ExecConfig, ExecTool},
        read::{ReadTool, ReadToolKind, ReadTools},
        todo::TodoTool,
        write::{DEFAULT_WRITE_MAX_BYTES, WriteTool, WriteToolKind, save_result},
    },
};
use std::{
    collections::BTreeSet,
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
                        tracing::info!(target: "saya::tools", event = "tool.finished", status = if result.ok { "completed" } else { "failed" }, error_code = result.error.as_ref().map(|error| error.code.as_str()).unwrap_or(""));
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    }
}

fn failure(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
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
    external_active: Arc<AtomicUsize>,
    execution: RwLock<Option<Arc<crate::execution::PreparedEnvironment>>>,
    secret_registry: OnceLock<Arc<crate::credentials::SecretRegistry>>,
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
    pub(crate) fn register_control_definition(
        &self,
        definition: crate::boundary::ToolDefinition,
    ) -> Result<(), BoundaryError> {
        self.registry
            .write()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?
            .register_builtin(definition)
    }
    /// Return native tool schemas without executing or authorizing a call.
    pub fn definitions(
        &self,
        mode: crate::contract::WorkMode,
    ) -> Result<Vec<crate::boundary::ToolDefinition>, BoundaryError> {
        let registry = self
            .registry
            .read()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?;
        registry.validate_current_secrets(&self.current_secrets()?)?;
        Ok(registry.definitions(mode, !self.read_only))
    }
    fn registered(&self, name: &str) -> Result<Option<RegisteredTool>, BoundaryError> {
        Ok(self
            .registry
            .read()
            .map_err(|_| failure("registry_error", "tool registry lock failed"))?
            .get(name)
            .cloned())
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
        registry.replace_external(server, revision, tools)
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
        for kind in [WriteToolKind::WriteFile, WriteToolKind::ApplyPatch] {
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
            external_active: Arc::new(AtomicUsize::new(0)),
            execution: RwLock::new(None),
            secret_registry: OnceLock::new(),
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
    #[tracing::instrument(target = "saya::tools", name = "authorize", skip_all, fields(session_id = %context.run.session_id.0, run_id = %context.run.id.0, call_id = call.call_id.as_str()))]
    pub async fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        summary: &str,
        interactive: bool,
        review: DispatchReview<'_>,
    ) -> Result<Authorization, BoundaryError> {
        if self.read_only && matches!(call.name.as_str(), "write_file" | "apply_patch" | "exec") {
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
        } else if matches!(call.name.as_str(), "output_read" | "output_search") {
            let store = self
                .output_store
                .get()
                .ok_or_else(|| failure("unknown_tool", "Output storage is unavailable"))?;
            crate::tools::output::validate_call(call, store.limits())?;
            Operation::output_query(&call.name)?
        } else if call.name == "todo_write" {
            crate::tools::todo::parse(call)?;
            Operation::todo_write()
        } else if matches!(
            call.name.as_str(),
            "read_file" | "list_dir" | "find_files" | "search_text" | "write_file" | "apply_patch"
        ) {
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
            Operation::external(entry.descriptor())?
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
    #[tracing::instrument(target = "saya::tools", name = "tool", skip_all, fields(session_id = %context.run.session_id.0, run_id = %context.run.id.0, call_id = call.call_id.as_str()))]
    pub(crate) async fn execute_with_cancel(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        permit: Arc<OneShotPermit>,
        cancel: &dyn Cancellation,
        background: bool,
    ) -> Result<ToolResult, BoundaryError> {
        let known_secrets = self.current_secrets()?;
        if self.read_only && matches!(call.name.as_str(), "write_file" | "apply_patch" | "exec") {
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
        let store: Arc<dyn Store> = if background {
            Arc::new(BackgroundStore {
                inner: self.store.clone(),
                call_id: call.call_id.clone(),
            })
        } else {
            self.store.clone()
        };
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
        tracing::info!(target: "saya::tools", event = "tool.dispatched");
        if matches!(
            call.name.as_str(),
            "read_file" | "list_dir" | "find_files" | "search_text" | "write_file" | "apply_patch"
        ) && let Some(environment) = self.effective_exec_config()?.execution
        {
            return self
                .execute_file_worker(call, context, &permit, store.as_ref(), cancel, &environment)
                .await;
        }
        match call.name.as_str() {
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
                        tracing::info!(target: "saya::tools", event = "tool.finished", status = if result.ok { "completed" } else { "failed" });
                        Ok(result)
                    }
                    Err(error) => {
                        let result = ToolResult {
                            ok: false,
                            data: None,
                            error: Some(error.0.clone()),
                            truncated: false,
                        };
                        save_result(store.as_ref(), call, context, &result).await?;
                        Err(error)
                    }
                }
            }
            "write_file" | "apply_patch" | "exec" => {
                let observed = Arc::new(ObservedStore {
                    inner: store.clone(),
                    call_id: call.call_id.clone(),
                    intent: AtomicBool::new(false),
                    finished: AtomicBool::new(false),
                });
                let tool: Box<dyn Tool> = match call.name.as_str() {
                    "exec" => Box::new(
                        ExecTool::new(
                            self.files.clone(),
                            observed.clone(),
                            permit,
                            self.effective_exec_config()?,
                        )
                        .with_output_capture(self.output_store.get().cloned()),
                    ),
                    name => Box::new(
                        WriteTool::new(
                            self.files.clone(),
                            observed.clone(),
                            permit,
                            if name == "write_file" {
                                WriteToolKind::WriteFile
                            } else {
                                WriteToolKind::ApplyPatch
                            },
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
                if self.read_only && entry.descriptor().requires_durability() {
                    return Err(failure(
                        "durability_required",
                        "side effects require durable storage",
                    ));
                }
                if !permit.authorize_external(call, context, entry.descriptor())? {
                    return Err(failure(
                        "stale_approval",
                        "external approval is stale or cancelled",
                    ));
                }
                let _lease = ExternalLease(&permit);
                let mut recorded = call.clone();
                let mut arguments = serde_json::from_str(&recorded.arguments)
                    .map_err(|_| failure("invalid_args", "invalid external arguments"))?;
                arguments =
                    Operation::external(entry.descriptor())?.normalized_arguments(arguments)?;
                crate::redaction::redact(&mut arguments, &known_secrets)?;
                recorded.arguments = serde_json::to_string(&arguments)
                    .map_err(|_| failure("invalid_args", "invalid external arguments"))?;
                store
                    .commit(&[StoreChange::ToolIntent {
                        run_id: context.run.id,
                        call: recorded,
                    }])
                    .await?;
                let outcome = match self.begin_external(&entry) {
                    Ok((adapter, _registry_lease)) => {
                        if !cancel.is_cancelled()
                            && permit.authorize_external(call, context, entry.descriptor())?
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
                                "external approval is stale or cancelled",
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
                        },
                        outputs: Vec::new(),
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
                            serde_json::json!("mcp_text_projection_ndjson_v1"),
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
            },
        };
        let result = crate::tools::write::sanitize_result(&result, &self.current_secrets()?)?;
        save_result(store, call, context, &result).await?;
        Ok(result)
    }
}
