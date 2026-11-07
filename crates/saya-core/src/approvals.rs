//! One-run approvals. Grants are held only in memory and are never restored.

use crate::{
    boundary::{Cancellation, Model, Store, StoreChange, ToolContext},
    contract::{
        ApprovalRequest, ApprovalSource, BoundaryError, ErrorInfo, EventPayload, PermissionMode,
        ReviewSource, RunId, ToolCall, WorkMode,
    },
    mode::ModeController,
    permissions::{Decision, Operation, Policy},
    review::{ReviewConfig, ReviewDecisionSource, ReviewInput, SharedBudget, review_operation},
    tools::{
        exec::{ExecGate, ExecPlan, resolve_executable},
        read::ReadGate,
        write::WriteGate,
    },
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

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
fn locked() -> BoundaryError {
    failure("approval_error", "approval lock poisoned")
}

/// A version shared with permission configuration; updating it invalidates all
/// previously issued approvals. Bound to the run's mode controller.
#[derive(Clone)]
pub struct PermissionRevision {
    value: Arc<Mutex<(PermissionMode, u64)>>,
    modes: ModeController,
}
impl PermissionRevision {
    pub fn new(modes: ModeController, initial: PermissionMode) -> Self {
        Self {
            value: Arc::new(Mutex::new((initial, 0))),
            modes,
        }
    }
    fn snapshot(&self) -> Result<(PermissionMode, u64), BoundaryError> {
        Ok(*self.value.lock().map_err(|_| locked())?)
    }
    pub fn current(&self) -> Result<u64, BoundaryError> {
        Ok(self.snapshot()?.1)
    }
    /// Update permission settings only while no model or tool call is active.
    /// Returns false if a call is in progress; a successful update invalidates grants.
    pub fn change_if_idle(&self, permissions: PermissionMode) -> Result<bool, BoundaryError> {
        let mut result = Ok(());
        let changed = self.modes.while_idle(|| match self.value.lock() {
            Ok(mut value) => {
                if let Some(next) = value.1.checked_add(1) {
                    value.0 = permissions;
                    value.1 = next;
                } else {
                    result = Err(failure("permission_error", "permission revision exhausted"));
                }
            }
            Err(_) => result = Err(locked()),
        })?;
        result?;
        Ok(changed)
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalFile {
    schema_version: u8,
    commands: Vec<ApprovalCommand>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalCommand {
    argv: Vec<String>,
    cwd: PathBuf,
}
#[derive(Clone, PartialEq, Eq)]
struct CommandIdentity {
    argv: Vec<String>,
    cwd: PathBuf,
}

/// Preauthorizations for a single run; each entry is usable once. Parsing does
/// not grant permission and an explicit deny or PLAN restriction still wins.
#[derive(Default)]
pub struct Preauthorizations {
    commands: Vec<CommandIdentity>,
}
impl Preauthorizations {
    /// Parse schema-v1 JSON, resolve cwd and executable, and reject patterns.
    /// Program names are resolved against the current PATH. No commands run.
    pub fn from_json(text: &str, workspace: &Path) -> Result<Self, BoundaryError> {
        if text.len() > 1024 * 1024 {
            return Err(failure(
                "invalid_approval_file",
                "approval file exceeds size limit",
            ));
        }
        let file: ApprovalFile = serde_json::from_str(text)
            .map_err(|e| failure("invalid_approval_file", e.to_string()))?;
        if file.schema_version != 1 {
            return Err(failure(
                "invalid_approval_file",
                "unsupported approval schema",
            ));
        }
        let workspace = std::fs::canonicalize(workspace)
            .map_err(|e| failure("invalid_approval_file", e.to_string()))?;
        let path_env = std::env::var_os("PATH");
        let mut commands = Vec::new();
        for command in file.commands {
            if command.argv.first().is_none_or(String::is_empty)
                || command
                    .argv
                    .iter()
                    .any(|s| s.contains(['*', '?', '[', ']', '\0']))
            {
                return Err(failure(
                    "invalid_approval_file",
                    "argv must be complete and contain no wildcard",
                ));
            }
            let cwd = if command.cwd.is_absolute() {
                command.cwd
            } else {
                workspace.join(command.cwd)
            };
            let cwd = std::fs::canonicalize(cwd)
                .map_err(|e| failure("invalid_approval_file", e.to_string()))?;
            if !cwd.is_dir() {
                return Err(failure("invalid_approval_file", "cwd must be a directory"));
            }
            let executable = resolve_executable(&command.argv[0], &cwd, path_env.as_ref())
                .map_err(|e| failure("invalid_approval_file", e.0.message))?;
            let executable = executable
                .to_str()
                .ok_or_else(|| failure("invalid_approval_file", "executable must be UTF-8"))?;
            let mut argv = command.argv;
            argv[0] = executable.into();
            commands.push(CommandIdentity { argv, cwd });
        }
        Ok(Self { commands })
    }
    /// Load the explicitly named file. Read failures return an error.
    pub fn from_file(path: &Path, workspace: &Path) -> Result<Self, BoundaryError> {
        use std::{io::Read, os::unix::fs::OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| failure("invalid_approval_file", e.to_string()))?;
        if !file
            .metadata()
            .map_err(|e| failure("invalid_approval_file", e.to_string()))?
            .is_file()
        {
            return Err(failure(
                "invalid_approval_file",
                "approval file must be a regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| failure("invalid_approval_file", e.to_string()))?;
        if bytes.len() > 1024 * 1024 {
            return Err(failure(
                "invalid_approval_file",
                "approval file exceeds size limit",
            ));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| failure("invalid_approval_file", "approval file must be UTF-8"))?;
        Self::from_json(text, workspace)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Binding {
    run_id: RunId,
    session_id: crate::contract::SessionId,
    tool: String,
    normalized_args: String,
    cwd: PathBuf,
    mode: WorkMode,
    permissions: PermissionMode,
    mode_revision: u64,
    permission_revision: u64,
}
struct Pending {
    binding: Binding,
    call_id: String,
    request: ApprovalRequest,
}
struct State {
    pending: HashMap<String, Pending>,
    rejected: HashSet<RejectionKey>,
    preapproved: Vec<CommandIdentity>,
}

/// Results of deciding whether a single operation may be dispatched.
pub enum Authorization {
    Permit(Arc<OneShotPermit>),
    Pending(ApprovalRequest),
    NeedsConfirmation(ApprovalRequest),
    Denied(&'static str),
    Stale,
}
/// All information supplied to an authorization check is bound to a single run.
pub struct AuthorizationInput<'a> {
    pub operation: &'a Operation,
    pub call: &'a ToolCall,
    pub context: &'a ToolContext,
    pub summary: &'a str,
    pub interactive: bool,
}

/// Per-run inputs. No historic approval is accepted during construction.
pub struct ApprovalSetup {
    pub run_id: RunId,
    pub session_id: crate::contract::SessionId,
    pub initial_permissions: PermissionMode,
    pub preauthorizations: Preauthorizations,
    pub known_secrets: Vec<String>,
    pub cancellation: Arc<dyn Cancellation>,
}

/// Coordinates one run's transient authorizations. Construct a new manager on
/// session resume: prior grants and rejection memory are intentionally absent.
pub struct ApprovalManager {
    policy: Arc<Policy>,
    modes: ModeController,
    permissions: PermissionRevision,
    store: Arc<dyn Store>,
    run_id: RunId,
    session_id: crate::contract::SessionId,
    initial_permissions: PermissionMode,
    cancellation: Arc<dyn Cancellation>,
    initial_mode_revision: u64,
    initial_permission_revision: u64,
    secrets: Vec<String>,
    state: Arc<Mutex<State>>,
    authorize_lock: tokio::sync::Mutex<()>,
}
impl ApprovalManager {
    pub(crate) fn owns_context(&self, context: &ToolContext) -> bool {
        self.run_id == context.run.id && self.session_id == context.run.session_id
    }
    pub(crate) fn owns_permit(&self, permit: &OneShotPermit) -> bool {
        Arc::ptr_eq(&self.state, &permit.state)
    }
    pub fn new(
        policy: Arc<Policy>,
        modes: ModeController,
        permissions: PermissionRevision,
        store: Arc<dyn Store>,
        setup: ApprovalSetup,
    ) -> Result<Self, BoundaryError> {
        if !permissions.modes.same_instance(&modes) {
            return Err(failure(
                "invalid_permissions",
                "permission revision belongs to another mode controller",
            ));
        }
        let initial_mode_revision = modes.snapshot()?.revision;
        let (initial_permission_mode, initial_permission_revision) = permissions.snapshot()?;
        if initial_permission_mode != setup.initial_permissions {
            return Err(failure(
                "invalid_permissions",
                "initial permission mode differs from revision",
            ));
        }
        Ok(Self {
            policy,
            modes,
            permissions,
            store,
            run_id: setup.run_id,
            session_id: setup.session_id,
            initial_permissions: setup.initial_permissions,
            cancellation: setup.cancellation,
            initial_mode_revision,
            initial_permission_revision,
            secrets: setup.known_secrets,
            authorize_lock: tokio::sync::Mutex::new(()),
            state: Arc::new(Mutex::new(State {
                pending: HashMap::new(),
                rejected: HashSet::new(),
                preapproved: setup.preauthorizations.commands,
            })),
        })
    }
    fn bind(&self, input: &AuthorizationInput<'_>) -> Result<Binding, BoundaryError> {
        let (permission_mode, permission_revision) = self.permissions.snapshot()?;
        if input.context.run.permissions != permission_mode {
            return Err(failure(
                "stale_approval",
                "permission mode differs from current settings",
            ));
        }
        if input.context.run.id != self.run_id
            || input.context.run.session_id != self.session_id
            || input.call.name != input.operation.tool_name()
        {
            return Err(failure(
                "invalid_args",
                "operation does not belong to this run or tool",
            ));
        }
        let mode = self.modes.snapshot()?;
        if mode.current != input.context.run.work_mode
            || mode.pending.is_some()
            || !self.modes.shared_approval_is_current(mode.revision)?
        {
            return Err(failure(
                "stale_approval",
                "mode changed or operation is already in flight",
            ));
        }
        let args: Value = serde_json::from_str(&input.call.arguments)
            .map_err(|e| failure("invalid_args", e.to_string()))?;
        let normalized = input.operation.normalized_arguments(args)?;
        let cwd = std::fs::canonicalize(&input.context.workspace)
            .map_err(|e| failure("invalid_path", e.to_string()))?;
        if cwd != self.policy.workspace() {
            return Err(failure(
                "invalid_workspace",
                "policy workspace differs from run",
            ));
        }
        Ok(Binding {
            run_id: self.run_id,
            session_id: input.context.run.session_id,
            tool: input.call.name.clone(),
            normalized_args: serde_json::to_string(&normalized)
                .map_err(|e| failure("invalid_args", e.to_string()))?,
            cwd,
            mode: mode.current,
            permissions: input.context.run.permissions,
            mode_revision: mode.revision,
            permission_revision,
        })
    }
    fn current(&self, binding: &Binding) -> Result<bool, BoundaryError> {
        let snapshot = self.modes.snapshot()?;
        Ok(snapshot.current == binding.mode
            && snapshot.pending.is_none()
            && snapshot.revision == binding.mode_revision
            && self.permissions.snapshot()? == (binding.permissions, binding.permission_revision)
            && self
                .modes
                .shared_approval_is_current(binding.mode_revision)?)
    }
    /// Decide rules, perform isolated model review only when required, and create
    /// an exact one-use permit or a stored confirmation request. No tool runs.
    pub async fn authorize(
        &self,
        input: AuthorizationInput<'_>,
        model: &dyn Model,
        budget: &SharedBudget,
        config: &ReviewConfig,
    ) -> Result<Authorization, BoundaryError> {
        let cancel = self.cancellation.as_ref();
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "authorization cancelled"));
        }
        let _guard = tokio::select! {
            guard = self.authorize_lock.lock() => guard,
            () = cancel.cancelled() => return Err(failure("cancelled", "authorization cancelled")),
        };
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "authorization cancelled"));
        }
        let binding = self.bind(&input)?;
        {
            let state = self.state.lock().map_err(|_| locked())?;
            if state.rejected.contains(&rejection_key(&binding)) {
                return Ok(Authorization::Denied("previously_denied"));
            }
            if let Some(existing) = state
                .pending
                .values()
                .find(|pending| pending.call_id == input.call.call_id && pending.binding == binding)
            {
                return Ok(if input.interactive {
                    Authorization::Pending(existing.request.clone())
                } else {
                    Authorization::NeedsConfirmation(existing.request.clone())
                });
            }
        }
        let raw: Value = serde_json::from_str(&input.call.arguments)
            .map_err(|e| failure("invalid_args", e.to_string()))?;
        let mut review_config = config.clone();
        review_config
            .known_secrets
            .extend(self.secrets.iter().cloned());
        let review = review_operation(
            &self.policy,
            ReviewInput {
                mode: binding.mode,
                permissions: binding.permissions,
                operation: input.operation,
                arguments: raw,
                goal_summary: input.summary,
            },
            model,
            budget,
            &review_config,
            cancel,
        )
        .await?;
        let source = match review.source {
            ReviewDecisionSource::Rule => ReviewSource::Rule,
            ReviewDecisionSource::Model => ReviewSource::Model,
            ReviewDecisionSource::HumanFallback if review.reason == "review_sensitive_data" => {
                ReviewSource::Rule
            }
            ReviewDecisionSource::HumanFallback => ReviewSource::Model,
        };
        let decision = match review.decision {
            Decision::Allow => "allow",
            Decision::Ask => "ask",
            Decision::Deny => "deny",
            Decision::ModelReview => "model_review",
        };
        self.store
            .commit(&[StoreChange::Activity {
                run_id: self.run_id,
                payload: EventPayload::PermissionReviewed {
                    call_id: input.call.call_id.clone(),
                    decision: decision.into(),
                    source,
                    reason: review.reason.clone(),
                },
            }])
            .await?;
        if cancel.is_cancelled() {
            return Err(failure("cancelled", "authorization cancelled"));
        }
        if !self.current(&binding)? {
            return Ok(Authorization::Stale);
        }
        match review.decision {
            Decision::Deny => return Ok(Authorization::Denied("policy_denied")),
            Decision::Allow => {
                return Ok(Authorization::Permit(
                    self.permit(binding, input.call.call_id.clone()),
                ));
            }
            Decision::Ask => {}
            Decision::ModelReview => {
                return Err(failure("approval_error", "unresolved review decision"));
            }
        }
        if let Some((argv, cwd)) = input.operation.command_identity()
            && binding.mode_revision == self.initial_mode_revision
            && binding.permission_revision == self.initial_permission_revision
            && binding.permissions == self.initial_permissions
        {
            let preapproved = {
                let mut state = self.state.lock().map_err(|_| locked())?;
                if state.rejected.contains(&rejection_key(&binding)) {
                    return Ok(Authorization::Denied("previously_denied"));
                }
                if let Some(index) = state
                    .preapproved
                    .iter()
                    .position(|item| item.argv == argv && item.cwd == cwd)
                {
                    state.preapproved.remove(index);
                    true
                } else {
                    false
                }
            };
            if preapproved {
                self.store
                    .commit(&[StoreChange::Activity {
                        run_id: self.run_id,
                        payload: EventPayload::PermissionResolved {
                            approval_id: format!("preauthorization-{}", input.call.call_id),
                            call_id: input.call.call_id.clone(),
                            decision: "allow".into(),
                            source: ApprovalSource::Preauthorization,
                        },
                    }])
                    .await?;
                return Ok(Authorization::Permit(
                    self.permit(binding, input.call.call_id.clone()),
                ));
            }
        }
        let mut arguments: Value = serde_json::from_str(&binding.normalized_args)
            .map_err(|e| failure("invalid_args", e.to_string()))?;
        crate::redaction::redact(&mut arguments, &self.secrets)?;
        let mut summary = Value::String(input.summary.to_owned());
        crate::redaction::redact(&mut summary, &self.secrets)?;
        let request = ApprovalRequest {
            approval_id: uuid::Uuid::now_v7().to_string(),
            call_id: input.call.call_id.clone(),
            run_id: self.run_id,
            tool: input.call.name.clone(),
            operation_summary: summary.as_str().unwrap_or_default().to_owned(),
            arguments,
            cwd: input
                .operation
                .command_identity()
                .map_or_else(|| binding.cwd.clone(), |(_, cwd)| cwd),
            work_mode: binding.mode,
        };
        self.store
            .commit(&[StoreChange::ApprovalRequested(request.clone())])
            .await?;
        if !self.current(&binding)? {
            return Ok(Authorization::Stale);
        }
        let mut state = self.state.lock().map_err(|_| locked())?;
        if state.rejected.contains(&rejection_key(&binding)) {
            return Ok(Authorization::Denied("previously_denied"));
        }
        if let Some(existing) = state
            .pending
            .values()
            .find(|pending| pending.call_id == input.call.call_id && pending.binding == binding)
        {
            return Ok(if input.interactive {
                Authorization::Pending(existing.request.clone())
            } else {
                Authorization::NeedsConfirmation(existing.request.clone())
            });
        }
        state.pending.insert(
            request.approval_id.clone(),
            Pending {
                binding,
                call_id: input.call.call_id.clone(),
                request: request.clone(),
            },
        );
        Ok(if input.interactive {
            Authorization::Pending(request)
        } else {
            Authorization::NeedsConfirmation(request)
        })
    }
    /// Resolve a previously requested confirmation. The operation must still
    /// match, and a rejected normalized operation is remembered only this run.
    pub fn resolve(
        &self,
        approval_id: &str,
        approve: bool,
    ) -> Result<Authorization, BoundaryError> {
        if self.cancellation.is_cancelled() {
            return Err(failure("cancelled", "authorization cancelled"));
        }
        let pending = self
            .state
            .lock()
            .map_err(|_| locked())?
            .pending
            .remove(approval_id);
        let Some(pending) = pending else {
            return Ok(Authorization::Stale);
        };
        if !self.current(&pending.binding)? {
            return Ok(Authorization::Stale);
        }
        {
            let mut state = self.state.lock().map_err(|_| locked())?;
            if state.rejected.contains(&rejection_key(&pending.binding)) {
                return Ok(Authorization::Denied("previously_denied"));
            }
            if !approve {
                state.rejected.insert(rejection_key(&pending.binding));
                return Ok(Authorization::Denied("user_denied"));
            }
        }
        Ok(Authorization::Permit(
            self.permit(pending.binding, pending.call_id),
        ))
    }
    fn permit(&self, binding: Binding, call_id: String) -> Arc<OneShotPermit> {
        Arc::new(OneShotPermit {
            binding,
            call_id,
            policy: self.policy.clone(),
            modes: self.modes.clone(),
            permissions: self.permissions.clone(),
            cancellation: self.cancellation.clone(),
            state: self.state.clone(),
            checks: AtomicU8::new(0),
            lease_active: AtomicBool::new(false),
        })
    }
}
#[derive(Hash, PartialEq, Eq)]
struct RejectionKey {
    run_id: RunId,
    tool: String,
    normalized_args: String,
    cwd: PathBuf,
}
fn rejection_key(binding: &Binding) -> RejectionKey {
    RejectionKey {
        run_id: binding.run_id,
        tool: binding.tool.clone(),
        normalized_args: binding.normalized_args.clone(),
        cwd: binding.cwd.clone(),
    }
}
/// A single-operation, non-serializable grant. Tool adapters call it twice:
/// before intent persistence and immediately before dispatch. It becomes spent
/// on the second successful check; another call cannot reuse it.
pub struct OneShotPermit {
    binding: Binding,
    call_id: String,
    policy: Arc<Policy>,
    modes: ModeController,
    permissions: PermissionRevision,
    cancellation: Arc<dyn Cancellation>,
    state: Arc<Mutex<State>>,
    checks: AtomicU8,
    lease_active: AtomicBool,
}
impl OneShotPermit {
    pub(crate) fn authorize_todo(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<bool, BoundaryError> {
        self.check(call, context, &Operation::todo_write())
    }
    pub(crate) fn finish_todo(&self) -> Result<(), BoundaryError> {
        self.end()
    }
    pub(crate) fn consume_input(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<(), BoundaryError> {
        let operation = Operation::ask_user();
        if !self.check(call, context, &operation)? || !self.check(call, context, &operation)? {
            return Err(failure("stale_approval", "input approval is stale"));
        }
        self.end()
    }
    /// Use an exact, one-shot permit around one run-local background control.
    pub(crate) fn with_background_control<T>(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        action: impl FnOnce() -> Result<T, BoundaryError>,
    ) -> Result<T, BoundaryError> {
        let operation = Operation::background_control(&call.name)?;
        if !self.check(call, context, &operation)? || !self.check(call, context, &operation)? {
            return Err(failure(
                "stale_approval",
                "background control approval is stale",
            ));
        }
        let result = action();
        self.end()?;
        result
    }
    fn check(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        operation: &Operation,
    ) -> Result<bool, BoundaryError> {
        let snapshot = self.modes.snapshot()?;
        if self.cancellation.is_cancelled()
            || self.call_id != call.call_id
            || self.binding.run_id != context.run.id
            || self.binding.session_id != context.run.session_id
            || self.binding.tool != call.name
            || self.binding.mode != context.run.work_mode
            || self.binding.permissions != context.run.permissions
            || self.binding.mode != snapshot.current
            || snapshot.pending.is_some()
            || self.binding.mode_revision != snapshot.revision
            || self.permissions.snapshot()?
                != (self.binding.permissions, self.binding.permission_revision)
        {
            return Ok(false);
        }
        let cwd = std::fs::canonicalize(&context.workspace)
            .map_err(|e| failure("invalid_path", e.to_string()))?;
        if cwd != self.binding.cwd || operation.tool_name() != self.binding.tool {
            return Ok(false);
        }
        let args: Value = serde_json::from_str(&call.arguments)
            .map_err(|e| failure("invalid_args", e.to_string()))?;
        let normalized = serde_json::to_string(&operation.normalized_arguments(args)?)
            .map_err(|e| failure("invalid_args", e.to_string()))?;
        if normalized != self.binding.normalized_args
            || self
                .policy
                .evaluate(snapshot.current, context.run.permissions, operation)
                .decision
                == Decision::Deny
        {
            return Ok(false);
        }
        let state = self.state.lock().map_err(|_| locked())?;
        if state.rejected.contains(&rejection_key(&self.binding)) {
            return Ok(false);
        }
        match self
            .checks
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                if count < 2 { Some(count + 1) } else { None }
            }) {
            Ok(0) => Ok(true),
            Ok(1) => {
                if !self.modes.begin_shared(self.binding.mode_revision)? {
                    return Ok(false);
                }
                self.lease_active.store(true, Ordering::Release);
                let snapshot = self.modes.snapshot()?;
                if self.cancellation.is_cancelled()
                    || snapshot.pending.is_some()
                    || self.permissions.snapshot()?
                        != (self.binding.permissions, self.binding.permission_revision)
                {
                    self.end()?;
                    return Ok(false);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    fn end(&self) -> Result<(), BoundaryError> {
        if self.lease_active.swap(false, Ordering::AcqRel) {
            self.modes.finish()?;
        }
        Ok(())
    }
}
impl WriteGate for OneShotPermit {
    fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        normalized_target: &Path,
    ) -> Result<bool, BoundaryError> {
        let op = Operation::file(&context.workspace, &call.name, normalized_target)?;
        self.check(call, context, &op)
    }
    fn finish(&self) -> Result<(), BoundaryError> {
        self.end()
    }
}
impl ReadGate for OneShotPermit {
    fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        target: &Path,
    ) -> Result<bool, BoundaryError> {
        self.check(
            call,
            context,
            &Operation::file(&context.workspace, &call.name, target)?,
        )
    }
    fn finish(&self) -> Result<(), BoundaryError> {
        self.end()
    }
}
impl ExecGate for OneShotPermit {
    fn authorize(
        &self,
        plan: &ExecPlan,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<bool, BoundaryError> {
        self.check(call, context, &Operation::exec(plan)?)
    }
    fn finish(&self) -> Result<(), BoundaryError> {
        self.end()
    }
}
