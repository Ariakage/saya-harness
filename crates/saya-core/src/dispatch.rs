//! Coordinated local-tool authorization and dispatch. The caller owns the agent
//! loop, passes the same run budget to review, and handles confirmation outcomes.
//! Volatile stores are appropriate only for controlled tests, not real workspace
//! side effects or recovery.
use crate::{
    approvals::{
        ApprovalManager, ApprovalSetup, Authorization, AuthorizationInput, OneShotPermit,
        PermissionRevision,
    },
    boundary::{Cancellation, Model, Store, StoreChange, Tool, ToolContext},
    contract::{BoundaryError, ErrorInfo, ToolCall, ToolResult},
    mode::ModeController,
    permissions::{Operation, Policy, PolicyGate},
    review::{ReviewConfig, SharedBudget},
    tools::{
        exec::{ExecConfig, ExecTool},
        read::{ReadTool, ReadToolKind, ReadTools},
        write::{DEFAULT_WRITE_MAX_BYTES, WriteTool, WriteToolKind, save_result},
    },
};
use std::{path::Path, sync::Arc};

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
}
/// Review dependencies shared with the surrounding execution loop.
pub struct DispatchReview<'a> {
    pub model: &'a dyn Model,
    pub budget: &'a SharedBudget,
    pub config: &'a ReviewConfig,
}
impl ToolDispatcher {
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
        let manager =
            ApprovalManager::new(policy.clone(), modes, permissions, store.clone(), setup)?;
        Ok(Self {
            manager,
            policy,
            files,
            store,
            cancel,
            exec_config,
        })
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
        if !self.manager.owns_permit(&permit) {
            return Err(failure(
                "permission_denied",
                "permit belongs to another dispatcher",
            ));
        }
        match call.name.as_str() {
            "read_file" | "list_dir" | "find_files" | "search_text" => {
                let kind = match call.name.as_str() {
                    "read_file" => ReadToolKind::ReadFile,
                    "list_dir" => ReadToolKind::ListDir,
                    "find_files" => ReadToolKind::FindFiles,
                    _ => ReadToolKind::SearchText,
                };
                self.store
                    .commit(&[StoreChange::ToolIntent {
                        run_id: context.run.id,
                        call: call.clone(),
                    }])
                    .await?;
                let tool = ReadTool::with_gate(self.files.clone(), kind, permit);
                match tool.execute(call, context, self.cancel.as_ref()).await {
                    Ok(result) => {
                        save_result(self.store.as_ref(), call, context, &result).await?;
                        Ok(result)
                    }
                    Err(error) => {
                        let result = ToolResult {
                            ok: false,
                            data: None,
                            error: Some(error.0.clone()),
                            truncated: false,
                        };
                        save_result(self.store.as_ref(), call, context, &result).await?;
                        Err(error)
                    }
                }
            }
            "write_file" | "apply_patch" => {
                let kind = if call.name == "write_file" {
                    WriteToolKind::WriteFile
                } else {
                    WriteToolKind::ApplyPatch
                };
                WriteTool::new(
                    self.files.clone(),
                    self.store.clone(),
                    permit,
                    kind,
                    DEFAULT_WRITE_MAX_BYTES,
                )
                .execute(call, context, self.cancel.as_ref())
                .await
            }
            "exec" => {
                ExecTool::new(
                    self.files.clone(),
                    self.store.clone(),
                    permit,
                    self.exec_config.clone(),
                )
                .execute(call, context, self.cancel.as_ref())
                .await
            }
            _ => Err(failure("unknown_tool", "unsupported local tool")),
        }
    }
}
