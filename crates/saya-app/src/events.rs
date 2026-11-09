//! Ordered durable events with interface-owned delivery.
use saya_core::{
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, StoreChange,
    },
    contract::{
        BoundaryError, ErrorInfo, HistoryItem, ModelPurpose, RunId, SCHEMA_VERSION, Session,
        SessionId,
    },
};
use std::sync::Arc;
fn cli_error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn redact_value(value: &mut serde_json::Value, secrets: &[String], _: usize) {
    if saya_core::redaction::redact(value, secrets).is_err() {
        *value = serde_json::Value::String("sensitive details withheld".into());
    }
}
fn contains_encoded_secret(text: &str, secrets: &[String]) -> bool {
    saya_core::redaction::contains_secret(text, secrets)
}

/// Delivery is ordered and bounded by the caller's sink. Errors stop delivery.
pub trait EventSink: Send + Sync {
    fn publish<'a>(&'a self, event: &'a Event) -> Operation<'a, ()>;
}
/// A bounded event channel for interfaces. Closing its receiver fails delivery.
pub struct ChannelSink(pub tokio::sync::mpsc::Sender<Event>);
impl EventSink for ChannelSink {
    fn publish<'a>(&'a self, event: &'a Event) -> Operation<'a, ()> {
        Box::pin(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                self.0.send(event.clone()),
            )
            .await
            .map_err(|_| cli_error("event_consumer_stalled", "event delivery deadline exceeded"))?
            .map_err(|_| cli_error("event_consumer_closed", "event consumer disconnected"))
        })
    }
}
use saya_core::{
    contract::{Event, EventPayload, RunResult, Usage},
    sqlite::SqliteStore,
};
use std::{
    collections::HashMap,
    mem::MaybeUninit,
    time::{SystemTime, UNIX_EPOCH},
};

struct State {
    next: u64,
    end: u64,
    names: HashMap<String, String>,
    pending_execution: Option<EventPayload>,
    terminal_emitted: bool,
}

pub struct EventWriter {
    store: Arc<SqliteStore>,
    sink: Arc<dyn EventSink>,
    session: SessionId,
    run: RunId,
    secrets: Vec<String>,
    secret_registry: Option<Arc<saya_core::credentials::SecretRegistry>>,
    state: tokio::sync::Mutex<State>,
    interrupted: std::sync::Mutex<Vec<EventPayload>>,
}
impl EventWriter {
    pub fn new(
        store: Arc<SqliteStore>,
        session: SessionId,
        run: RunId,
        secrets: Vec<String>,
        sink: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            store,
            sink,
            session,
            run,
            secrets,
            secret_registry: None,
            interrupted: std::sync::Mutex::new(Vec::new()),
            state: tokio::sync::Mutex::new(State {
                next: 1,
                end: 0,
                names: HashMap::new(),
                pending_execution: None,
                terminal_emitted: false,
            }),
        }
    }
    /// Share refreshed credentials with the host-side display boundary.
    pub fn with_secret_registry(
        mut self,
        registry: Arc<saya_core::credentials::SecretRegistry>,
    ) -> Result<Self, BoundaryError> {
        registry.register(
            &self
                .secrets
                .iter()
                .filter(|value| !value.is_empty())
                .cloned()
                .collect::<Vec<_>>(),
        )?;
        self.secret_registry = Some(registry);
        Ok(self)
    }
    pub fn secret_registry(&self) -> Option<Arc<saya_core::credentials::SecretRegistry>> {
        self.secret_registry.clone()
    }
    fn current_secrets(&self) -> Result<Vec<String>, BoundaryError> {
        match &self.secret_registry {
            Some(registry) => registry.snapshot(),
            None => Ok(self.secrets.clone()),
        }
    }
    fn safe_event(&self, event: &Event) -> Result<Event, BoundaryError> {
        let secrets = self.current_secrets()?;
        let mut value = serde_json::to_value(event)
            .map_err(|_| cli_error("output_error", "cannot serialize event"))?;
        redact_value(&mut value, &secrets, 0);
        if contains_encoded_secret(&value.to_string(), &secrets) {
            return Err(cli_error(
                "output_error",
                "sensitive event details withheld",
            ));
        }
        serde_json::from_value(value).map_err(|_| cli_error("output_error", "cannot encode event"))
    }
    async fn publish(&self, event: &Event) -> Result<(), BoundaryError> {
        let safe = self.safe_event(event)?;
        self.sink.publish(&safe).await
    }
    async fn event(
        &self,
        state: &mut State,
        payload: EventPayload,
        run: Option<RunId>,
    ) -> Result<Event, BoundaryError> {
        // Keep the final reserved number available for a terminal failure if
        // the next database reservation fails. Visible sequences are never reused.
        if state.next >= state.end {
            let (start, end) = self
                .store
                .reserve_event_sequences(self.session, 4096)
                .await?;
            state.next = start;
            state.end = end;
        }
        let seq = state.next;
        state.next += 1;
        self.envelope(payload, run, seq)
    }
    fn envelope(
        &self,
        payload: EventPayload,
        run: Option<RunId>,
        seq: u64,
    ) -> Result<Event, BoundaryError> {
        let timestamp = timestamp()?;
        self.safe_event(&Event {
            schema_version: SCHEMA_VERSION,
            session_id: self.session,
            run_id: run,
            seq,
            timestamp,
            payload,
        })
    }
    pub async fn emit(
        &self,
        payload: EventPayload,
        run: Option<RunId>,
        durable: bool,
    ) -> Result<(), BoundaryError> {
        let mut state = self.state.lock().await;
        if state.terminal_emitted {
            return if matches!(payload, EventPayload::RunFinished(_)) {
                Ok(())
            } else {
                Err(cli_error("output_error", "event stream already finished"))
            };
        }
        let interrupted = self.take_interrupted()?;
        for payload in interrupted {
            let event = self.event(&mut state, payload, Some(self.run)).await?;
            self.store
                .commit(&[StoreChange::ReservedEvent(event.clone())])
                .await?;
            self.publish(&event).await?;
        }
        let event = self.event(&mut state, payload, run).await?;
        if durable {
            self.store
                .commit(&[StoreChange::ReservedEvent(event.clone())])
                .await?;
        }
        self.publish(&event).await?;
        state.terminal_emitted |= matches!(event.payload, EventPayload::RunFinished(_));
        Ok(())
    }
    /// Deliver one terminal failure using an already reserved sequence when
    /// storage cannot reserve another block. The fallback is not persisted.
    /// Sink failures stop delivery without appending another output record.
    pub async fn emit_failure(&self, result: RunResult) -> Result<(), BoundaryError> {
        let payload = EventPayload::RunFinished(result);
        match self.emit(payload.clone(), Some(self.run), false).await {
            Ok(()) => return Ok(()),
            Err(error) if error.0.code.starts_with("storage_") => {}
            Err(error) => return Err(error),
        }
        let mut state = self.state.lock().await;
        if state.terminal_emitted {
            return Ok(());
        }
        if state.next > state.end {
            return Err(cli_error(
                "output_error",
                "no reserved terminal event sequence",
            ));
        }
        let seq = state.next;
        state.next += 1;
        let event = self.envelope(payload, Some(self.run), seq)?;
        self.publish(&event).await?;
        state.terminal_emitted = true;
        Ok(())
    }
    async fn store_changes(&self, changes: &[StoreChange]) -> Result<(), BoundaryError> {
        let mut state = self.state.lock().await;
        let mut payloads = self.take_interrupted()?;
        for change in changes {
            match change {
                StoreChange::RunStarted(run) => payloads.push(EventPayload::RunStarted {
                    work_mode: run.work_mode,
                    permissions: run.permissions,
                    profile: self
                        .store
                        .load_session(self.session)
                        .await?
                        .ok_or_else(|| cli_error("storage_error", "session missing"))?
                        .profile,
                }),
                StoreChange::History { item, .. } => match item {
                    HistoryItem::AssistantText(_)
                    | HistoryItem::ToolCall(_)
                    | HistoryItem::ProviderItem { .. } => {
                        if let Some(payload) = state.pending_execution.take() {
                            payloads.push(payload);
                        }
                        if let HistoryItem::ToolCall(call) = item {
                            state.names.insert(call.call_id.clone(), call.name.clone());
                            let args = serde_json::from_str(&call.arguments).unwrap_or_else(|_| {
                                serde_json::Value::String(call.arguments.clone())
                            });
                            payloads.push(EventPayload::ToolRequested {
                                call_id: call.call_id.clone(),
                                name: call.name.clone(),
                                args,
                            });
                        }
                    }
                    _ => {}
                },
                StoreChange::ToolIntent { call, .. } => {
                    state.names.insert(call.call_id.clone(), call.name.clone());
                    // A locally paired intent/result records a rejected or
                    // otherwise undispatched call; it is not proof of execution.
                    if !changes.iter().any(|other| {
                        matches!(other,
                        StoreChange::ToolFinished {call_id, ..} if call_id == &call.call_id)
                    }) {
                        payloads.push(EventPayload::ToolStarted {
                            call_id: call.call_id.clone(),
                            name: call.name.clone(),
                        });
                    }
                }
                StoreChange::ToolFinished {
                    call_id, result, ..
                } => {
                    if let Some(name) = state.names.get(call_id) {
                        payloads.push(EventPayload::ToolFinished {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            result: result.clone(),
                        });
                    }
                }
                StoreChange::ApprovalRequested(request) => {
                    payloads.push(EventPayload::PermissionRequested {
                        approval_id: request.approval_id.clone(),
                        call_id: request.call_id.clone(),
                        operation_summary: request.operation_summary.clone(),
                        work_mode: request.work_mode,
                    })
                }
                StoreChange::Activity { payload, .. } => payloads.push(payload.clone()),
                StoreChange::RunFinished(result) => {
                    if let Some(payload) = state.pending_execution.take() {
                        payloads.push(payload);
                    }
                    payloads.push(EventPayload::RunFinished(result.clone()))
                }
                _ => {}
            }
        }
        let mut all = changes.to_vec();
        let mut events = Vec::new();
        for payload in payloads {
            let event = self.event(&mut state, payload, Some(self.run)).await?;
            all.push(StoreChange::ReservedEvent(event.clone()));
            events.push(event);
        }
        self.store.commit(&all).await?;
        for event in &events {
            self.publish(event).await?;
            state.terminal_emitted |= matches!(event.payload, EventPayload::RunFinished(_));
        }
        Ok(())
    }
    async fn execution_finished(&self, payload: EventPayload) {
        self.state.lock().await.pending_execution = Some(payload);
    }
    fn take_interrupted(&self) -> Result<Vec<EventPayload>, BoundaryError> {
        let mut pending = self
            .interrupted
            .lock()
            .map_err(|_| cli_error("output_error", "event lock failed"))?;
        Ok(std::mem::take(&mut *pending))
    }
}
pub fn timestamp() -> Result<String, BoundaryError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| cli_error("clock_error", "system clock is before epoch"))?
        .as_secs();
    let seconds = libc::time_t::try_from(seconds)
        .map_err(|_| cli_error("clock_error", "system clock is out of range"))?;
    let mut tm = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: gmtime_r writes a valid tm to the provided pointer on success.
    let ptr = unsafe { libc::gmtime_r(&seconds, tm.as_mut_ptr()) };
    if ptr.is_null() {
        return Err(cli_error("clock_error", "cannot read UTC clock"));
    }
    let tm = unsafe { tm.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

pub struct EventStore(pub Arc<EventWriter>);
impl Store for EventStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.0.store.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move { self.0.store_changes(changes).await })
    }
}

pub struct EventModel(pub Arc<dyn Model>, pub Arc<EventWriter>);
struct EventStream {
    inner: Box<dyn ModelStream>,
    events: Arc<EventWriter>,
    request_id: String,
    purpose: ModelPurpose,
    buffered: String,
    guard: ModelGuard,
}
struct ModelGuard {
    events: Arc<EventWriter>,
    request_id: String,
    purpose: ModelPurpose,
    finished: bool,
}
impl Drop for ModelGuard {
    fn drop(&mut self) {
        if !self.finished
            && let Ok(mut pending) = self.events.interrupted.lock()
        {
            pending.push(EventPayload::ModelFinished {
                request_id: self.request_id.clone(),
                purpose: self.purpose,
                ok: false,
                usage: None,
                error: Some(ErrorInfo {
                    code: "model_interrupted".into(),
                    message: "model request ended before a complete response".into(),
                }),
            });
        }
    }
}
impl Model for EventModel {
    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        self.0.validate_session(session)
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.1
                .emit(
                    EventPayload::ModelStarted {
                        request_id: request.request_id.clone(),
                        purpose: request.purpose,
                        profile: request.profile.clone(),
                    },
                    Some(self.1.run),
                    true,
                )
                .await?;
            let mut guard = ModelGuard {
                events: self.1.clone(),
                request_id: request.request_id.clone(),
                purpose: request.purpose,
                finished: false,
            };
            match self.0.start(request, cancel).await {
                Ok(inner) => Ok(Box::new(EventStream {
                    inner,
                    events: self.1.clone(),
                    request_id: request.request_id.clone(),
                    purpose: request.purpose,
                    buffered: String::new(),
                    guard,
                }) as Box<dyn ModelStream>),
                Err(error) => {
                    guard.finished = true;
                    self.1
                        .emit(
                            EventPayload::ModelFinished {
                                request_id: request.request_id.clone(),
                                purpose: request.purpose,
                                ok: false,
                                usage: None,
                                error: Some(error.0.clone()),
                            },
                            Some(self.1.run),
                            true,
                        )
                        .await?;
                    Err(error)
                }
            }
        })
    }
}
impl ModelStream for EventStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            let update = match self.inner.next(cancel).await {
                Ok(update) => update,
                Err(error) => {
                    self.guard.finished = true;
                    self.events
                        .emit(
                            EventPayload::ModelFinished {
                                request_id: self.request_id.clone(),
                                purpose: self.purpose,
                                ok: false,
                                usage: None,
                                error: Some(error.0.clone()),
                            },
                            Some(self.events.run),
                            true,
                        )
                        .await?;
                    return Err(error);
                }
            };
            match &update {
                Some(ModelUpdate::TextDelta(text)) if self.purpose == ModelPurpose::Execution => {
                    if self.events.secret_registry.is_none() && self.events.secrets.is_empty() {
                        self.events
                            .emit(
                                EventPayload::ModelTextDelta {
                                    request_id: self.request_id.clone(),
                                    text: text.clone(),
                                },
                                Some(self.events.run),
                                false,
                            )
                            .await?;
                    } else {
                        self.buffered.push_str(text);
                    }
                }
                Some(ModelUpdate::Finished { usage, .. }) => {
                    self.guard.finished = true;
                    if !self.buffered.is_empty() {
                        self.events
                            .emit(
                                EventPayload::ModelTextDelta {
                                    request_id: self.request_id.clone(),
                                    text: std::mem::take(&mut self.buffered),
                                },
                                Some(self.events.run),
                                false,
                            )
                            .await?;
                    }
                    let payload = EventPayload::ModelFinished {
                        request_id: self.request_id.clone(),
                        purpose: self.purpose,
                        ok: true,
                        usage: usage.clone() as Option<Usage>,
                        error: None,
                    };
                    if self.purpose == ModelPurpose::Execution {
                        self.events.execution_finished(payload).await;
                    } else {
                        self.events
                            .emit(payload, Some(self.events.run), true)
                            .await?;
                    }
                }
                None => {
                    self.guard.finished = true;
                    self.events
                        .emit(
                            EventPayload::ModelFinished {
                                request_id: self.request_id.clone(),
                                purpose: self.purpose,
                                ok: false,
                                usage: None,
                                error: Some(ErrorInfo {
                                    code: "incomplete_model_response".into(),
                                    message: "stream ended before completion".into(),
                                }),
                            },
                            Some(self.events.run),
                            true,
                        )
                        .await?;
                }
                _ => {}
            }
            Ok(update)
        })
    }
}
