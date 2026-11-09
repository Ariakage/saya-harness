//! Host-bound checkpoint tools using the common one-shot approval protocol.
use super::{AgentRuntime, CallProgress, error, failed, secrets};
use crate::{
    approvals::{Authorization, AuthorizationInput, OneShotPermit},
    boundary::{StoreChange, ToolContext, ToolDefinition},
    checkpoint::{
        ChangedFiles, CheckpointDiff, CheckpointSource, CheckpointStore, CheckpointToken,
        CheckpointWorkspace, PreparedRestore, PreparedSnapshot, RestoreStatus,
    },
    contract::{
        ApprovalSource, BoundaryError, EventPayload, HistoryItem, Session, ToolCall, ToolResult,
        WorkMode,
    },
    dispatch::DispatchReview,
    permissions::{Decision, Operation},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(test)]
#[path = "runtime_checkpoint/tests.rs"]
mod tests;

const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_RESULT_BYTES: usize = 64 * 1024;
const MAX_PAGE_ITEMS: usize = 32;
const MAX_REVIEW_BYTES: usize = 128 * 1024;

pub(super) struct CheckpointState {
    store: Arc<CheckpointStore>,
    protected_roots: Vec<PathBuf>,
    allowed_write_roots: Vec<PathBuf>,
}
impl CheckpointState {
    pub fn definitions(&self, mode: WorkMode, read_only: bool) -> Vec<ToolDefinition> {
        let token = json!({"type":"object","additionalProperties":false,"required":["id","sha256"],"properties":{"id":{"type":"string"},"sha256":{"type":"string"}}});
        let paths = json!({"type":"array","minItems":1,"maxItems":self.store.limits().max_files,"items":{"type":"string"}});
        let label = json!({"type":["string","null"],"maxLength":256});
        let mut definitions = Vec::new();
        for (name, description, properties, required) in [
            (
                "checkpoint_create",
                "Capture selected workspace text files into a private immutable checkpoint. This does not change workspace files.",
                json!({"paths":paths,"label":label}),
                vec!["paths"],
            ),
            (
                "checkpoint_list",
                "List this session's workspace checkpoints with bounded pagination. Use catalog_sha256 when continuing a page.",
                page_schema(None),
                vec![],
            ),
            (
                "checkpoint_diff",
                "Inspect a bounded page of changes from one immutable checkpoint to observed workspace files.",
                page_schema(Some(token.clone())),
                vec!["checkpoint"],
            ),
            (
                "checkpoint_accept",
                "Accept selected current files into a new checkpoint baseline. Other baseline files remain unchanged.",
                json!({"checkpoint":token,"paths":paths,"label":label}),
                vec!["checkpoint", "paths"],
            ),
            (
                "checkpoint_restore",
                "Restore selected checkpoint files using exact observed versions. Multi-file restoration may be partial. Inspect every reported outcome and never replay an unknown operation.",
                json!({"checkpoint":token,"paths":paths,"label":label}),
                vec!["checkpoint", "paths"],
            ),
        ] {
            if name == "checkpoint_restore"
                && (mode == WorkMode::Plan || read_only || self.allowed_write_roots.is_empty())
            {
                continue;
            }
            definitions.push(ToolDefinition { name: name.into(), description: description.into(), input_schema: json!({"type":"object","additionalProperties":false,"properties":properties,"required":required}) });
        }
        definitions
    }

    fn prepare(
        &self,
        session: &Session,
        call: &ToolCall,
        context: &ToolContext,
        cancel: &AtomicBool,
    ) -> Result<Preparation, BoundaryError> {
        let action = parse(call, self.store.limits().max_files)?;
        let canonical = std::fs::canonicalize(&session.workspace)
            .map_err(|_| error("checkpoint_scope", "Workspace is unavailable"))?;
        let workspace = self
            .store
            .bind_workspace(&canonical, session.id, &self.protected_roots)?;
        let source = CheckpointSource {
            session_id: session.id,
            run_id: context.run.id,
            revision: session.history.len() as u64,
            label: action.label(),
        };
        let (action, selected, metadata) = match action {
            Arguments::Create(args) => {
                let prepared = workspace.prepare_capture(&args.paths, source, cancel)?;
                let metadata = json!({"kind":"capture","observations":prepared.observations(),"source":prepared.source()});
                let paths = prepared.paths();
                (Action::Snapshot(prepared), paths, metadata)
            }
            Arguments::Accept(args) => {
                let prepared =
                    workspace.prepare_accept(&args.checkpoint, &args.paths, source, cancel)?;
                let metadata = json!({"kind":"accept","checkpoint":args.checkpoint,"observations":prepared.observations(),"source":prepared.source()});
                let paths = prepared.paths();
                (Action::Snapshot(prepared), paths, metadata)
            }
            Arguments::Restore(args) => {
                if context.run.work_mode == WorkMode::Plan {
                    return Err(error(
                        "mode_denied",
                        "PLAN does not restore workspace files",
                    ));
                }
                let prepared =
                    workspace.prepare_restore(&args.checkpoint, &args.paths, source, cancel)?;
                let paths = prepared
                    .changes()
                    .iter()
                    .map(|change| change.path.clone())
                    .collect::<Vec<_>>();
                for path in &paths {
                    self.restore_target(&canonical.join(path))?;
                }
                let metadata = json!({"kind":"restore","checkpoint":args.checkpoint,"operation_id":prepared.operation_id(),"changes":prepared.changes(),"source":prepared.source()});
                (Action::Restore(prepared), paths, metadata)
            }
            Arguments::List(page) => {
                let mut values = workspace.list()?;
                values.sort_by(|left, right| {
                    left.source
                        .revision
                        .cmp(&right.source.revision)
                        .then(left.token.id.cmp(&right.token.id))
                });
                let value = page_result(values, &page, "checkpoints", false, json!({}))?;
                (
                    Action::Read(value),
                    Vec::new(),
                    json!({"kind":"list","page":page}),
                )
            }
            Arguments::Diff(args) => {
                let diff = workspace.diff(&args.checkpoint, cancel)?;
                let paths = diff
                    .files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>();
                let metadata = json!({"kind":"diff","checkpoint":diff.checkpoint,"observations":diff.files.iter().map(|file| json!({"path":file.path,"current":file.current,"baseline":file.baseline})).collect::<Vec<_>>()});
                let value = diff_result(diff, &args.page())?;
                (Action::Read(value), paths, metadata)
            }
        };
        let mut paths = selected
            .iter()
            .map(|path| canonical.join(path))
            .collect::<Vec<_>>();
        // Store access is a distinct managed capability. Ordinary file tools
        // remain excluded by dispatcher.protect_host_paths.
        paths.push(self.store.protected_path().to_owned());
        let metadata = json!({"schema_version":1,"workspace":workspace.identity(),"targets":selected,"prepared":metadata,
            "session_history_sha256":crate::context_builder::history_digest(&session.history)?});
        let encoded = serde_json::to_vec(&metadata).map_err(|_| {
            error(
                "checkpoint_limit",
                "Checkpoint review metadata cannot be encoded",
            )
        })?;
        if encoded.len() > MAX_REVIEW_BYTES {
            return Err(error(
                "checkpoint_limit",
                "Checkpoint review metadata exceeds its limit",
            ));
        }
        let binding = crate::memory_context::digest(&encoded);
        let write = matches!(action, Action::Restore(_));
        let operation = Operation::managed(&call.name, paths.clone(), binding, !write, metadata)?;
        Ok(Preparation {
            workspace,
            action,
            operation,
            paths,
        })
    }

    fn restore_target(&self, path: &Path) -> Result<(), BoundaryError> {
        let resolved = resolve_future(path)?;
        if !self
            .allowed_write_roots
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            return Err(error(
                "path_denied",
                "Restore target is outside host-approved writable roots",
            ));
        }
        if self
            .protected_roots
            .iter()
            .any(|root| protected_overlap(&resolved, root))
            || protected_overlap(&resolved, self.store.protected_path())
        {
            return Err(error(
                "path_denied",
                "Restore target addresses protected host storage",
            ));
        }
        Ok(())
    }
}

struct Preparation {
    workspace: CheckpointWorkspace,
    action: Action,
    operation: Operation,
    paths: Vec<PathBuf>,
}
enum Action {
    Snapshot(PreparedSnapshot),
    Restore(PreparedRestore),
    Read(ToolResult),
}
struct ManagedLease<'a>(&'a OneShotPermit);
impl Drop for ManagedLease<'_> {
    fn drop(&mut self) {
        let _ = self.0.finish_managed();
    }
}

impl AgentRuntime {
    pub fn with_checkpoints(
        mut self,
        store: Arc<CheckpointStore>,
        protected_roots: Vec<PathBuf>,
        allowed_write_roots: Vec<PathBuf>,
    ) -> Result<Self, BoundaryError> {
        if protected_roots.len() > 256 || allowed_write_roots.len() > 256 {
            return Err(error(
                "checkpoint_config",
                "Checkpoint host root count exceeds its limit",
            ));
        }
        let protected_roots = protected_roots
            .iter()
            .map(|path| resolve_future(path))
            .collect::<Result<Vec<_>, _>>()?;
        let allowed_write_roots = allowed_write_roots
            .iter()
            .map(|path| resolve_future(path))
            .collect::<Result<Vec<_>, _>>()?;
        self.dispatcher
            .protect_host_paths(vec![store.protected_path().to_owned()])?;
        self.checkpoints = Some(Arc::new(CheckpointState {
            store,
            protected_roots,
            allowed_write_roots,
        }));
        Ok(self)
    }

    pub(super) async fn execute_checkpoint(
        &self,
        session: &mut Session,
        call: &ToolCall,
        context: &ToolContext,
        progress: &mut CallProgress,
    ) -> Result<ToolResult, BoundaryError> {
        let Some(state) = self.checkpoints.clone() else {
            return self
                .save_local(
                    session,
                    call,
                    failed("unknown_tool", "Checkpoints are unavailable"),
                )
                .await;
        };
        if call.name == "checkpoint_restore"
            && (context.run.work_mode == WorkMode::Plan || self.read_only)
        {
            return self
                .save_local(
                    session,
                    call,
                    failed(
                        "mode_denied",
                        "Workspace restoration is unavailable in this run mode",
                    ),
                )
                .await;
        }
        let saved = self
            .store
            .load_session(session.id)
            .await?
            .ok_or_else(|| error("storage_error", "Checkpoint source session is absent"))?;
        if saved != *session {
            return Err(error(
                "session_conflict",
                "Checkpoint source differs from persisted history",
            ));
        }
        let state_copy = state.clone();
        let call_copy = call.clone();
        let context_copy = context.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let deadline =
            tokio::time::Instant::now() + self.limits.tool_timeout.min(self.budget.remaining()?);
        let mut worker = tokio::task::spawn_blocking(move || {
            state_copy.prepare(&saved, &call_copy, &context_copy, &worker_cancel)
        });
        let (prepared, stopped) = tokio::select! {
            result = &mut worker => (result,None),
            _ = self.cancel.cancelled() => { cancel.store(true, Ordering::Release); (worker.await,Some("cancelled")) },
            _ = tokio::time::sleep_until(deadline) => { cancel.store(true, Ordering::Release); (worker.await,Some("tool_timeout")) },
        };
        let prepared =
            prepared.map_err(|_| error("checkpoint_io", "Checkpoint preparation worker failed"))?;
        if let Some(reason) =
            stopped.or_else(|| (tokio::time::Instant::now() >= deadline).then_some("tool_timeout"))
        {
            return self
                .save_local(
                    session,
                    call,
                    failed(
                        reason,
                        "Checkpoint preparation stopped before authorization or dispatch",
                    ),
                )
                .await;
        }
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(failure) => {
                return self
                    .save_local(session, call, failed(&failure.0.code, &failure.0.message))
                    .await;
            }
        };
        self.check()?;
        let authorization = self
            .dispatcher
            .authorize_managed(
                AuthorizationInput {
                    operation: &prepared.operation,
                    call,
                    context,
                    summary: &self.summary,
                    interactive: self.confirmation.is_some(),
                },
                DispatchReview {
                    model: self.model.as_ref(),
                    budget: &self.budget,
                    config: &self.review,
                },
            )
            .await?;
        let authorization = match authorization {
            Authorization::Pending(request) => match self.confirm(&request).await? {
                Some(answer) => {
                    self.store
                        .commit(&[StoreChange::Activity {
                            run_id: self.run.id,
                            payload: EventPayload::PermissionResolved {
                                approval_id: request.approval_id.clone(),
                                call_id: call.call_id.clone(),
                                decision: if answer { "allow" } else { "deny" }.into(),
                                source: ApprovalSource::User,
                            },
                        }])
                        .await?;
                    self.dispatcher.resolve(&request.approval_id, answer)?
                }
                None => {
                    return self
                        .save_local(
                            session,
                            call,
                            failed(
                                "needs_confirmation",
                                "Checkpoint confirmation is unavailable",
                            ),
                        )
                        .await;
                }
            },
            other => other,
        };
        let permit = match authorization {
            Authorization::Permit(permit) => permit,
            Authorization::Denied(_) => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("permission_denied", "Checkpoint operation was denied"),
                    )
                    .await;
            }
            Authorization::Stale => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("stale_approval", "Checkpoint approval changed"),
                    )
                    .await;
            }
            _ => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("needs_confirmation", "Checkpoint confirmation is required"),
                    )
                    .await;
            }
        };
        if !permit.authorize_managed(call, context, &prepared.operation)? {
            return self
                .save_local(
                    session,
                    call,
                    failed(
                        "stale_approval",
                        "Checkpoint approval changed before intent persistence",
                    ),
                )
                .await;
        }
        self.check()?;
        self.store
            .commit(&[StoreChange::ToolIntent {
                run_id: self.run.id,
                call: call.clone(),
            }])
            .await?;
        *progress = CallProgress::Dispatched;
        let current = self
            .store
            .load_session(session.id)
            .await?
            .ok_or_else(|| error("storage_error", "Checkpoint source session is absent"))?;
        if current != *session {
            return Err(error(
                "session_conflict",
                "Checkpoint source changed before dispatch",
            ));
        }
        let modes = self.modes.clone();
        let policy = self.dispatcher.clone();
        let call_copy = call.clone();
        let context_copy = context.clone();
        let source = self.secret_source.clone();
        let policy_fingerprint = self.dispatcher.permission_fingerprint();
        let expected_mode = self.modes.snapshot()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            let _lease = ManagedLease(permit.as_ref());
            if !permit.authorize_managed(&call_copy, &context_copy, &prepared.operation)? {
                return Ok(failed(
                    "stale_approval",
                    "Checkpoint approval changed before dispatch",
                ));
            }
            let check = |path: Option<&Path>| -> Result<(), BoundaryError> {
                if worker_cancel.load(Ordering::Acquire) {
                    return Err(error("cancelled", "Checkpoint operation was cancelled"));
                }
                let current = modes.snapshot()?;
                if current.pending.is_some()
                    || current.current != context_copy.run.work_mode
                    || current.revision != expected_mode.revision
                    || policy.permission_fingerprint() != policy_fingerprint
                    || policy
                        .managed_context_decision(
                            current.current,
                            context_copy.run.permissions,
                            &prepared.operation,
                        )
                        .decision
                        == Decision::Deny
                {
                    return Err(error(
                        "stale_approval",
                        "Checkpoint dispatch policy changed",
                    ));
                }
                if let Some(path) = path {
                    state.restore_target(path)?;
                }
                Ok(())
            };
            check(None)?;
            let action = prepared.action;
            let snapshot_action = matches!(action, Action::Snapshot(_));
            let result = match action {
                Action::Read(result) => Ok(result),
                Action::Snapshot(snapshot) => prepared
                    .workspace
                    .publish_snapshot(snapshot, &worker_cancel)
                    .and_then(|summary| result(json!({"checkpoint":summary}), true, None, false)),
                Action::Restore(restore) => {
                    let allowed = restore.changes().to_vec();
                    prepared
                        .workspace
                        .apply(&restore, &worker_cancel, |operation| {
                            if !allowed.iter().any(|approved| approved == operation) {
                                return Err(error(
                                    "stale_approval",
                                    "Restore operation differs from its approved observation",
                                ));
                            }
                            let path = prepared.workspace.identity().path.join(&operation.path);
                            if !prepared.paths.contains(&path) {
                                return Err(error(
                                    "stale_approval",
                                    "Restore target differs from the approved target set",
                                ));
                            }
                            check(Some(&path))
                        })
                        .and_then(restore_result)
                }
            };
            match result {
                Ok(result) => secrets::redact(result, &source),
                Err(mut failure) if failure.2.is_some() => {
                    if let Some(actual) = failure.2.take() {
                        let actual = secrets::redact(*actual, &source)?;
                        failure.2 = Some(Box::new(bounded_actual_result(actual)?));
                    }
                    Err(failure)
                }
                Err(failure)
                    if matches!(
                        failure.0.code.as_str(),
                        "checkpoint_pending" | "checkpoint_storage"
                    ) || (snapshot_action
                        && matches!(
                            failure.0.code.as_str(),
                            "checkpoint_io" | "checkpoint_unknown"
                        )) =>
                {
                    Ok(failed(
                        "outcome_unknown",
                        "Checkpoint publication requires host inspection and must not be replayed",
                    ))
                }
                Err(failure) => Ok(failed(&failure.0.code, &failure.0.message)),
            }
        });
        let outcome = tokio::select! {
            result = &mut worker => result,
            _ = self.cancel.cancelled() => { cancel.store(true, Ordering::Release); worker.await },
            _ = tokio::time::sleep(self.limits.tool_timeout.min(self.budget.remaining()?)) => { cancel.store(true, Ordering::Release); worker.await },
        }.map_err(|_| error("outcome_unknown", "Checkpoint worker completion is unknown"))?;
        let result = outcome?;
        let item = HistoryItem::ToolResult {
            call_id: call.call_id.clone(),
            result: result.clone(),
        };
        if let Err(mut failure) = self
            .store
            .commit(&[
                StoreChange::ToolFinished {
                    run_id: self.run.id,
                    call_id: call.call_id.clone(),
                    result: result.clone(),
                },
                StoreChange::History {
                    session_id: session.id,
                    item: item.clone(),
                },
            ])
            .await
        {
            failure.2 = Some(Box::new(result));
            return Err(failure);
        }
        session.history.push(item);
        Ok(result)
    }
}

fn page_schema(token: Option<Value>) -> Value {
    let mut properties = json!({"offset":{"type":["integer","null"],"minimum":0},"max_items":{"type":["integer","null"],"minimum":1,"maximum":MAX_PAGE_ITEMS},
        "max_bytes":{"type":["integer","null"],"minimum":1024,"maximum":MAX_RESULT_BYTES},"catalog_sha256":{"type":["string","null"]}});
    if let Some(token) = token {
        properties["checkpoint"] = token;
    }
    properties
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    offset: Option<usize>,
    max_items: Option<usize>,
    max_bytes: Option<usize>,
    catalog_sha256: Option<String>,
}
impl Page {
    fn validate(&self) -> Result<(), BoundaryError> {
        if self
            .max_items
            .is_some_and(|count| count == 0 || count > MAX_PAGE_ITEMS)
            || self
                .max_bytes
                .is_some_and(|bytes| !(1024..=MAX_RESULT_BYTES).contains(&bytes))
            || self.offset.is_some_and(|offset| offset > 1024)
            || self
                .catalog_sha256
                .as_ref()
                .is_some_and(|hash| !valid_hash(hash))
            || (self.offset.unwrap_or(0) > 0 && self.catalog_sha256.is_none())
        {
            return Err(error("invalid_args", "Checkpoint page bounds are invalid"));
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    paths: Vec<String>,
    label: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectArgs {
    checkpoint: CheckpointToken,
    paths: Vec<String>,
    label: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffArgs {
    checkpoint: CheckpointToken,
    offset: Option<usize>,
    max_items: Option<usize>,
    max_bytes: Option<usize>,
    catalog_sha256: Option<String>,
}
impl DiffArgs {
    fn page(&self) -> Page {
        Page {
            offset: self.offset,
            max_items: self.max_items,
            max_bytes: self.max_bytes,
            catalog_sha256: self.catalog_sha256.clone(),
        }
    }
}
enum Arguments {
    Create(CreateArgs),
    List(Page),
    Diff(DiffArgs),
    Accept(SelectArgs),
    Restore(SelectArgs),
}
impl Arguments {
    fn label(&self) -> String {
        match self {
            Self::Create(args) => args.label.clone(),
            Self::Accept(args) | Self::Restore(args) => args.label.clone(),
            _ => None,
        }
        .unwrap_or_else(|| "Selected workspace checkpoint".into())
    }
}
fn parse(call: &ToolCall, max_files: usize) -> Result<Arguments, BoundaryError> {
    if call.arguments.len() > MAX_ARGUMENT_BYTES {
        return Err(error(
            "invalid_args",
            "Checkpoint arguments exceed their byte limit",
        ));
    }
    let invalid = |_| {
        error(
            "invalid_args",
            "Checkpoint arguments do not match the tool schema",
        )
    };
    let args = match call.name.as_str() {
        "checkpoint_create" => {
            Arguments::Create(serde_json::from_str(&call.arguments).map_err(invalid)?)
        }
        "checkpoint_list" => {
            Arguments::List(serde_json::from_str(&call.arguments).map_err(invalid)?)
        }
        "checkpoint_diff" => {
            Arguments::Diff(serde_json::from_str(&call.arguments).map_err(invalid)?)
        }
        "checkpoint_accept" => {
            Arguments::Accept(serde_json::from_str(&call.arguments).map_err(invalid)?)
        }
        "checkpoint_restore" => {
            Arguments::Restore(serde_json::from_str(&call.arguments).map_err(invalid)?)
        }
        _ => return Err(error("unknown_tool", "Checkpoint tool is unknown")),
    };
    if args.label().is_empty()
        || args.label().len() > 256
        || args.label().chars().any(crate::text::is_terminal_control)
    {
        return Err(error("invalid_args", "Checkpoint label is invalid"));
    }
    let paths = match &args {
        Arguments::Create(args) => Some(&args.paths),
        Arguments::Accept(args) | Arguments::Restore(args) => Some(&args.paths),
        _ => None,
    };
    if let Some(paths) = paths {
        let mut seen = BTreeSet::new();
        if paths.is_empty()
            || paths.len() > max_files
            || paths.iter().any(|path| {
                path.is_empty()
                    || path.len() > 1024
                    || path.chars().any(crate::text::is_terminal_control)
                    || path.contains('\\')
                    || path.split('/').count() > 16
                    || path
                        .split('/')
                        .any(|part| part.is_empty() || matches!(part, "." | ".."))
                    || !seen.insert(path)
            })
        {
            return Err(error(
                "invalid_args",
                "Checkpoint paths are invalid or repeated",
            ));
        }
    }
    match &args {
        Arguments::List(page) => page.validate()?,
        Arguments::Diff(args) => {
            validate_token(&args.checkpoint)?;
            args.page().validate()?;
        }
        Arguments::Accept(args) | Arguments::Restore(args) => validate_token(&args.checkpoint)?,
        _ => {}
    }
    Ok(args)
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn validate_token(token: &CheckpointToken) -> Result<(), BoundaryError> {
    if !uuid::Uuid::parse_str(&token.id).is_ok_and(|id| id.to_string() == token.id)
        || !valid_hash(&token.sha256)
    {
        return Err(error(
            "invalid_args",
            "Checkpoint token identity is invalid",
        ));
    }
    Ok(())
}
fn result(
    data: Value,
    ok: bool,
    error: Option<crate::contract::ErrorInfo>,
    truncated: bool,
) -> Result<ToolResult, BoundaryError> {
    let result = ToolResult {
        ok,
        data: Some(data),
        error,
        truncated,
    };
    if serde_json::to_vec(&result)
        .map_err(|_| {
            crate::runtime::error("checkpoint_limit", "Checkpoint result cannot be encoded")
        })?
        .len()
        > MAX_RESULT_BYTES
    {
        return Err(crate::runtime::error(
            "checkpoint_limit",
            "Checkpoint result exceeds its byte limit",
        ));
    }
    Ok(result)
}
fn page_result<T: Serialize>(
    items: Vec<T>,
    page: &Page,
    key: &str,
    incomplete: bool,
    extra: Value,
) -> Result<ToolResult, BoundaryError> {
    page.validate()?;
    let values = serde_json::to_value(items)
        .map_err(|_| error("checkpoint_limit", "Checkpoint page cannot be encoded"))?;
    let all = values
        .as_array()
        .ok_or_else(|| error("checkpoint_limit", "Checkpoint page is invalid"))?;
    let hash = if let Some(hash) = extra.get("catalog_sha256").and_then(Value::as_str) {
        hash.to_owned()
    } else {
        crate::memory_context::digest(
            &serde_json::to_vec(&values)
                .map_err(|_| error("checkpoint_limit", "Checkpoint page cannot be hashed"))?,
        )
    };
    if page
        .catalog_sha256
        .as_ref()
        .is_some_and(|expected| *expected != hash)
    {
        return Err(error(
            "checkpoint_conflict",
            "Checkpoint page source changed",
        ));
    }
    let offset = page.offset.unwrap_or(0);
    if offset > all.len() {
        return Err(error(
            "invalid_args",
            "Checkpoint page offset is outside its source",
        ));
    }
    let mut selected = Vec::new();
    let mut next = offset;
    for value in all
        .iter()
        .skip(offset)
        .take(page.max_items.unwrap_or(MAX_PAGE_ITEMS))
    {
        let mut candidate = selected.clone();
        candidate.push(value.clone());
        let mut data = extra.clone();
        data[key] = json!(candidate);
        data["catalog_sha256"] = json!(hash);
        data["total_items"] = json!(all.len());
        data["next_offset"] = json!(if next + 1 < all.len() {
            Some(next + 1)
        } else {
            None
        });
        let encoded = serde_json::to_vec(&ToolResult {
            ok: true,
            data: Some(data),
            error: None,
            truncated: incomplete || next + 1 < all.len(),
        })
        .map_err(|_| error("checkpoint_limit", "Checkpoint page cannot be encoded"))?;
        if encoded.len() > page.max_bytes.unwrap_or(MAX_RESULT_BYTES) {
            break;
        }
        selected.push(value.clone());
        next += 1;
    }
    if next == offset && offset < all.len() {
        return Err(error(
            "checkpoint_limit",
            "A checkpoint item exceeds the selected page limit",
        ));
    }
    let more = next < all.len();
    let mut data = extra;
    data[key] = json!(selected);
    data["catalog_sha256"] = json!(hash);
    data["total_items"] = json!(all.len());
    data["next_offset"] = json!(if more { Some(next) } else { None });
    let output = result(data, true, None, incomplete || more)?;
    if serde_json::to_vec(&output)
        .map_err(|_| error("checkpoint_limit", "Checkpoint page cannot be encoded"))?
        .len()
        > page.max_bytes.unwrap_or(MAX_RESULT_BYTES)
    {
        return Err(error(
            "checkpoint_limit",
            "Checkpoint page metadata exceeds its byte limit",
        ));
    }
    Ok(output)
}
fn diff_result(mut diff: CheckpointDiff, page: &Page) -> Result<ToolResult, BoundaryError> {
    let source_hash = crate::memory_context::digest(
        &serde_json::to_vec(&diff)
            .map_err(|_| error("checkpoint_limit", "Checkpoint diff cannot be hashed"))?,
    );
    for file in &mut diff.files {
        if serde_json::to_vec(file)
            .map_err(|_| error("checkpoint_limit", "Checkpoint diff cannot be encoded"))?
            .len()
            > page
                .max_bytes
                .unwrap_or(MAX_RESULT_BYTES)
                .saturating_sub(2048)
        {
            file.unified_diff = None;
            file.preview_truncated = true;
            diff.preview_truncated = true;
        }
    }
    page_result(
        diff.files,
        page,
        "files",
        diff.preview_truncated,
        json!({"checkpoint":diff.checkpoint,"source":diff.source,"catalog_sha256":source_hash}),
    )
}
fn restore_result(mut changed: ChangedFiles) -> Result<ToolResult, BoundaryError> {
    let count = changed.files.len();
    let applied = changed
        .files
        .iter()
        .filter(|file| file.publication_observed)
        .count();
    let mut omitted = count.saturating_sub(MAX_PAGE_ITEMS);
    changed.files.truncate(MAX_PAGE_ITEMS);
    let status = changed.status;
    let mut data = serde_json::to_value(changed)
        .map_err(|_| error("checkpoint_limit", "Restore outcome cannot be encoded"))?;
    data["total_files"] = json!(count);
    data["applied_files"] = json!(applied);
    data["files_omitted"] = json!(omitted);
    while serde_json::to_vec(&data)
        .map_err(|_| error("checkpoint_limit", "Restore outcome cannot be encoded"))?
        .len()
        > MAX_RESULT_BYTES - 1024
    {
        let Some(files) = data.get_mut("files").and_then(Value::as_array_mut) else {
            return Err(error(
                "checkpoint_limit",
                "Restore outcome file metadata is invalid",
            ));
        };
        if files.pop().is_none() {
            return Err(error(
                "checkpoint_limit",
                "Restore outcome metadata exceeds its limit",
            ));
        }
        omitted += 1;
        data["files_omitted"] = json!(omitted);
    }
    let complete = status == RestoreStatus::Completed;
    let info = (!complete).then(|| crate::contract::ErrorInfo {
        code: if status == RestoreStatus::Unknown {
            "outcome_unknown"
        } else {
            "checkpoint_incomplete"
        }
        .into(),
        message: "Inspect actual per-file restore outcomes before continuing".into(),
    });
    result(data, complete, info, omitted > 0)
}
fn bounded_actual_result(mut actual: ToolResult) -> Result<ToolResult, BoundaryError> {
    if let Some(data) = &actual.data
        && let Ok(changed) = serde_json::from_value::<ChangedFiles>(data.clone())
    {
        let mut bounded = restore_result(changed)?;
        bounded.ok = actual.ok;
        bounded.error = actual.error;
        bounded.truncated |= actual.truncated;
        return Ok(bounded);
    }
    if serde_json::to_vec(&actual)
        .map_err(|_| {
            error(
                "checkpoint_limit",
                "Actual checkpoint outcome cannot be encoded",
            )
        })?
        .len()
        > MAX_RESULT_BYTES
    {
        let data = actual.data.as_ref();
        let files = data
            .and_then(|data| data.get("files"))
            .and_then(Value::as_array);
        actual.data = Some(
            json!({"operation_id":data.and_then(|data|data.get("operation_id")),"status":data.and_then(|data|data.get("status")),
            "total_files":files.map(Vec::len),"applied_files":files.map(|files|files.iter().filter(|file|file["publication_observed"]==true).count()),
            "files_omitted":files.map(Vec::len),"actual_result_withheld":"Checkpoint failure metadata exceeds its result limit"}),
        );
        actual.truncated = true;
    }
    Ok(actual)
}
fn resolve_future(path: &Path) -> Result<PathBuf, BoundaryError> {
    if !path.is_absolute()
        || path.as_os_str().len() > 4096
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(error(
            "checkpoint_config",
            "Checkpoint host paths must be bounded absolute paths",
        ));
    }
    let mut parent = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(&parent) {
            Ok(mut resolved) => {
                for name in suffix.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&parent).is_ok() {
                    return Err(error(
                        "checkpoint_path",
                        "Checkpoint target has a dangling link",
                    ));
                }
                suffix.push(
                    parent
                        .file_name()
                        .ok_or_else(|| {
                            error(
                                "checkpoint_path",
                                "Checkpoint path has no existing ancestor",
                            )
                        })?
                        .to_owned(),
                );
                parent.pop();
            }
            Err(_) => {
                return Err(error(
                    "checkpoint_path",
                    "Checkpoint path cannot be resolved safely",
                ));
            }
        }
    }
}
fn protected_overlap(path: &Path, root: &Path) -> bool {
    use unicode_normalization::UnicodeNormalization;
    let fold = |path: &Path| {
        unicase::UniCase::unicode(path.to_string_lossy().nfc().collect::<String>())
            .to_folded_case()
            .nfc()
            .collect::<String>()
    };
    let path = fold(path);
    let root = fold(root);
    let root = root.trim_end_matches('/');
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
