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
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::{ExecConfig, ExecTool},
        read::{ReadTool, ReadToolKind, ReadTools},
        write::{DEFAULT_WRITE_MAX_BYTES, WriteTool, WriteToolKind, save_result},
    },
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

struct ObservedStore {
    inner: Arc<dyn Store>,
    call_id: String,
    intent: AtomicBool,
    finished: AtomicBool,
}
impl Store for ObservedStore {
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
                    StoreChange::ToolFinished { call_id, .. } if call_id == &self.call_id => {
                        self.finished.store(true, Ordering::Release)
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
    modes: ModeController,
    read_only: bool,
}
/// Review dependencies shared with the surrounding execution loop.
pub struct DispatchReview<'a> {
    pub model: &'a dyn Model,
    pub budget: &'a SharedBudget,
    pub config: &'a ReviewConfig,
}
impl ToolDispatcher {
    /// Return native tool schemas without executing or authorizing a call.
    pub fn definitions(&self) -> Vec<crate::boundary::ToolDefinition> {
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
                    DEFAULT_WRITE_MAX_BYTES,
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
        let manager = ApprovalManager::new(
            policy.clone(),
            modes.clone(),
            permissions,
            store.clone(),
            setup,
        )?;
        Ok(Self {
            manager,
            policy,
            files,
            store,
            cancel,
            exec_config,
            modes,
            read_only: false,
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
                self.exec_config.clone(),
            );
            Operation::exec(&planner.plan(call, context)?)?
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
    pub(crate) async fn execute_with_cancel(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        permit: Arc<OneShotPermit>,
        cancel: &dyn Cancellation,
        background: bool,
    ) -> Result<ToolResult, BoundaryError> {
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
        match call.name.as_str() {
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
                        save_result(store.as_ref(), call, context, &result).await?;
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
                    "exec" => Box::new(ExecTool::new(
                        self.files.clone(),
                        observed.clone(),
                        permit,
                        self.exec_config.clone(),
                    )),
                    name => Box::new(WriteTool::new(
                        self.files.clone(),
                        observed.clone(),
                        permit,
                        if name == "write_file" {
                            WriteToolKind::WriteFile
                        } else {
                            WriteToolKind::ApplyPatch
                        },
                        DEFAULT_WRITE_MAX_BYTES,
                    )),
                };
                let result = tool.execute(call, context, cancel).await?;
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
            _ => Err(failure("unknown_tool", "unsupported local tool")),
        }
    }
}
