//! Run-scoped background tool jobs. This supervisor does not outlive its run;
//! it never restores or replays a job and is not an operating-system sandbox.
use crate::{
    approvals::{Authorization, OneShotPermit},
    boundary::{Cancellation, ToolContext},
    contract::{ApprovalRequest, BoundaryError, ErrorInfo, ToolCall, ToolResult, WorkMode},
    dispatch::{DispatchReview, ToolDispatcher},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, watch};
use tracing::{Instrument, instrument::WithSubscriber};

pub const DEFAULT_MAX_BACKGROUND_RUNNING: usize = 4;
pub const DEFAULT_MAX_BACKGROUND_JOBS: usize = 256;

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
#[derive(Clone)]
struct JobCancel(watch::Sender<bool>);
impl JobCancel {
    fn new() -> Self {
        Self(watch::channel(false).0)
    }
    fn cancel(&self) {
        self.0.send_replace(true);
    }
}
impl Cancellation for JobCancel {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut rx = self.0.subscribe();
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}
struct Combined {
    root: Arc<dyn Cancellation>,
    job: JobCancel,
}
impl Cancellation for Combined {
    fn is_cancelled(&self) -> bool {
        self.root.is_cancelled() || self.job.is_cancelled()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            tokio::select! { () = self.root.cancelled() => (), () = self.job.cancelled() => () }
        })
    }
}
/// A snapshot of a run-local job. Errors retain any observed tool result.
#[derive(Debug, Clone)]
pub enum JobStatus {
    Running,
    Finished(ToolResult),
    Failed(BoundaryError),
}
/// Run-local observation of a background job transition. This is an in-memory
/// control-plane record, not the durable schema-v1 event stream delivered in P6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundEvent {
    pub sequence: u64,
    pub job_id: String,
    pub kind: BackgroundEventKind,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundEventKind {
    /// The job was accepted for scheduling; its tool has not necessarily begun.
    Accepted {
        call_id: String,
        tool: String,
    },
    CancellationRequested {
        reason: BackgroundCancelReason,
    },
    Finished {
        outcome: BackgroundOutcome,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundCancelReason {
    User,
    Plan,
    Shutdown,
    Failure,
    Timeout,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundOutcome {
    Succeeded,
    Failed { code: String },
    Cancelled,
}
/// Authorization result for an explicitly requested background operation.
pub enum JobSubmission {
    Started { job_id: String },
    Pending(PendingJob),
    NeedsConfirmation(PendingJob),
    Denied(&'static str),
    Stale,
}
pub struct PendingJob {
    pub request: ApprovalRequest,
    call: ToolCall,
    context: ToolContext,
}
pub struct BackgroundRequest<'a> {
    /// Provider call whose result will contain this job's identifier.
    pub parent_call_id: &'a str,
    pub tool: &'a str,
    pub arguments: serde_json::Value,
    pub context: &'a ToolContext,
    pub summary: &'a str,
    pub interactive: bool,
}
/// Stable parent-to-child association without embedding arbitrary provider IDs.
pub(crate) fn child_prefix(parent_call_id: &str) -> String {
    let digest: String = Sha256::digest(parent_call_id.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("background-{digest}-")
}
struct Job {
    cancel: JobCancel,
    mutating: bool,
    status: JobStatus,
}
struct State {
    closed: bool,
    fatal: Option<BoundaryError>,
    jobs: HashMap<String, Job>,
    events: Vec<BackgroundEvent>,
}
impl State {
    fn event(&mut self, job_id: &str, kind: BackgroundEventKind) {
        let event = match &kind {
            BackgroundEventKind::Accepted { .. } => "background.accepted",
            BackgroundEventKind::CancellationRequested { .. } => {
                "background.cancellation_requested"
            }
            BackgroundEventKind::Finished { .. } => "background.finished",
        };
        tracing::info!(target: "saya::background", event, job_id);
        self.events.push(BackgroundEvent {
            sequence: self.events.len() as u64 + 1,
            job_id: job_id.into(),
            kind,
        });
    }
    fn request_cancellation(&mut self, id: &str, reason: BackgroundCancelReason) {
        if let Some(job) = self.jobs.get(id)
            && matches!(job.status, JobStatus::Running)
            && !job.cancel.is_cancelled()
        {
            job.cancel.cancel();
            self.event(id, BackgroundEventKind::CancellationRequested { reason });
        }
    }
}
struct Shared {
    state: Mutex<State>,
    changed: Notify,
}
struct ModeLease(crate::mode::ModeController);
impl Drop for ModeLease {
    fn drop(&mut self) {
        let _ = self.0.finish();
    }
}
/// Owns background calls for one dispatcher/run. Limits count all retained jobs
/// and concurrent running jobs. The caller must call `shutdown` before run end.
pub struct BackgroundJobs {
    dispatcher: Arc<ToolDispatcher>,
    shared: Arc<Shared>,
    max_running: usize,
    max_jobs: usize,
    tool_timeout: Duration,
}
impl BackgroundJobs {
    pub fn with_defaults(dispatcher: Arc<ToolDispatcher>) -> Result<Self, BoundaryError> {
        Self::new(
            dispatcher,
            DEFAULT_MAX_BACKGROUND_RUNNING,
            DEFAULT_MAX_BACKGROUND_JOBS,
        )
    }
    pub fn new(
        dispatcher: Arc<ToolDispatcher>,
        max_running: usize,
        max_jobs: usize,
    ) -> Result<Self, BoundaryError> {
        if max_running == 0 || max_jobs == 0 {
            return Err(failure(
                "invalid_config",
                "background limits must be positive",
            ));
        }
        Ok(Self {
            dispatcher,
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    closed: false,
                    fatal: None,
                    jobs: HashMap::new(),
                    events: Vec::new(),
                }),
                changed: Notify::new(),
            }),
            max_running,
            max_jobs,
            tool_timeout: Duration::from_secs(120),
        })
    }
    /// Set the non-exec deadline for this supervisor. Zero is rejected.
    /// Reaching the deadline cancels the tool and waits for its cleanup.
    pub fn with_tool_timeout(mut self, timeout: Duration) -> Result<Self, BoundaryError> {
        if timeout.is_zero() {
            return Err(failure(
                "invalid_config",
                "background tool timeout must be positive",
            ));
        }
        self.tool_timeout = timeout;
        Ok(self)
    }
    /// Authorize an explicitly requested background operation. Its internal
    /// call ID is distinct from the model-facing launch call ID.
    pub async fn submit(
        &self,
        request: BackgroundRequest<'_>,
        review: DispatchReview<'_>,
    ) -> Result<JobSubmission, BoundaryError> {
        let call = ToolCall {
            call_id: format!(
                "{}{}",
                child_prefix(request.parent_call_id),
                uuid::Uuid::now_v7()
            ),
            name: request.tool.into(),
            arguments: request.arguments.to_string(),
        };
        let auth = self
            .dispatcher
            .authorize(
                &call,
                request.context,
                request.summary,
                request.interactive,
                review,
            )
            .await?;
        let context = request.context.clone();
        Ok(match auth {
            Authorization::Permit(permit) => JobSubmission::Started {
                job_id: self.start(call, context, permit)?,
            },
            Authorization::Pending(request) => JobSubmission::Pending(PendingJob {
                request,
                call,
                context,
            }),
            Authorization::NeedsConfirmation(request) => {
                JobSubmission::NeedsConfirmation(PendingJob {
                    request,
                    call,
                    context,
                })
            }
            Authorization::Denied(reason) => JobSubmission::Denied(reason),
            Authorization::Stale => JobSubmission::Stale,
        })
    }
    /// Resolve one pending request; an approval is consumed for this operation only.
    pub fn resolve(
        &self,
        pending: PendingJob,
        approve: bool,
    ) -> Result<JobSubmission, BoundaryError> {
        Ok(
            match self
                .dispatcher
                .resolve(&pending.request.approval_id, approve)?
            {
                Authorization::Permit(permit) => JobSubmission::Started {
                    job_id: self.start(pending.call, pending.context, permit)?,
                },
                Authorization::Denied(reason) => JobSubmission::Denied(reason),
                _ => JobSubmission::Stale,
            },
        )
    }
    /// Start a previously authorized operation and immediately return a job ID.
    /// A pending confirmation is never treated as a permit. Execution and its
    /// intent/result are handled by the same dispatcher as foreground calls.
    pub fn start(
        &self,
        call: ToolCall,
        context: ToolContext,
        permit: Arc<OneShotPermit>,
    ) -> Result<String, BoundaryError> {
        if !self.dispatcher.owns_permit(&permit) {
            return Err(failure(
                "permission_denied",
                "permit belongs to another dispatcher",
            ));
        }
        if !matches!(
            call.name.as_str(),
            "read_file"
                | "list_dir"
                | "find_files"
                | "search_text"
                | "write_file"
                | "apply_patch"
                | "exec"
        ) {
            return Err(failure("unknown_tool", "unsupported background tool"));
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| failure("runtime_error", "Tokio runtime required"))?;
        let cancel = JobCancel::new();
        let id = if call.call_id.starts_with("background-") {
            call.call_id.clone()
        } else {
            uuid::Uuid::now_v7().to_string()
        };
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| failure("background_error", "job lock poisoned"))?;
        if state.closed || self.dispatcher.root_cancellation().is_cancelled() {
            return Err(failure("cancelled", "run is stopping"));
        }
        if state.jobs.len() >= self.max_jobs
            || state
                .jobs
                .values()
                .filter(|job| matches!(job.status, JobStatus::Running))
                .count()
                >= self.max_running
        {
            return Err(failure("limit_reached", "background job limit reached"));
        }
        if state.jobs.contains_key(&id) {
            return Err(failure("invalid_args", "duplicate background job ID"));
        }
        let mode = self.dispatcher.mode_controller().snapshot()?;
        if mode.pending.is_some() || mode.current != context.run.work_mode {
            return Err(failure("stale_approval", "work mode changed"));
        }
        let modes = self.dispatcher.mode_controller();
        if !modes.begin_shared(mode.revision)? {
            return Err(failure(
                "stale_approval",
                "work mode changed before scheduling",
            ));
        }
        let lease = ModeLease(modes);
        let mutating = matches!(call.name.as_str(), "write_file" | "apply_patch" | "exec");
        state.jobs.insert(
            id.clone(),
            Job {
                cancel: cancel.clone(),
                mutating,
                status: JobStatus::Running,
            },
        );
        state.event(
            &id,
            BackgroundEventKind::Accepted {
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
            },
        );
        let shared = self.shared.clone();
        let dispatcher = self.dispatcher.clone();
        let worker_state = shared.clone();
        let worker_id = id.clone();
        let tool_timeout = self.tool_timeout;
        let job_id = id.clone();
        let span = tracing::info_span!(target: "saya::background", "background", session_id = %context.run.session_id.0, run_id = %context.run.id.0, call_id = call.call_id.as_str(), job_id = job_id.as_str());
        let worker_span = span.clone();
        let task = runtime.spawn(
            async move {
                let _lease = lease;
                let signal = Combined {
                    root: dispatcher.root_cancellation(),
                    job: cancel,
                };
                let operation =
                    dispatcher.execute_with_cancel(&call, &context, permit, &signal, true);
                tokio::pin!(operation);
                tokio::select! {
                    biased;
                    result = &mut operation => return result,
                    () = tokio::time::sleep(tool_timeout), if call.name != "exec" => {}
                }
                if signal.is_cancelled() {
                    return operation.await;
                }
                let cancellation = match worker_state.state.lock() {
                    Ok(mut state) => {
                        if signal.is_cancelled() {
                            Ok(false)
                        } else {
                            state.request_cancellation(&worker_id, BackgroundCancelReason::Timeout);
                            Ok(true)
                        }
                    }
                    Err(_) => {
                        signal.job.cancel();
                        Err(failure("background_error", "job lock poisoned"))
                    }
                };
                // Keep polling the same future. In particular, a write which
                // crossed its commit point must finish and report what happened.
                let observed = operation.await;
                if !cancellation? {
                    return observed;
                }
                match observed {
                    Err(error) if error.0.code != "cancelled" => Err(error),
                    observed => {
                        let mut error = failure("tool_timeout", "background tool timed out");
                        error.2 = observed.ok().map(Box::new);
                        Err(error)
                    }
                }
            }
            .instrument(worker_span)
            .with_current_subscriber(),
        );
        runtime.spawn(
            async move {
                let outcome = task.await.unwrap_or_else(|_| {
                    Err(failure(
                        "background_error",
                        "background task failed unexpectedly",
                    ))
                });
                if let Ok(mut state) = shared.state.lock() {
                    if let Err(error) = &outcome
                        && !matches!(error.0.code.as_str(), "cancelled" | "tool_timeout")
                    {
                        state.closed = true;
                        state.fatal = Some(error.clone());
                        let ids: Vec<_> = state
                            .jobs
                            .keys()
                            .filter(|id| *id != &job_id)
                            .cloned()
                            .collect();
                        for id in ids {
                            state.request_cancellation(&id, BackgroundCancelReason::Failure);
                        }
                    }
                    if let Some(job) = state.jobs.get_mut(&job_id) {
                        let (status, outcome) = match outcome {
                            Ok(value) => {
                                let outcome = if value.ok {
                                    BackgroundOutcome::Succeeded
                                } else if value
                                    .error
                                    .as_ref()
                                    .is_some_and(|e| e.code == "cancelled")
                                {
                                    BackgroundOutcome::Cancelled
                                } else {
                                    BackgroundOutcome::Failed {
                                        code: value.error.as_ref().map_or_else(
                                            || "tool_error".into(),
                                            |e| e.code.clone(),
                                        ),
                                    }
                                };
                                (JobStatus::Finished(value), outcome)
                            }
                            Err(error) => {
                                let outcome = if error.0.code == "cancelled" {
                                    BackgroundOutcome::Cancelled
                                } else {
                                    BackgroundOutcome::Failed {
                                        code: error.0.code.clone(),
                                    }
                                };
                                (JobStatus::Failed(error), outcome)
                            }
                        };
                        job.status = status;
                        state.event(&job_id, BackgroundEventKind::Finished { outcome });
                    }
                }
                shared.changed.notify_waiters();
            }
            .instrument(span)
            .with_current_subscriber(),
        );
        Ok(id)
    }
    pub fn status(&self, id: &str) -> Result<Option<JobStatus>, BoundaryError> {
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| failure("background_error", "job lock poisoned"))?;
        Ok(state.jobs.get(id).map(|job| job.status.clone()))
    }
    /// Return a consistent snapshot of run-local transitions in observed order.
    pub fn events(&self) -> Result<Vec<BackgroundEvent>, BoundaryError> {
        Ok(self
            .shared
            .state
            .lock()
            .map_err(|_| failure("background_error", "job lock poisoned"))?
            .events
            .clone())
    }
    pub(crate) fn running_count(&self) -> Result<usize, BoundaryError> {
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| failure("background_error", "job lock poisoned"))?;
        Ok(state
            .jobs
            .values()
            .filter(|job| matches!(job.status, JobStatus::Running))
            .count())
    }
    /// Wait for a job to finish; returns `None` for an unknown job ID.
    pub async fn wait(&self, id: &str) -> Result<Option<JobStatus>, BoundaryError> {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let status = self.status(id)?;
            if !matches!(status, Some(JobStatus::Running)) {
                return Ok(status);
            }
            notified.await;
        }
    }
    /// Returns the failure that stopped further background dispatch, if any.
    pub fn fatal_error(&self) -> Result<Option<ErrorInfo>, BoundaryError> {
        Ok(self
            .shared
            .state
            .lock()
            .map_err(|_| failure("background_error", "job lock poisoned"))?
            .fatal
            .as_ref()
            .map(|error| error.0.clone()))
    }
    pub(crate) fn fatal_failure(&self) -> Result<Option<BoundaryError>, BoundaryError> {
        Ok(self
            .shared
            .state
            .lock()
            .map_err(|_| failure("background_error", "job lock poisoned"))?
            .fatal
            .clone())
    }
    pub fn cancel(&self, id: &str) -> Result<bool, BoundaryError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| failure("background_error", "job lock poisoned"))?;
        if !state.jobs.contains_key(id) {
            return Ok(false);
        }
        state.request_cancellation(id, BackgroundCancelReason::User);
        Ok(true)
    }
    /// Queue a mode request. PLAN cancels active mutating jobs; the mode
    /// controller applies the change only when in-flight calls have drained.
    pub fn request_mode(&self, mode: WorkMode) -> Result<(), BoundaryError> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| failure("background_error", "job lock poisoned"))?;
        if state.closed {
            return Err(failure("invalid_run", "run is stopping or has ended"));
        }
        self.dispatcher.mode_controller().request_after(mode, || {
            if mode == WorkMode::Plan {
                let ids: Vec<_> = state
                    .jobs
                    .iter()
                    .filter(|(_, job)| job.mutating)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    state.request_cancellation(&id, BackgroundCancelReason::Plan);
                }
            }
        })
    }
    /// Prevent new work, cancel all unfinished jobs and await their cleanup.
    pub async fn shutdown(&self) -> Result<(), BoundaryError> {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| failure("background_error", "job lock poisoned"))?;
            state.closed = true;
            let ids: Vec<_> = state.jobs.keys().cloned().collect();
            for id in ids {
                state.request_cancellation(&id, BackgroundCancelReason::Shutdown);
            }
        }
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self
                    .shared
                    .state
                    .lock()
                    .map_err(|_| failure("background_error", "job lock poisoned"))?;
                if state
                    .jobs
                    .values()
                    .all(|job| !matches!(job.status, JobStatus::Running))
                {
                    return Ok(());
                }
            }
            notified.await;
        }
    }
}
impl Drop for BackgroundJobs {
    fn drop(&mut self) {
        if let Ok(mut state) = self.shared.state.lock() {
            state.closed = true;
            for job in state.jobs.values() {
                job.cancel.cancel();
            }
        }
    }
}

/// Shared bridge for model-facing start/status/cancel tools. The enclosing
/// runtime must count each call toward its tool budget, handle confirmation
/// requests, and call `BackgroundJobs::shutdown` before finishing a run.
pub struct BackgroundTools {
    jobs: Arc<BackgroundJobs>,
    model: Arc<dyn crate::boundary::Model>,
    budget: crate::review::SharedBudget,
    review: crate::review::ReviewConfig,
    summary: String,
    interactive: bool,
    pending: Mutex<HashMap<String, PendingBackground>>,
}
enum PendingBackground {
    Job(Box<PendingJob>),
    Control {
        call: ToolCall,
        context: ToolContext,
        kind: BackgroundToolKind,
    },
}
impl BackgroundTools {
    pub fn new(
        jobs: Arc<BackgroundJobs>,
        model: Arc<dyn crate::boundary::Model>,
        budget: crate::review::SharedBudget,
        review: crate::review::ReviewConfig,
        summary: String,
        interactive: bool,
    ) -> Self {
        Self {
            jobs,
            model,
            budget,
            review,
            summary,
            interactive,
            pending: Mutex::new(HashMap::new()),
        }
    }
    /// Resolve a human confirmation for one background tool invocation.
    pub fn resolve(&self, approval_id: &str, approve: bool) -> Result<ToolResult, BoundaryError> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| failure("background_error", "approval lock poisoned"))?
            .remove(approval_id);
        match pending {
            Some(PendingBackground::Job(pending)) => {
                Ok(match self.jobs.resolve(*pending, approve)? {
                    JobSubmission::Started { job_id } => {
                        tool_result(serde_json::json!({"status":"started","job_id":job_id}))
                    }
                    JobSubmission::Denied(_) => {
                        tool_error("permission_denied", "user denied background operation")
                    }
                    _ => tool_error("stale_approval", "background authorization changed"),
                })
            }
            Some(PendingBackground::Control {
                call,
                context,
                kind,
            }) => Ok(match self.jobs.dispatcher.resolve(approval_id, approve)? {
                Authorization::Permit(permit) => {
                    self.execute_control(kind, &call, &context, &permit)?
                }
                Authorization::Denied(_) => {
                    tool_error("permission_denied", "user denied background operation")
                }
                _ => tool_error("stale_approval", "background authorization changed"),
            }),
            None => Ok(tool_error(
                "stale_approval",
                "background authorization changed",
            )),
        }
    }
    fn execute_control(
        &self,
        kind: BackgroundToolKind,
        call: &ToolCall,
        context: &ToolContext,
        permit: &Arc<OneShotPermit>,
    ) -> Result<ToolResult, BoundaryError> {
        if !self.jobs.dispatcher.owns_permit(permit) {
            return Err(failure(
                "permission_denied",
                "permit belongs to another dispatcher",
            ));
        }
        let args: JobArgs = serde_json::from_str(&call.arguments)
            .map_err(|_| failure("invalid_args", "invalid background job ID"))?;
        permit.with_background_control(call, context, || {
            if matches!(kind, BackgroundToolKind::Cancel) {
                return Ok(tool_result(serde_json::json!({"found":self.jobs.cancel(&args.job_id)?})));
            }
            Ok(match self.jobs.status(&args.job_id)? {
                None => tool_result(serde_json::json!({"status":"not_found"})),
                Some(JobStatus::Running) => tool_result(serde_json::json!({"status":"running"})),
                Some(JobStatus::Finished(result)) => tool_result(serde_json::json!({"status":"finished","result":result})),
                Some(JobStatus::Failed(error)) => tool_result(serde_json::json!({"status":"failed","error":error.0,"observed_result":error.2})),
            })
        })
    }
    pub fn tool(self: &Arc<Self>, kind: BackgroundToolKind) -> BackgroundTool {
        BackgroundTool {
            shared: self.clone(),
            kind,
        }
    }
}
#[derive(Clone, Copy)]
pub enum BackgroundToolKind {
    Start,
    Status,
    Cancel,
}
pub struct BackgroundTool {
    shared: Arc<BackgroundTools>,
    kind: BackgroundToolKind,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StartArgs {
    tool: String,
    arguments: serde_json::Value,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct JobArgs {
    job_id: String,
}
fn tool_result(data: serde_json::Value) -> ToolResult {
    ToolResult {
        ok: true,
        data: Some(data),
        error: None,
        truncated: false,
    }
}
fn tool_error(code: &str, message: &str) -> ToolResult {
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
impl crate::boundary::Tool for BackgroundTool {
    fn definition(&self) -> crate::boundary::ToolDefinition {
        let (name, description, properties, required) = match self.kind {
            BackgroundToolKind::Start => (
                "background_start",
                "Start one authorized local tool in the current run; return a job ID without waiting for completion.",
                serde_json::json!({"tool":{"type":"string","enum":["read_file","list_dir","find_files","search_text","write_file","apply_patch","exec"]},"arguments":{"type":"object"}}),
                serde_json::json!(["tool", "arguments"]),
            ),
            BackgroundToolKind::Status => (
                "background_status",
                "Get a background job's status and any completed result without waiting.",
                serde_json::json!({"job_id":{"type":"string"}}),
                serde_json::json!(["job_id"]),
            ),
            BackgroundToolKind::Cancel => (
                "background_cancel",
                "Request cancellation of a job in the current run.",
                serde_json::json!({"job_id":{"type":"string"}}),
                serde_json::json!(["job_id"]),
            ),
        };
        crate::boundary::ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: serde_json::json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }
    }
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> crate::boundary::Operation<'a, ToolResult> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(failure("cancelled", "background control cancelled"));
            }
            if !self.shared.jobs.dispatcher.owns_context(context) {
                return Err(failure(
                    "invalid_run",
                    "background tools belong to another run",
                ));
            }
            if call.name != self.definition().name {
                return Ok(tool_error(
                    "invalid_args",
                    "tool name does not match adapter",
                ));
            }
            match self.kind {
                BackgroundToolKind::Start => {
                    let args: StartArgs = match serde_json::from_str(&call.arguments) {
                        Ok(args) => args,
                        Err(_) => {
                            return Ok(tool_error("invalid_args", "invalid background request"));
                        }
                    };
                    if !args.arguments.is_object() {
                        return Ok(tool_error("invalid_args", "arguments must be an object"));
                    }
                    let summary = format!("Background execution: {}", self.shared.summary);
                    let result = self
                        .shared
                        .jobs
                        .submit(
                            BackgroundRequest {
                                parent_call_id: &call.call_id,
                                tool: &args.tool,
                                arguments: args.arguments,
                                context,
                                summary: &summary,
                                interactive: self.shared.interactive,
                            },
                            DispatchReview {
                                model: self.shared.model.as_ref(),
                                budget: &self.shared.budget,
                                config: &self.shared.review,
                            },
                        )
                        .await?;
                    Ok(match result {
                        JobSubmission::Started { job_id } => {
                            if cancel.is_cancelled() {
                                self.shared.jobs.cancel(&job_id)?;
                                return Err(failure("cancelled", "background control cancelled"));
                            }
                            tool_result(serde_json::json!({"status":"started","job_id":job_id}))
                        }
                        JobSubmission::Pending(pending)
                        | JobSubmission::NeedsConfirmation(pending) => {
                            let request = pending.request.clone();
                            self.shared
                                .pending
                                .lock()
                                .map_err(|_| failure("background_error", "approval lock poisoned"))?
                                .insert(
                                    request.approval_id.clone(),
                                    PendingBackground::Job(Box::new(pending)),
                                );
                            ToolResult {
                                ok: false,
                                data: Some(
                                    serde_json::json!({"status":"needs_confirmation","request":request}),
                                ),
                                error: Some(ErrorInfo {
                                    code: "needs_confirmation".into(),
                                    message: "background operation requires confirmation".into(),
                                }),
                                truncated: false,
                            }
                        }
                        JobSubmission::Denied(reason) => tool_error("permission_denied", reason),
                        JobSubmission::Stale => {
                            tool_error("stale_approval", "background authorization is stale")
                        }
                    })
                }
                BackgroundToolKind::Status | BackgroundToolKind::Cancel => {
                    match serde_json::from_str::<JobArgs>(&call.arguments) {
                        Ok(_) => {}
                        Err(_) => {
                            return Ok(tool_error("invalid_args", "invalid background job ID"));
                        }
                    }
                    let auth = self
                        .shared
                        .jobs
                        .dispatcher
                        .authorize(
                            call,
                            context,
                            &self.shared.summary,
                            self.shared.interactive,
                            DispatchReview {
                                model: self.shared.model.as_ref(),
                                budget: &self.shared.budget,
                                config: &self.shared.review,
                            },
                        )
                        .await?;
                    Ok(match auth {
                        Authorization::Permit(permit) => self
                            .shared
                            .execute_control(self.kind, call, context, &permit)?,
                        Authorization::Pending(request)
                        | Authorization::NeedsConfirmation(request) => {
                            self.shared
                                .pending
                                .lock()
                                .map_err(|_| failure("background_error", "approval lock poisoned"))?
                                .insert(
                                    request.approval_id.clone(),
                                    PendingBackground::Control {
                                        call: call.clone(),
                                        context: context.clone(),
                                        kind: self.kind,
                                    },
                                );
                            ToolResult {
                                ok: false,
                                data: Some(
                                    serde_json::json!({"status":"needs_confirmation","request":request}),
                                ),
                                error: Some(ErrorInfo {
                                    code: "needs_confirmation".into(),
                                    message: "background control requires confirmation".into(),
                                }),
                                truncated: false,
                            }
                        }
                        Authorization::Denied(reason) => tool_error("permission_denied", reason),
                        Authorization::Stale => {
                            tool_error("stale_approval", "background authorization is stale")
                        }
                    })
                }
            }
        })
    }
}
