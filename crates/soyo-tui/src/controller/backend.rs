//! Helper-backed remote adapters mirroring `Prepared` and `RuntimeHandle`.
//!
//! The controller keeps a single code path: `SessionBackend` is either the
//! in-process `session::Prepared` (explicit `--config`/`--data-dir`, or the
//! helper is unreachable) or `RemotePrepared`, a mirror of the helper-owned
//! session. `ActiveHandle` mirrors the same split for runs. Remote state that
//! the runtime exposes synchronously (mode snapshot, compaction flag) is
//! cached here and refreshed from stream events and our own control calls.
use super::turn_summary::SummarySink;
use serde_json::{Value, json};
use soyo_app::{
    context::{ContextEntry, ContextSnapshot},
    events::EventSink,
    interaction::Request,
    session::Prepared,
    thinking::{ThinkingLog, ThinkingSnapshot},
};
use soyo_core::{
    boundary::{Cancellation, Store},
    contract::{
        ApprovalRequest, BoundaryError, ErrorInfo, Event, EventPayload, RunResult, Session, Usage,
        WorkMode,
    },
    input::InputRequest,
    metrics::RunMetrics,
    mode::ModeSnapshot,
    runtime::RuntimeHandle,
    sqlite::{RunMetricsRecord, SessionSnapshot},
};
use soyo_ipc::{
    client::HelperClient,
    types::{
        ApprovalRequestDto, BackgroundJobDto, DesktopError, DesktopStreamEnvelope,
        PersonalContextDto, SubmitPromptParams, SubmitPromptResultDto, WorkModeDto,
    },
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
/// Handle methods mirror `RuntimeHandle`'s `BoundaryError` contract.
type BResult<T> = std::result::Result<T, BoundaryError>;

fn boundary(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn helper_error(err: &DesktopError) -> BoundaryError {
    boundary(&err.code, err.message.clone())
}

/// Mirror of the helper-owned prepared session. Session mutation methods
/// replay the local store operations as IPC calls and then update the mirror.
pub struct RemotePrepared {
    pub(super) client: Arc<HelperClient>,
    pub session: Session,
}

impl RemotePrepared {
    pub async fn load(client: Arc<HelperClient>, session_id: &str) -> Result<Self> {
        let value = client
            .call("get_session", json!({"session_id": session_id}))
            .await
            .map_err(|err| -> Box<dyn std::error::Error> { Box::new(helper_error(&err)) })?;
        if value.is_null() {
            return Err(boundary("session_missing", "session not found in helper").into());
        }
        let session: Session =
            serde_json::from_value(value).map_err(|_| -> Box<dyn std::error::Error> {
                Box::new(boundary("helper_protocol", "invalid session payload"))
            })?;
        Ok(Self { client, session })
    }
}

pub enum SessionBackend {
    Local(Prepared),
    Remote(RemotePrepared),
}

impl SessionBackend {
    pub fn session(&self) -> &Session {
        match self {
            Self::Local(prepared) => &prepared.session,
            Self::Remote(remote) => &remote.session,
        }
    }

    pub fn set_session(&mut self, session: Session) {
        match self {
            Self::Local(prepared) => prepared.session = session,
            Self::Remote(remote) => remote.session = session,
        }
    }

    /// In-process backend access — `None` when running through the helper.
    #[cfg(test)]
    pub fn local(&self) -> Option<&Prepared> {
        match self {
            Self::Local(prepared) => Some(prepared),
            Self::Remote(_) => None,
        }
    }

    #[cfg(test)]
    pub fn local_mut(&mut self) -> Option<&mut Prepared> {
        match self {
            Self::Local(prepared) => Some(prepared),
            Self::Remote(_) => None,
        }
    }

    /// Persist a mutated `Session` record. Remote callers have already applied
    /// the equivalent change through a dedicated IPC (for example
    /// `set_work_mode`), so this only updates the mirror.
    pub async fn commit_session(&mut self, updated: Session) -> Result<()> {
        match self {
            Self::Local(prepared) => {
                prepared
                    .store
                    .commit(&[soyo_core::boundary::StoreChange::Session(updated.clone())])
                    .await?;
                prepared.session = updated;
            }
            Self::Remote(remote) => remote.session = updated,
        }
        Ok(())
    }

    /// Persist a changed profile fingerprint onto the session record.
    /// Remote mode performs the matching commit inside `set_workspace` and
    /// `set_thinking_effort` on the helper, so this is a no-op there.
    pub async fn update_config_fingerprint(&self, fingerprint: &str) -> Result<()> {
        match self {
            Self::Local(prepared) => {
                prepared
                    .store
                    .update_session_config(&prepared.session, fingerprint)
                    .await?;
            }
            Self::Remote(_) => {}
        }
        Ok(())
    }

    /// Change the session workspace durably.
    pub async fn set_workspace(&mut self, workspace: &Path, fingerprint: &str) -> Result<()> {
        match self {
            Self::Local(prepared) => {
                let mut updated = prepared.session.clone();
                updated.workspace = workspace.to_path_buf();
                prepared
                    .store
                    .update_session_config(&updated, fingerprint)
                    .await?;
                prepared.session = updated;
            }
            Self::Remote(remote) => {
                remote
                    .client
                    .call(
                        "set_workspace",
                        json!({"path": workspace.to_string_lossy()}),
                    )
                    .await
                    .map_err(|err| helper_error(&err))?;
                remote.session.workspace = workspace.to_path_buf();
            }
        }
        Ok(())
    }

    /// Last recorded execution usage for the session, used by `/new` and
    /// `/resume` to show the previous run's token totals.
    pub async fn last_execution_usage(&self) -> Result<Option<Usage>> {
        match self {
            Self::Local(prepared) => Ok(prepared
                .store
                .run_metrics(prepared.session.id)
                .await?
                .into_iter()
                .rev()
                .find_map(|record: RunMetricsRecord| record.metrics.execution_usage)),
            Self::Remote(remote) => {
                let snapshot = remote
                    .client
                    .call(
                        "read_session_snapshot",
                        json!({"session_id": remote.session.id.0.to_string()}),
                    )
                    .await
                    .map_err(|err| helper_error(&err))?;
                let snapshot: SessionSnapshot = serde_json::from_value(snapshot)
                    .map_err(|_| boundary("helper_protocol", "invalid snapshot payload"))?;
                // Snapshot rows wrap metrics as {"run_id", "seq", "data"}.
                Ok(snapshot
                    .metrics
                    .into_iter()
                    .rev()
                    .filter_map(|mut value| {
                        serde_json::from_value::<RunMetrics>(value["data"].take()).ok()
                    })
                    .find_map(|metrics| metrics.execution_usage))
            }
        }
    }
}

/// `read_session_snapshot` IPC shared by `SessionBackend::read_snapshot` and
/// service tasks that only hold the client plus session id.
pub async fn remote_session_snapshot(
    client: &HelperClient,
    session_id: &str,
    cancel: Arc<dyn Cancellation>,
) -> Result<SessionSnapshot> {
    let value = client
        .call("read_session_snapshot", json!({"session_id": session_id}))
        .await
        .map_err(|err| helper_error(&err))?;
    if cancel.is_cancelled() {
        return Err(boundary("cancelled", "snapshot read cancelled").into());
    }
    serde_json::from_value(value)
        .map_err(|_| boundary("helper_protocol", "invalid snapshot payload").into())
}

/// Mode/compaction state cached for the remote handle. Updated from raw
/// `mode.*` stream events, our own control calls, and the terminal
/// `RunCompleted` envelope.
struct RemoteRunState {
    mode: ModeSnapshot,
    compaction_pending: bool,
}

/// Remote equivalent of `RuntimeHandle`. Reads that the runtime exposes
/// synchronously are served from `RemoteRunState`; mutations go over IPC.
#[derive(Clone)]
pub struct RemoteHandle {
    client: Arc<HelperClient>,
    session_id: String,
    state: Arc<Mutex<RemoteRunState>>,
}

impl RemoteHandle {
    fn update_mode(&self, current: WorkMode, pending: Option<WorkMode>) {
        if let Ok(mut state) = self.state.lock() {
            state.mode.current = current;
            state.mode.pending = pending;
            state.mode.revision = state.mode.revision.saturating_add(1);
        }
    }

    fn set_compaction_pending(&self, pending: bool) {
        if let Ok(mut state) = self.state.lock() {
            state.compaction_pending = pending;
        }
    }

    /// Remote equivalent of `RuntimeHandle::mode`: cached snapshot.
    pub fn mode(&self) -> BResult<ModeSnapshot> {
        self.state
            .lock()
            .map(|state| state.mode)
            .map_err(|_| boundary("state_lock", "remote mode lock failed"))
    }

    /// `RuntimeHandle::request_mode` → `set_work_mode`. The helper also
    /// invalidates stale approvals server-side.
    pub async fn request_mode(&self, target: WorkMode) -> BResult<()> {
        let dto: soyo_ipc::types::ModeStateDto = self
            .client
            .call_typed("set_work_mode", &json!({"mode": WorkModeDto::from(target)}))
            .await
            .map_err(|err| helper_error(&err))?;
        self.update_mode(dto.current.into(), dto.pending.map(Into::into));
        Ok(())
    }

    /// `RuntimeHandle::compaction_pending`: locally cached flag.
    pub fn compaction_pending(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.compaction_pending)
            .unwrap_or(false)
    }

    /// `RuntimeHandle::request_compaction` → `compact_session`. Mid-run the
    /// helper only queues the request and answers immediately.
    pub async fn request_compaction(&self) -> BResult<()> {
        let (events, mut rx) = mpsc::unbounded_channel();
        let call = tokio::spawn({
            let client = self.client.clone();
            async move {
                client
                    .call_streaming::<_, SubmitPromptResultDto>(
                        "compact_session",
                        &json!({"raw_events": true}),
                        events,
                    )
                    .await
            }
        });
        while rx.recv().await.is_some() {}
        let result = call
            .await
            .map_err(|_| boundary("run_task_failed", "compaction request task failed"))?
            .map_err(|err| helper_error(&err))?;
        if result.action == "compaction_queued" {
            self.set_compaction_pending(true);
        }
        Ok(())
    }

    /// `RuntimeHandle::steer` → `submit_prompt` while a run is active. The
    /// helper answers `{"action":"steered"}` without starting a new run.
    pub async fn steer(&self, text: String) -> BResult<()> {
        let params = SubmitPromptParams {
            prompt: text,
            session_id: Some(self.session_id.clone()),
            ..Default::default()
        };
        let result: SubmitPromptResultDto = self
            .client
            .call_typed("submit_prompt", &params)
            .await
            .map_err(|err| helper_error(&err))?;
        if result.action != "steered" {
            return Err(boundary(
                "helper_protocol",
                format!("unexpected submit_prompt action '{}'", result.action),
            ));
        }
        Ok(())
    }

    /// `RuntimeHandle::background_events` → `list_jobs` job-state rows.
    /// Lifecycle events are not published on the wire, so remote runs report
    /// the helper's current job table instead.
    pub async fn background_jobs(&self) -> BResult<Vec<BackgroundJobDto>> {
        self.client
            .call_typed("list_jobs", &json!({}))
            .await
            .map_err(|err| helper_error(&err))
    }

    /// `RuntimeHandle::cancel_background` → `cancel_job`.
    pub async fn cancel_background(&self, id: &str) -> BResult<bool> {
        self.client
            .call_typed("cancel_job", &json!({"job_id": id}))
            .await
            .map_err(|err| helper_error(&err))
    }

    /// Remote runs live inside the helper; nothing local to shut down.
    pub async fn shutdown_background(&self) -> BResult<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub enum ActiveHandle {
    Local(RuntimeHandle),
    Remote(RemoteHandle),
}

impl ActiveHandle {
    pub fn mode(&self) -> BResult<ModeSnapshot> {
        match self {
            Self::Local(handle) => handle.mode(),
            Self::Remote(handle) => handle.mode(),
        }
    }

    /// Signal cancellation: local runs observe `RunCancellation` directly; the
    /// helper gets a fire-and-forget `cancel_run` request.
    pub fn cancel_run(&self) {
        if let Self::Remote(handle) = self {
            let client = handle.client.clone();
            tokio::spawn(async move {
                let _ = client.call("cancel_run", json!({})).await;
            });
        }
    }

    pub fn compaction_pending(&self) -> bool {
        match self {
            Self::Local(handle) => handle.compaction_pending(),
            Self::Remote(handle) => handle.compaction_pending(),
        }
    }

    pub async fn request_mode(&self, target: WorkMode) -> BResult<()> {
        match self {
            Self::Local(handle) => handle.request_mode(target),
            Self::Remote(handle) => handle.request_mode(target).await,
        }
    }

    pub async fn request_compaction(&self) -> BResult<()> {
        match self {
            Self::Local(handle) => handle.request_compaction(),
            Self::Remote(handle) => handle.request_compaction().await,
        }
    }

    pub async fn steer(&self, text: String) -> BResult<()> {
        match self {
            Self::Local(handle) => handle.steer(text),
            Self::Remote(handle) => handle.steer(text).await,
        }
    }

    pub async fn cancel_background(&self, id: &str) -> BResult<bool> {
        match self {
            Self::Local(handle) => handle.cancel_background(id),
            Self::Remote(handle) => handle.cancel_background(id).await,
        }
    }

    pub async fn shutdown_background(&self) -> BResult<()> {
        match self {
            Self::Local(handle) => handle.shutdown_background().await,
            Self::Remote(handle) => handle.shutdown_background().await,
        }
    }

    /// Lifecycle transitions are tracked inside the run's job registry — an
    /// in-process concept. Remote runs expose job state via `list_jobs`
    /// instead (`RemoteHandle::background_jobs`).
    #[cfg(test)]
    pub fn background_events(&self) -> BResult<Vec<soyo_core::background::BackgroundEvent>> {
        match self {
            Self::Local(handle) => handle.background_events(),
            Self::Remote(_) => Err(boundary(
                "unsupported_remote",
                "background lifecycle log is only available on the in-process backend",
            )),
        }
    }
}

/// Everything `Active` needs to drive a remote run end to end.
pub struct RemoteRun {
    pub context: watch::Receiver<Option<ContextSnapshot>>,
    pub thinking: watch::Receiver<ThinkingLog>,
    pub requests: mpsc::Receiver<Request>,
    pub handle: RemoteHandle,
    pub join: tokio::task::JoinHandle<(BResult<RunResult>, Session)>,
}

fn approval_from_dto(dto: &ApprovalRequestDto) -> Option<ApprovalRequest> {
    Some(ApprovalRequest {
        approval_id: dto.approval_id.clone(),
        call_id: dto.call_id.clone(),
        run_id: serde_json::from_value(Value::String(dto.run_id.clone())).ok()?,
        tool: dto.tool.clone(),
        operation_summary: dto.operation_summary.clone(),
        arguments: dto.arguments.clone(),
        cwd: PathBuf::from(&dto.cwd),
        work_mode: dto.work_mode.into(),
    })
}

/// Spawn the helper `submit_prompt` stream and translate its envelopes back
/// into the channel shapes `Active` already drives: schema `Event`s, context
/// and thinking watches, pending `Request`s, and the terminal
/// `(RunResult, Session)` outcome.
pub fn spawn_remote_run(
    client: Arc<HelperClient>,
    session_id: String,
    params: SubmitPromptParams,
    compact: bool,
    summary_sink: SummarySink,
    initial_mode: WorkMode,
    session_fallback: Session,
) -> RemoteRun {
    let (context_tx, context) = watch::channel(None);
    let (thinking_tx, thinking) = watch::channel(ThinkingLog::default());
    let (requests_tx, requests) = mpsc::channel(16);
    let (envelope_tx, mut envelope_rx) = mpsc::unbounded_channel();
    let (result_tx, result_rx) = oneshot::channel::<RunResult>();
    let state = Arc::new(Mutex::new(RemoteRunState {
        mode: ModeSnapshot {
            current: initial_mode,
            pending: None,
            revision: 0,
        },
        compaction_pending: false,
    }));
    let handle = RemoteHandle {
        client: client.clone(),
        session_id: session_id.clone(),
        state: state.clone(),
    };

    let call = {
        let client = client.clone();
        tokio::spawn(async move {
            if compact {
                client
                    .call_streaming::<_, Value>(
                        "compact_session",
                        &json!({"raw_events": true}),
                        envelope_tx,
                    )
                    .await
            } else {
                client
                    .call_streaming::<_, SubmitPromptResultDto>(
                        "submit_prompt",
                        &params,
                        envelope_tx,
                    )
                    .await
                    .map(|_| Value::Null)
            }
        })
    };

    let pump = {
        let client = client.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let mut result_tx = Some(result_tx);
            while let Some(envelope) = envelope_rx.recv().await {
                match envelope {
                    DesktopStreamEnvelope::RawSchemaEvent { event } => {
                        let Ok(event) = serde_json::from_value::<Event>(event) else {
                            continue;
                        };
                        if let Ok(mut state) = state.lock() {
                            match &event.payload {
                                EventPayload::ModeChangeRequested { to, .. } => {
                                    state.mode.pending = Some(*to);
                                    state.mode.revision = state.mode.revision.saturating_add(1);
                                }
                                EventPayload::ModeChanged { to, .. } => {
                                    state.mode.current = *to;
                                    state.mode.pending = None;
                                    state.mode.revision = state.mode.revision.saturating_add(1);
                                }
                                _ => {}
                            }
                        }
                        if let EventPayload::RunFinished(result) = &event.payload
                            && let Some(tx) = result_tx.take()
                        {
                            let _ = tx.send(result.clone());
                        }
                        let _ = summary_sink.publish(&event).await;
                    }
                    DesktopStreamEnvelope::Context {
                        entries,
                        skills_count,
                        ..
                    } => {
                        let snapshot = ContextSnapshot {
                            entries: entries
                                .into_iter()
                                .map(|entry| ContextEntry {
                                    label: Box::leak(entry.label.into_boxed_str()),
                                    tokens: entry.tokens,
                                })
                                .collect(),
                            skills_count: skills_count.map(|count| count as usize),
                        };
                        context_tx.send_replace(Some(snapshot));
                    }
                    DesktopStreamEnvelope::Thinking(entries) => {
                        let log = ThinkingLog {
                            entries: entries
                                .into_iter()
                                .map(|entry| ThinkingSnapshot {
                                    request_id: entry.request_id,
                                    text: entry.text,
                                    active: entry.active,
                                    truncated: entry.truncated,
                                    available: entry.available,
                                })
                                .collect(),
                        };
                        thinking_tx.send_replace(log);
                    }
                    DesktopStreamEnvelope::ApprovalNeeded(dto) => {
                        let Some(request) = approval_from_dto(&dto) else {
                            continue;
                        };
                        let (send, receive) = oneshot::channel();
                        if requests_tx
                            .send(Request::Approval(request, send))
                            .await
                            .is_err()
                        {
                            continue;
                        }
                        let client = client.clone();
                        tokio::spawn(async move {
                            if let Ok(Ok(Some(approved))) = receive.await {
                                let _ = client
                                    .call(
                                        "reply_approval",
                                        json!({
                                            "approval_id": dto.approval_id,
                                            "call_id": dto.call_id,
                                            "approved": approved,
                                        }),
                                    )
                                    .await;
                            }
                        });
                    }
                    DesktopStreamEnvelope::InputNeeded(dto) => {
                        let request = InputRequest {
                            question: dto.question.clone(),
                            options: dto.options.clone(),
                        };
                        let (send, receive) = oneshot::channel();
                        if requests_tx
                            .send(Request::Input(request, send))
                            .await
                            .is_err()
                        {
                            continue;
                        }
                        let client = client.clone();
                        tokio::spawn(async move {
                            if let Ok(Ok(answer)) = receive.await {
                                let _ = client
                                    .call(
                                        "reply_input",
                                        json!({
                                            "input_id": dto.input_id,
                                            "answer": answer,
                                        }),
                                    )
                                    .await;
                            }
                        });
                    }
                    DesktopStreamEnvelope::RunCompleted {
                        compaction_pending, ..
                    } => {
                        if let Some(pending) = compaction_pending
                            && let Ok(mut state) = state.lock()
                        {
                            state.compaction_pending = pending;
                        }
                    }
                    _ => {}
                }
            }
        })
    };

    let join = tokio::spawn(async move {
        let _ = pump.await;
        let outcome: BResult<RunResult> = match call.await {
            Ok(Ok(_)) => match result_rx.await {
                Ok(result) => Ok(result),
                Err(_) => Err(boundary(
                    "run_incomplete",
                    "run ended without a terminal event",
                )),
            },
            Ok(Err(err)) => Err(helper_error(&err)),
            Err(_) => Err(boundary(
                "run_task_failed",
                "remote run task terminated unexpectedly",
            )),
        };
        let session = client
            .call("get_session", json!({"session_id": session_id}))
            .await
            .ok()
            .and_then(|value| serde_json::from_value::<Session>(value).ok())
            .unwrap_or(session_fallback);
        (outcome, session)
    });

    RemoteRun {
        context,
        thinking,
        requests,
        handle,
        join,
    }
}

/// Personal context pass-through for `SubmitPromptParams`.
pub fn personal_context_dto(
    context: &soyo_core::runtime::PersonalContext,
) -> Option<PersonalContextDto> {
    if context.preferences.is_none() && context.memory.is_none() {
        None
    } else {
        Some(PersonalContextDto {
            preferences: context.preferences.clone(),
            memory: context.memory.clone(),
        })
    }
}
