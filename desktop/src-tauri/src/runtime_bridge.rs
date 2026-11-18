//! In-process bridge between Tauri commands/channels and `soyo-app` / `soyo-core`.

use crate::{
    redaction, tool_format,
    types::{
        ActiveSessionDto, ApprovalRequestDto, AttachmentTokenDto, BootstrapStateDto,
        ContextEntryDto, DesktopError, DesktopStreamEnvelope, InputRequestDto, McpStatusBadgeDto,
        MessageRoleDto, ModeStateDto, PermissionModeDto, ProfileOptionDto, RestoredMessageDto,
        ResumeSessionResultDto, RunResultDto, SchemaEnvelopeDto, SchemaEventDto, SessionItemDto,
        SessionPageDto, SubmitPromptResultDto, ThinkingSnapshotDto, TodoItemDto, ToolResultDto,
        WorkModeDto,
    },
};
use serde_json::Value;
use soyo_app::{
    context::{ContextModel, ContextSnapshot, ContextSources},
    events::{ChannelSink, EventModel, EventSink, EventStore, EventWriter},
    interaction::{Request, RunCancellation, input_channel},
    offload, offload_boundary,
    session::{self, Prepared},
    thinking::{ThinkingLog, ThinkingModel, extract_reasoning},
};
use soyo_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, Operation, Store, StoreChange},
    config::{self, Config, Environment, Overrides},
    contract::{
        BoundaryError, ErrorInfo, Event, EventPayload, HistoryItem, ModelPurpose, RunId, RunResult,
        Session, Usage, WorkMode,
    },
    credentials::SecretRegistry,
    execution::BackendKind,
    media,
    runtime::{PersonalContext, RuntimeHandle},
    sqlite::SessionSummary,
    tools::todo::{current_todos, result_todos},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tauri::ipc::Channel;
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};

const SESSIONS_PAGE_SIZE: u64 = 20;
const MAX_RESTORED_HISTORY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Execution,
    Compaction,
}

#[derive(Debug, Clone, Copy)]
pub struct StreamSendError;

#[derive(Clone)]
pub enum StreamSink {
    Tauri(Channel<DesktopStreamEnvelope>),
    Mpsc(mpsc::UnboundedSender<DesktopStreamEnvelope>),
}

impl StreamSink {
    pub fn send(&self, envelope: DesktopStreamEnvelope) -> Result<(), StreamSendError> {
        match self {
            Self::Tauri(channel) => channel.send(envelope).map_err(|_| StreamSendError),
            Self::Mpsc(tx) => tx.send(envelope).map_err(|_| StreamSendError),
        }
    }
}

pub type TestModelFactory = Arc<dyn Fn() -> Arc<dyn Model> + Send + Sync>;

pub struct ActiveControl {
    pub run_id: RunId,
    pub kind: RunKind,
    pub cancel: Arc<RunCancellation>,
    pub handle: Option<RuntimeHandle>,
    pub pending: Option<PendingInteraction>,
}

pub enum PendingInteraction {
    Approval {
        approval_id: String,
        call_id: String,
        request: Request,
    },
    Input {
        input_id: String,
        request: Request,
    },
}

pub struct DesktopInner {
    pub config: Option<Config>,
    pub config_error: Option<String>,
    pub prepared: Option<Prepared>,
    pub active_run_id: Option<RunId>,
    pub last_usage: Option<Usage>,
}

pub struct DesktopState {
    pub inner: Arc<AsyncMutex<DesktopInner>>,
    pub active_control: Arc<StdMutex<Option<ActiveControl>>>,
    pub secrets: Arc<SecretRegistry>,
    pub test_model_factory: Option<TestModelFactory>,
}

#[derive(Default)]
struct Measurements {
    pending: Option<(String, Instant)>,
    requests: u64,
    output_tokens: u64,
    elapsed: Duration,
    estimated: bool,
    incomplete: bool,
}

impl Measurements {
    fn observe(&mut self, payload: &EventPayload, now: Instant) {
        match payload {
            EventPayload::ModelStarted {
                request_id,
                purpose: ModelPurpose::Execution,
                ..
            } => {
                if self.pending.is_some() || request_id.len() > 256 {
                    self.incomplete = true;
                    self.pending = None;
                    return;
                }
                self.requests = self.requests.saturating_add(1);
                self.pending = Some((request_id.clone(), now));
            }
            EventPayload::ModelFinished {
                request_id,
                purpose: ModelPurpose::Execution,
                usage,
                ..
            } => {
                let Some((pending_id, started)) = self.pending.take() else {
                    self.incomplete = true;
                    return;
                };
                let duration = now.checked_duration_since(started);
                let output = usage.as_ref().and_then(|u| u.output_tokens);
                let elapsed = duration.and_then(|d| self.elapsed.checked_add(d));
                let tokens = output.and_then(|o| self.output_tokens.checked_add(o));
                if pending_id != *request_id || elapsed.is_none() || tokens.is_none() {
                    self.incomplete = true;
                    return;
                }
                if let (Some(elapsed), Some(tokens)) = (elapsed, tokens) {
                    self.elapsed = elapsed;
                    self.output_tokens = tokens;
                    self.estimated |= usage.as_ref().is_some_and(|u| u.estimated);
                }
            }
            _ => {}
        }
    }

    fn summary_line(&self, model: &str, total_elapsed: Duration, usage: Option<&Usage>) -> String {
        let elapsed_str = format_elapsed(total_elapsed);
        let rate_str = if !self.incomplete
            && self.pending.is_none()
            && self.requests > 0
            && !self.elapsed.is_zero()
        {
            let rate = self.output_tokens as f64 / self.elapsed.as_secs_f64();
            if rate.is_finite() {
                if self.estimated {
                    format!("~{rate:.1} tok/s")
                } else {
                    format!("{rate:.1} tok/s")
                }
            } else {
                "— tok/s".into()
            }
        } else {
            "— tok/s".into()
        };
        let tokens_str = usage
            .and_then(|u| {
                u.input_tokens?
                    .checked_add(u.output_tokens?)
                    .map(|total| format_tokens(total, u.estimated))
            })
            .unwrap_or_else(|| "— tokens".into());
        format!("{model} · {elapsed_str} · {rate_str} · {tokens_str}")
    }
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let m = d.as_secs() / 60;
        let s = d.as_secs() % 60;
        format!("{m}m {s}s")
    }
}

fn format_tokens(total: u64, estimated: bool) -> String {
    let prefix = if estimated { "~" } else { "" };
    if total >= 1_000_000 {
        format!("{prefix}{:.1}m tokens", total as f64 / 1_000_000.0)
    } else if total >= 1_000 {
        let k = total as f64 / 1_000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{prefix}{}k tokens", k.round() as u64)
        } else {
            format!("{prefix}{k:.1}k tokens")
        }
    } else {
        format!("{prefix}{total} tokens")
    }
}

struct SummarySink {
    channel: ChannelSink,
    measurements: Arc<StdMutex<Measurements>>,
}

impl EventSink for SummarySink {
    fn publish<'a>(&'a self, event: &'a Event) -> Operation<'a, ()> {
        if let Ok(mut guard) = self.measurements.lock() {
            guard.observe(&event.payload, Instant::now());
        }
        self.channel.publish(event)
    }
}

impl DesktopState {
    pub fn from_environment() -> Self {
        let env = Environment::capture();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let config_path = config::config_path(&Overrides::default(), &env)
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        match config::load(Overrides::default(), env, &cwd) {
            Ok(cfg) => Self::with_config(cfg),
            Err(err) => Self {
                inner: Arc::new(AsyncMutex::new(DesktopInner {
                    config: None,
                    config_error: Some(format!(
                        "{}{}",
                        err.message(),
                        config_path.map(|p| format!(" ({p})")).unwrap_or_default()
                    )),
                    prepared: None,
                    active_run_id: None,
                    last_usage: None,
                })),
                active_control: Arc::new(StdMutex::new(None)),
                secrets: Arc::new(SecretRegistry::default()),
                test_model_factory: None,
            },
        }
    }

    pub fn with_config(mut cfg: Config) -> Self {
        if cfg.profile.is_none() && cfg.profiles.len() == 1 {
            cfg.profile = cfg.profiles.keys().next().cloned();
        }
        let registry = Arc::new(SecretRegistry::default());
        register_config_secrets(&cfg, &registry);
        Self {
            inner: Arc::new(AsyncMutex::new(DesktopInner {
                config: Some(cfg),
                config_error: None,
                prepared: None,
                active_run_id: None,
                last_usage: None,
            })),
            active_control: Arc::new(StdMutex::new(None)),
            secrets: registry,
            test_model_factory: None,
        }
    }

    pub fn with_test_model(mut self, factory: TestModelFactory) -> Self {
        self.test_model_factory = Some(factory);
        self
    }

    pub fn current_secrets(&self) -> Vec<String> {
        self.secrets.snapshot().unwrap_or_default()
    }

    pub async fn get_bootstrap_state(&self) -> BootstrapStateDto {
        let guard = self.inner.lock().await;
        let secrets = self.current_secrets();
        let active_run_id = guard.active_run_id.map(|id| id.0.to_string());
        let (current_mode, pending_mode) = self.active_mode_snapshot();

        let Some(cfg) = &guard.config else {
            return BootstrapStateDto {
                version: env!("CARGO_PKG_VERSION").into(),
                config_ready: false,
                config_path: String::new(),
                config_error: guard
                    .config_error
                    .as_deref()
                    .map(|msg| redaction::redact_str(msg, &secrets)),
                workspace: std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("/"))
                    .to_string_lossy()
                    .into_owned(),
                selected_profile: None,
                selected_model: None,
                context_window_tokens: None,
                thinking_effort: None,
                work_mode: WorkModeDto::Work,
                pending_mode: None,
                permission_mode: PermissionModeDto::Auto,
                sandbox_backend: "native".into(),
                auto_compact: true,
                auto_compact_trigger_tokens: None,
                auto_compact_trigger_percent: 80,
                profiles: Vec::new(),
                session: None,
                run_active: active_run_id.is_some(),
                active_run_id,
            };
        };

        let selected = cfg.profile.as_ref().and_then(|name| cfg.profiles.get(name));
        let profiles = cfg
            .profiles
            .iter()
            .map(|(name, profile)| ProfileOptionDto {
                name: redaction::redact_str(name, &secrets),
                model: redaction::redact_str(&profile.model, &secrets),
                protocol: format!("{:?}", profile.protocol),
                context_window_tokens: profile.context_window_tokens,
                thinking_effort: profile.thinking_effort.map(|e| e.as_str().to_owned()),
            })
            .collect();

        let session = guard.prepared.as_ref().map(|prepared| {
            build_active_session_dto(&prepared.session, guard.last_usage.clone(), &secrets)
        });

        let work_mode = current_mode.unwrap_or(cfg.mode).into();

        BootstrapStateDto {
            version: env!("CARGO_PKG_VERSION").into(),
            config_ready: true,
            config_path: redaction::redact_str(&cfg.config_path.to_string_lossy(), &secrets),
            config_error: None,
            workspace: redaction::redact_str(&cfg.cwd.to_string_lossy(), &secrets),
            selected_profile: cfg
                .profile
                .as_deref()
                .map(|p| redaction::redact_str(p, &secrets)),
            selected_model: selected.map(|p| redaction::redact_str(&p.model, &secrets)),
            context_window_tokens: selected.map(|p| p.context_window_tokens),
            thinking_effort: selected
                .and_then(|p| p.thinking_effort)
                .map(|e| e.as_str().to_owned()),
            work_mode,
            pending_mode,
            permission_mode: cfg.permissions_mode.into(),
            sandbox_backend: sandbox_label(cfg.sandbox.backend).into(),
            auto_compact: cfg.context.auto_compact,
            auto_compact_trigger_tokens: cfg.context.trigger_tokens,
            auto_compact_trigger_percent: cfg.context.trigger_percent,
            profiles,
            session,
            run_active: active_run_id.is_some(),
            active_run_id,
        }
    }

    fn active_mode_snapshot(&self) -> (Option<WorkMode>, Option<WorkModeDto>) {
        let Ok(guard) = self.active_control.lock() else {
            return (None, None);
        };
        let Some(control) = guard.as_ref() else {
            return (None, None);
        };
        let Some(handle) = &control.handle else {
            return (None, None);
        };
        match handle.mode() {
            Ok(snapshot) => (Some(snapshot.current), snapshot.pending.map(Into::into)),
            Err(_) => (None, None),
        }
    }

    pub async fn list_sessions_page(
        &self,
        page: Option<u32>,
    ) -> Result<SessionPageDto, DesktopError> {
        let page_num = page.unwrap_or(1).max(1);
        let offset = u64::from(page_num - 1) * SESSIONS_PAGE_SIZE;
        let cfg = self.require_config().await?;
        let secrets = self.current_secrets();
        let store = offload_boundary(move || {
            session::open(&cfg).map_err(|err| BoundaryError::new("storage_error", err.to_string()))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let summaries = store
            .list_sessions(offset, SESSIONS_PAGE_SIZE)
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let items = summaries
            .into_iter()
            .map(|summary| redact_summary(summary, &secrets))
            .collect();
        Ok(SessionPageDto {
            page: page_num,
            page_size: SESSIONS_PAGE_SIZE as u32,
            items,
        })
    }

    pub async fn new_session(&self) -> Result<ActiveSessionDto, DesktopError> {
        let secrets = self.current_secrets();
        let mut guard = self.inner.lock().await;
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Cancel or wait for the active run before creating a new session",
            ));
        }
        let cfg = guard.config.clone().ok_or_else(|| {
            DesktopError::new(
                "invalid_config",
                guard
                    .config_error
                    .as_deref()
                    .unwrap_or("Configuration is not loaded"),
            )
        })?;
        let new_sess = soyo_app::new_session(&cfg).map_err(|err| {
            DesktopError::new(
                "invalid_config",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;
        let prepared = session::create(&cfg, new_sess).await.map_err(|err| {
            DesktopError::new(
                "storage_error",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;
        let dto = build_active_session_dto(&prepared.session, None, &secrets);
        guard.last_usage = None;
        guard.prepared = Some(prepared);
        Ok(dto)
    }

    pub async fn submit_prompt(
        &self,
        prompt: String,
        attachments: Vec<AttachmentTokenDto>,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            return Err(DesktopError::new("invalid_input", "Prompt cannot be empty"));
        }
        let expanded = expand_attachments(&prompt, &attachments);

        // Fast-path: if a run is currently active, steer it at the next model boundary.
        let steered_run_id = {
            let control_guard = self.active_control.lock().ok();
            if let Some(control) = control_guard.as_ref().and_then(|g| g.as_ref()) {
                if control.kind == RunKind::Compaction {
                    return Err(DesktopError::new(
                        "compaction_active",
                        "Compaction is currently running; wait or cancel before steering",
                    ));
                }
                let handle = control.handle.as_ref().ok_or_else(|| {
                    DesktopError::new(
                        "run_starting",
                        "Run is still initializing; please retry in a moment",
                    )
                })?;
                let secrets = self.current_secrets();
                handle
                    .steer(expanded.clone())
                    .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                Some(control.run_id.0.to_string())
            } else {
                None
            }
        };
        if let Some(run_id) = steered_run_id {
            let session_id = {
                let guard = self.inner.lock().await;
                guard
                    .prepared
                    .as_ref()
                    .map(|p| p.session.id.0.to_string())
                    .unwrap_or_default()
            };
            return Ok(SubmitPromptResultDto {
                action: "steered".into(),
                session_id,
                run_id,
            });
        }

        self.start_run(Some(expanded), RunKind::Execution, sink)
            .await
    }

    pub async fn compact_session(
        &self,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        if let Ok(control_guard) = self.active_control.lock()
            && let Some(control) = control_guard.as_ref()
        {
            if control.cancel.is_cancelled() {
                return Err(DesktopError::new(
                    "cancelling",
                    "Run is cancelling; wait for cleanup before compacting",
                ));
            }
            if control.kind == RunKind::Compaction {
                return Err(DesktopError::new(
                    "compaction_active",
                    "Compaction is already running",
                ));
            }
            let handle = control
                .handle
                .as_ref()
                .ok_or_else(|| DesktopError::new("run_starting", "Run is still initializing"))?;
            let secrets = self.current_secrets();
            handle
                .request_compaction()
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
            return Ok(SubmitPromptResultDto {
                action: "compaction_queued".into(),
                session_id: String::new(),
                run_id: control.run_id.0.to_string(),
            });
        }

        self.start_run(None, RunKind::Compaction, sink).await
    }

    async fn start_run(
        &self,
        expanded_prompt: Option<String>,
        kind: RunKind,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let secrets = self.current_secrets();
        let mut guard = self.inner.lock().await;
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Another run is already active",
            ));
        }
        let cfg = guard.config.clone().ok_or_else(|| {
            DesktopError::new(
                "invalid_config",
                guard
                    .config_error
                    .as_deref()
                    .unwrap_or("Configuration is not loaded"),
            )
        })?;
        if cfg.profile.is_none() {
            return Err(DesktopError::new(
                "invalid_config",
                "Select a configured model profile first",
            ));
        }

        if guard.prepared.is_none() {
            if kind == RunKind::Compaction {
                return Err(DesktopError::new(
                    "session_required",
                    "Start or resume a session before compacting",
                ));
            }
            let new_sess = soyo_app::new_session(&cfg).map_err(|err| {
                DesktopError::new(
                    "invalid_config",
                    redaction::redact_str(&err.to_string(), &secrets),
                )
            })?;
            let prepared = session::create(&cfg, new_sess).await.map_err(|err| {
                DesktopError::new(
                    "storage_error",
                    redaction::redact_str(&err.to_string(), &secrets),
                )
            })?;
            guard.prepared = Some(prepared);
        }

        let run_id = RunId::new();
        let cancel = Arc::new(RunCancellation::new());
        {
            let mut control = self
                .active_control
                .lock()
                .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
            *control = Some(ActiveControl {
                run_id,
                kind,
                cancel: cancel.clone(),
                handle: None,
                pending: None,
            });
        }
        guard.active_run_id = Some(run_id);

        let prepared = guard.prepared.as_mut().ok_or_else(|| {
            DesktopError::new("session_unavailable", "Prepared session is unavailable")
        })?;
        let session_snapshot = prepared.session.clone();
        let session_id = session_snapshot.id;
        let model_name = cfg
            .profiles
            .get(&session_snapshot.profile)
            .map(|p| p.model.clone())
            .unwrap_or_else(|| session_snapshot.model.clone());

        let (event_tx, event_rx) = mpsc::channel(256);
        let (context_tx, context_rx) = watch::channel(None);
        let (thinking_tx, thinking_rx) = watch::channel(ThinkingLog::default());
        let measurements = Arc::new(StdMutex::new(Measurements::default()));
        let summary_sink = Arc::new(SummarySink {
            channel: ChannelSink(event_tx),
            measurements: measurements.clone(),
        });

        let dynamic_credentials = cfg.mcp.servers.values().any(|s| s.enabled);
        let writer = EventWriter::new(
            prepared.store.clone(),
            session_id,
            run_id,
            secrets.clone(),
            summary_sink,
        );
        let writer = Arc::new(if dynamic_credentials {
            writer
                .with_secret_registry(self.secrets.clone())
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?
        } else {
            writer
        });
        let store: Arc<dyn Store> = Arc::new(EventStore(writer.clone()));
        let (interaction, request_rx) =
            input_channel(16).map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        let launch_summary = expanded_prompt
            .clone()
            .unwrap_or_else(|| "Compact conversation context".into());
        let thinking_secrets = secrets.clone();
        let thinking_registry = dynamic_credentials.then(|| self.secrets.clone());
        let test_model_factory = self.test_model_factory.clone();
        let context_sources = ContextSources {
            project: true,
            personal: false,
        };

        let build_result = soyo_app::spawn_build_runtime(
            cfg.clone(),
            session_snapshot.clone(),
            launch_summary,
            soyo_app::Launch {
                permissions: cfg.permissions_mode,
                store,
                cancellation: cancel.clone(),
                confirmation: Some(interaction.clone()),
                input: Some(interaction),
                approvals: Preauthorizations::default(),
                personal_context: PersonalContext::default(),
                anthropic_prompt_cache: false,
                run_id: Some(run_id),
                secret_registry: Some(self.secrets.clone()),
            },
            move |model, _| {
                let inner_model = match &test_model_factory {
                    Some(factory) => factory(),
                    None => model,
                };
                let thinking = ThinkingModel::new(inner_model, thinking_tx, thinking_secrets);
                let thinking = match thinking_registry {
                    Some(registry) => thinking.with_secret_registry(registry),
                    None => thinking,
                };
                Arc::new(
                    ContextModel::new(
                        Arc::new(EventModel(Arc::new(thinking), writer)),
                        context_tx,
                        context_sources,
                    )
                    .with_managed_sources(true, true),
                )
            },
        )
        .await;

        let (mut runtime, _) = match build_result {
            Ok(built) => built,
            Err(err) => {
                self.clear_active_state(&mut guard);
                return Err(DesktopError::new(
                    "runtime_build_failed",
                    redaction::redact_str(&err.to_string(), &secrets),
                ));
            }
        };

        if kind == RunKind::Execution
            && let Some(text) = &expanded_prompt
            && let Err(err) = session::append(prepared, text.clone()).await
        {
            self.clear_active_state(&mut guard);
            return Err(DesktopError::new(
                "storage_error",
                redaction::redact_str(&err.to_string(), &secrets),
            ));
        }

        let mut current = prepared.session.clone();
        let handle = runtime.handle();
        if let Ok(mut control) = self.active_control.lock()
            && let Some(active) = control.as_mut()
        {
            active.handle = Some(handle.clone());
        }

        let started_at = Instant::now();
        let runtime_join = tokio::spawn(async move {
            let outcome = match kind {
                RunKind::Execution => runtime.run(&mut current).await,
                RunKind::Compaction => runtime.compact(&mut current).await,
            };
            (outcome, current)
        });

        let inner_arc = self.inner.clone();
        let active_control = self.active_control.clone();
        let registry = self.secrets.clone();
        let model_display = redaction::redact_str(&model_name, &secrets);

        tokio::spawn(async move {
            drive_active_run(DriverParams {
                run_id,
                model_display,
                started_at,
                sink,
                cancel,
                handle,
                runtime_join,
                event_rx,
                context_rx,
                thinking_rx,
                request_rx,
                measurements,
                inner: inner_arc,
                active_control,
                secrets: registry,
            })
            .await;
        });

        Ok(SubmitPromptResultDto {
            action: if kind == RunKind::Compaction {
                "compacting".into()
            } else {
                "started".into()
            },
            session_id: session_id.0.to_string(),
            run_id: run_id.0.to_string(),
        })
    }

    fn clear_active_state(&self, guard: &mut DesktopInner) {
        guard.active_run_id = None;
        if let Ok(mut control) = self.active_control.lock() {
            *control = None;
        }
    }

    pub fn cancel_run(&self) -> bool {
        let Ok(mut guard) = self.active_control.lock() else {
            return false;
        };
        let Some(control) = guard.as_mut() else {
            return false;
        };
        control.pending = None;
        let _ = control.cancel.cancel();
        true
    }

    pub fn reply_approval(
        &self,
        approval_id: &str,
        call_id: &str,
        approved: bool,
    ) -> Result<(), DesktopError> {
        let mut guard = self
            .active_control
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
        let Some(control) = guard.as_mut() else {
            return Err(DesktopError::new(
                "stale_approval",
                "No active run owns this approval request",
            ));
        };
        let Some(pending) = control.pending.take() else {
            return Err(DesktopError::new(
                "stale_approval",
                "No pending approval request is awaiting reply",
            ));
        };
        match pending {
            PendingInteraction::Approval {
                approval_id: cur_approval_id,
                call_id: cur_call_id,
                request,
            } => {
                if cur_approval_id != approval_id || cur_call_id != call_id {
                    control.pending = Some(PendingInteraction::Approval {
                        approval_id: cur_approval_id,
                        call_id: cur_call_id,
                        request,
                    });
                    return Err(DesktopError::new(
                        "stale_approval",
                        "Approval request does not match the active confirmation",
                    ));
                }
                match request {
                    Request::Approval(_, reply) => {
                        if reply.is_closed() {
                            return Err(DesktopError::new(
                                "stale_approval",
                                "Approval request expired or was cancelled",
                            ));
                        }
                        if reply.send(Ok(Some(approved))).is_err() {
                            return Err(DesktopError::new(
                                "stale_approval",
                                "Approval receiver closed before reply delivery",
                            ));
                        }
                        Ok(())
                    }
                    other => {
                        control.pending = Some(PendingInteraction::Input {
                            input_id: String::new(),
                            request: other,
                        });
                        Err(DesktopError::new(
                            "stale_approval",
                            "Pending interaction is not an approval request",
                        ))
                    }
                }
            }
            other => {
                control.pending = Some(other);
                Err(DesktopError::new(
                    "stale_approval",
                    "Pending interaction is an input prompt, not an approval request",
                ))
            }
        }
    }

    pub fn reply_input(&self, input_id: &str, answer: String) -> Result<(), DesktopError> {
        let mut guard = self
            .active_control
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
        let Some(control) = guard.as_mut() else {
            return Err(DesktopError::new(
                "stale_input",
                "No active run owns this input request",
            ));
        };
        let Some(pending) = control.pending.take() else {
            return Err(DesktopError::new(
                "stale_input",
                "No pending input request is awaiting reply",
            ));
        };
        match pending {
            PendingInteraction::Input {
                input_id: cur_input_id,
                request,
            } => {
                if cur_input_id != input_id {
                    control.pending = Some(PendingInteraction::Input {
                        input_id: cur_input_id,
                        request,
                    });
                    return Err(DesktopError::new(
                        "stale_input",
                        "Input request does not match the active clarification prompt",
                    ));
                }
                match request {
                    Request::Input(_, reply) => {
                        if reply.is_closed() {
                            return Err(DesktopError::new(
                                "stale_input",
                                "Input request expired or was cancelled",
                            ));
                        }
                        if reply.send(Ok(Some(answer))).is_err() {
                            return Err(DesktopError::new(
                                "stale_input",
                                "Input receiver closed before answer delivery",
                            ));
                        }
                        Ok(())
                    }
                    other => {
                        control.pending = Some(PendingInteraction::Approval {
                            approval_id: String::new(),
                            call_id: String::new(),
                            request: other,
                        });
                        Err(DesktopError::new(
                            "stale_input",
                            "Pending interaction is not an input request",
                        ))
                    }
                }
            }
            other => {
                control.pending = Some(other);
                Err(DesktopError::new(
                    "stale_input",
                    "Pending interaction is an approval request, not an input prompt",
                ))
            }
        }
    }

    pub async fn set_work_mode(&self, mode: WorkModeDto) -> Result<ModeStateDto, DesktopError> {
        let target: WorkMode = mode.into();
        let secrets = self.current_secrets();
        let (cfg, workspace) = {
            let guard = self.inner.lock().await;
            let cfg = guard.config.clone().ok_or_else(|| {
                DesktopError::new(
                    "invalid_config",
                    guard
                        .config_error
                        .as_deref()
                        .unwrap_or("Configuration is not loaded"),
                )
            })?;
            let workspace = guard
                .prepared
                .as_ref()
                .map(|p| p.session.workspace.clone())
                .unwrap_or_else(|| cfg.cwd.clone());
            (cfg, workspace)
        };

        offload_boundary(move || {
            session::check_roots(&cfg, &workspace, target)
                .map_err(|err| BoundaryError::new("invalid_mode_roots", err.to_string()))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        {
            let mut control_guard = self
                .active_control
                .lock()
                .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
            if let Some(control) = control_guard.as_mut() {
                let handle = control.handle.as_ref().ok_or_else(|| {
                    DesktopError::new("run_starting", "Run is still initializing")
                })?;
                handle
                    .request_mode(target)
                    .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

                if matches!(control.pending, Some(PendingInteraction::Approval { .. }))
                    && let Some(PendingInteraction::Approval {
                        request: Request::Approval(_, reply),
                        ..
                    }) = control.pending.take()
                {
                    let _ = reply.send(Err(BoundaryError(
                        ErrorInfo {
                            code: "stale_approval".into(),
                            message: "Work mode changed before approval resolved".into(),
                        },
                        None,
                        None,
                    )));
                }

                let snapshot = handle
                    .mode()
                    .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                return Ok(ModeStateDto {
                    current: snapshot.current.into(),
                    pending: snapshot.pending.map(Into::into),
                    active_run: true,
                });
            }
        }

        let mut guard = self.inner.lock().await;
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Run became active while switching mode; retry",
            ));
        }
        if let Some(prepared) = &mut guard.prepared {
            let mut updated = prepared.session.clone();
            updated.work_mode = target;
            prepared
                .store
                .commit(&[StoreChange::Session(updated.clone())])
                .await
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
            prepared.session = updated;
        }
        if let Some(cfg) = &mut guard.config {
            cfg.mode = target;
        }
        Ok(ModeStateDto {
            current: target.into(),
            pending: None,
            active_run: false,
        })
    }

    pub async fn set_permission_mode(
        &self,
        permissions: PermissionModeDto,
    ) -> Result<PermissionModeDto, DesktopError> {
        {
            let control = self
                .active_control
                .lock()
                .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
            if let Some(active) = control.as_ref() {
                if active.pending.is_some() {
                    return Err(DesktopError::new(
                        "pending_request",
                        "Resolve pending approval or input before changing permission mode",
                    ));
                }
                return Err(DesktopError::new(
                    "run_active",
                    "Permission mode can only be changed while idle",
                ));
            }
        }
        let mut guard = self.inner.lock().await;
        if guard.active_run_id.is_some() {
            return Err(DesktopError::new(
                "run_active",
                "Permission mode can only be changed while idle",
            ));
        }
        let config_err = guard
            .config_error
            .clone()
            .unwrap_or_else(|| "Configuration is not loaded".into());
        let cfg = guard
            .config
            .as_mut()
            .ok_or_else(|| DesktopError::new("invalid_config", config_err))?;
        cfg.permissions_mode = permissions.into();
        Ok(permissions)
    }

    pub async fn resume_session(
        &self,
        session_id: &str,
        accept_config_change: bool,
    ) -> Result<ResumeSessionResultDto, DesktopError> {
        let secrets = self.current_secrets();
        let parsed_id = session::parse_id(session_id).map_err(|err| {
            DesktopError::new(
                "invalid_session_id",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;

        let (cfg, already_loaded) = {
            let mut guard = self.inner.lock().await;
            if guard.active_run_id.is_some() {
                return Err(DesktopError::new(
                    "run_active",
                    "Cancel or wait for the active run before switching sessions",
                ));
            }
            let already = guard
                .prepared
                .as_ref()
                .is_some_and(|p| p.session.id == parsed_id);
            if already && let Some(prepared) = &guard.prepared {
                let sess_mode = prepared.session.work_mode;
                let sess_ws = prepared.session.workspace.clone();
                let sess_profile = prepared.session.profile.clone();
                if let Some(cfg) = &mut guard.config {
                    cfg.mode = sess_mode;
                    cfg.cwd = sess_ws;
                    cfg.profile = Some(sess_profile);
                }
                drop(guard);
                let bootstrap = self.get_bootstrap_state().await;
                return Ok(ResumeSessionResultDto {
                    bootstrap,
                    recovered_runs: 0,
                    recovered_tools: 0,
                });
            }
            let cfg = guard.config.clone().ok_or_else(|| {
                DesktopError::new(
                    "invalid_config",
                    guard
                        .config_error
                        .as_deref()
                        .unwrap_or("Configuration is not loaded"),
                )
            })?;
            (cfg, already)
        };
        let _ = already_loaded;

        let prepared = session::resume(
            &cfg,
            session_id,
            session::ResumeOptions {
                accept_config_change,
                cwd_explicit: false,
                profile_explicit: false,
                mode_override: None,
            },
        )
        .await
        .map_err(|err| {
            let msg = redaction::redact_str(&err.to_string(), &secrets);
            let code = if msg.contains("configuration changed") {
                "config_change_requires_confirmation"
            } else {
                "resume_failed"
            };
            DesktopError::new(code, msg)
        })?;

        let recovered_runs = prepared.recovery.interrupted_runs as u32;
        let recovered_tools =
            (prepared.recovery.unknown_tools + prepared.recovery.closed_calls) as u32;

        {
            let mut guard = self.inner.lock().await;
            if let Some(cfg) = &mut guard.config {
                cfg.cwd = prepared.session.workspace.clone();
                cfg.mode = prepared.session.work_mode;
                cfg.profile = Some(prepared.session.profile.clone());
            }
            guard.last_usage = None;
            guard.prepared = Some(prepared);
        }

        let bootstrap = self.get_bootstrap_state().await;
        Ok(ResumeSessionResultDto {
            bootstrap,
            recovered_runs,
            recovered_tools,
        })
    }

    pub async fn shutdown(&self) {
        let _ = self.cancel_run();
        for _ in 0..50 {
            let is_active = self
                .active_control
                .lock()
                .map(|g| g.is_some())
                .unwrap_or(false);
            if !is_active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn require_config(&self) -> Result<Config, DesktopError> {
        let guard = self.inner.lock().await;
        guard.config.clone().ok_or_else(|| {
            DesktopError::new(
                "invalid_config",
                guard
                    .config_error
                    .as_deref()
                    .unwrap_or("Configuration is not loaded"),
            )
        })
    }
}

struct DriverParams {
    run_id: RunId,
    model_display: String,
    started_at: Instant,
    sink: StreamSink,
    cancel: Arc<RunCancellation>,
    handle: RuntimeHandle,
    runtime_join: tokio::task::JoinHandle<(Result<RunResult, BoundaryError>, Session)>,
    event_rx: mpsc::Receiver<Event>,
    context_rx: watch::Receiver<Option<ContextSnapshot>>,
    thinking_rx: watch::Receiver<ThinkingLog>,
    request_rx: mpsc::Receiver<Request>,
    measurements: Arc<StdMutex<Measurements>>,
    inner: Arc<AsyncMutex<DesktopInner>>,
    active_control: Arc<StdMutex<Option<ActiveControl>>>,
    secrets: Arc<SecretRegistry>,
}

async fn drive_active_run(mut params: DriverParams) {
    let mut events_open = true;
    let mut requests_open = true;
    let mut thinking_open = true;
    let mut context_open = true;
    let mut tool_tracker: HashMap<String, (String, Option<Value>, Instant)> = HashMap::new();
    let mut consumer_disconnected = false;
    let mut runtime_join = params.runtime_join;

    let run_outcome = loop {
        tokio::select! {
            biased;
            maybe_req = params.request_rx.recv(), if requests_open => {
                match maybe_req {
                    Some(req) => {
                        let secrets = params.secrets.snapshot().unwrap_or_default();
                        let envelope = match &req {
                            Request::Approval(approval, _) => {
                                let formatted_call = redaction::redact_str(
                                    &tool_format::call(&approval.tool, &approval.arguments, None),
                                    &secrets,
                                );
                                let action = tool_format::approval_action(&approval.tool, &approval.arguments)
                                    .map(|a| redaction::redact_str(&a, &secrets));
                                DesktopStreamEnvelope::ApprovalNeeded(ApprovalRequestDto {
                                    approval_id: approval.approval_id.clone(),
                                    call_id: approval.call_id.clone(),
                                    run_id: approval.run_id.0.to_string(),
                                    tool: redaction::redact_str(&approval.tool, &secrets),
                                    operation_summary: redaction::redact_str(&approval.operation_summary, &secrets),
                                    arguments: redaction::redact_value(&approval.arguments, &secrets),
                                    formatted_call,
                                    action,
                                    cwd: redaction::redact_str(&approval.cwd.to_string_lossy(), &secrets),
                                    work_mode: approval.work_mode.into(),
                                })
                            }
                            Request::Input(input, _) => {
                                let input_id = format!("input_{}", params.run_id.0.simple());
                                DesktopStreamEnvelope::InputNeeded(InputRequestDto {
                                    input_id,
                                    question: redaction::redact_str(&input.question, &secrets),
                                    options: input
                                        .options
                                        .iter()
                                        .map(|o| redaction::redact_str(o, &secrets))
                                        .collect(),
                                })
                            }
                        };
                        if let Ok(mut control) = params.active_control.lock()
                            && let Some(active) = control.as_mut()
                        {
                            active.pending = Some(match &envelope {
                                DesktopStreamEnvelope::ApprovalNeeded(dto) => PendingInteraction::Approval {
                                    approval_id: dto.approval_id.clone(),
                                    call_id: dto.call_id.clone(),
                                    request: req,
                                },
                                DesktopStreamEnvelope::InputNeeded(dto) => PendingInteraction::Input {
                                    input_id: dto.input_id.clone(),
                                    request: req,
                                },
                                _ => unreachable!(),
                            });
                        }
                        if params.sink.send(envelope).is_err() {
                            consumer_disconnected = true;
                            params.cancel.cancel();
                        }
                    }
                    None => {
                        requests_open = false;
                    }
                }
            }
            maybe_event = params.event_rx.recv(), if events_open => {
                match maybe_event {
                    Some(event) => {
                        let secrets = params.secrets.snapshot().unwrap_or_default();
                        let dto = convert_event(event, &mut tool_tracker, &secrets);
                        if params.sink.send(DesktopStreamEnvelope::SchemaEvent(dto)).is_err() {
                            consumer_disconnected = true;
                            params.cancel.cancel();
                        }
                    }
                    None => {
                        events_open = false;
                    }
                }
            }
            changed = params.thinking_rx.changed(), if thinking_open => {
                match changed {
                    Ok(()) => {
                        let log = params.thinking_rx.borrow_and_update().clone();
                        let secrets = params.secrets.snapshot().unwrap_or_default();
                        let entries = redact_thinking(log, &secrets);
                        if params.sink.send(DesktopStreamEnvelope::Thinking(entries)).is_err() {
                            consumer_disconnected = true;
                            params.cancel.cancel();
                        }
                    }
                    Err(_) => {
                        thinking_open = false;
                    }
                }
            }
            changed = params.context_rx.changed(), if context_open => {
                match changed {
                    Ok(()) => {
                        if let Some(snapshot) = params.context_rx.borrow_and_update().clone()
                            && params.sink.send(convert_context(snapshot)).is_err()
                        {
                            consumer_disconnected = true;
                            params.cancel.cancel();
                        }
                    }
                    Err(_) => {
                        context_open = false;
                    }
                }
            }
            join_res = &mut runtime_join => {
                break join_res;
            }
        }
    };

    // Drain any events/thinking/context produced right before completion.
    while let Ok(event) = params.event_rx.try_recv() {
        if !consumer_disconnected {
            let secrets = params.secrets.snapshot().unwrap_or_default();
            let dto = convert_event(event, &mut tool_tracker, &secrets);
            if params
                .sink
                .send(DesktopStreamEnvelope::SchemaEvent(dto))
                .is_err()
            {
                consumer_disconnected = true;
            }
        }
    }
    if !consumer_disconnected {
        let log = params.thinking_rx.borrow_and_update().clone();
        if !log.entries.is_empty() {
            let secrets = params.secrets.snapshot().unwrap_or_default();
            let _ = params
                .sink
                .send(DesktopStreamEnvelope::Thinking(redact_thinking(
                    log, &secrets,
                )));
        }
        if let Some(snapshot) = params.context_rx.borrow_and_update().clone() {
            let _ = params.sink.send(convert_context(snapshot));
        }
    }

    params.event_rx.close();
    params.request_rx.close();
    let cleanup_res = params.handle.shutdown_background().await;

    let secrets = params.secrets.snapshot().unwrap_or_default();
    let (status_str, usage_opt, todos, final_error) = match run_outcome {
        Ok((outcome, updated_session)) => {
            let todos = current_todos(&updated_session.history)
                .into_iter()
                .map(|item| TodoItemDto::from_redacted(item, &secrets))
                .collect::<Vec<_>>();
            let (status_str, usage_opt, mut final_error) = match outcome {
                Ok(res) => (
                    format!("{:?}", res.status).to_lowercase(),
                    res.usage.clone(),
                    res.error.as_ref().map(|e| {
                        DesktopError::new(&e.code, redaction::redact_str(&e.message, &secrets))
                    }),
                ),
                Err(err) => (
                    "failed".into(),
                    None,
                    Some(DesktopError::from_boundary(&err, &secrets)),
                ),
            };
            if let Err(cleanup_err) = cleanup_res
                && final_error.is_none()
            {
                final_error = Some(DesktopError::from_boundary(&cleanup_err, &secrets));
            }
            let mut inner = params.inner.lock().await;
            if let Some(cfg) = &mut inner.config {
                cfg.mode = updated_session.work_mode;
            }
            if let Some(prepared) = &mut inner.prepared {
                prepared.session = updated_session;
            }
            if let Some(usage) = &usage_opt {
                inner.last_usage = Some(usage.clone());
            }
            inner.active_run_id = None;
            (status_str, usage_opt, todos, final_error)
        }
        Err(_) => {
            let mut inner = params.inner.lock().await;
            inner.active_run_id = None;
            (
                "failed".into(),
                None,
                Vec::new(),
                Some(DesktopError::new(
                    "run_task_failed",
                    "Runtime task terminated unexpectedly",
                )),
            )
        }
    };

    if let Ok(mut control) = params.active_control.lock() {
        *control = None;
    }

    if !consumer_disconnected {
        let total_elapsed = Instant::now().saturating_duration_since(params.started_at);
        let summary_line = params
            .measurements
            .lock()
            .map(|m| m.summary_line(&params.model_display, total_elapsed, usage_opt.as_ref()))
            .unwrap_or_else(|_| {
                format!(
                    "{} · {}",
                    params.model_display,
                    format_elapsed(total_elapsed)
                )
            });

        let _ = params.sink.send(DesktopStreamEnvelope::RunCompleted {
            status: status_str,
            summary_line: redaction::redact_str(&summary_line, &secrets),
            todos,
            error: final_error,
        });
    }
}

fn convert_context(snapshot: ContextSnapshot) -> DesktopStreamEnvelope {
    DesktopStreamEnvelope::Context {
        entries: snapshot
            .entries
            .into_iter()
            .map(|entry| ContextEntryDto {
                label: entry.label.into(),
                tokens: entry.tokens,
            })
            .collect(),
        skills_count: snapshot.skills_count.map(|c| c as u32),
        estimated: true,
    }
}

fn redact_thinking(log: ThinkingLog, secrets: &[String]) -> Vec<ThinkingSnapshotDto> {
    log.entries
        .into_iter()
        .map(|entry| ThinkingSnapshotDto {
            request_id: redaction::redact_str(&entry.request_id, secrets),
            text: redaction::redact_str(&entry.text, secrets),
            active: entry.active,
            truncated: entry.truncated,
            available: entry.available,
        })
        .collect()
}

fn convert_event(
    event: Event,
    tool_tracker: &mut HashMap<String, (String, Option<Value>, Instant)>,
    secrets: &[String],
) -> SchemaEnvelopeDto {
    let payload = match event.payload {
        EventPayload::SessionCreated {
            work_mode,
            permissions,
            workspace,
            profile,
        } => SchemaEventDto::SessionCreated {
            work_mode: work_mode.into(),
            permissions: permissions.into(),
            workspace: redaction::redact_str(&workspace.to_string_lossy(), secrets),
            profile: redaction::redact_str(&profile, secrets),
        },
        EventPayload::SessionResumed {
            work_mode,
            permissions,
            workspace,
            profile,
        } => SchemaEventDto::SessionResumed {
            work_mode: work_mode.into(),
            permissions: permissions.into(),
            workspace: redaction::redact_str(&workspace.to_string_lossy(), secrets),
            profile: redaction::redact_str(&profile, secrets),
        },
        EventPayload::RunStarted {
            work_mode,
            permissions,
            profile,
        } => SchemaEventDto::RunStarted {
            work_mode: work_mode.into(),
            permissions: permissions.into(),
            profile: redaction::redact_str(&profile, secrets),
        },
        EventPayload::ModeChangeRequested { from, to } => SchemaEventDto::ModeChangeRequested {
            from: from.into(),
            to: to.into(),
        },
        EventPayload::ModeChanged { from, to } => SchemaEventDto::ModeChanged {
            from: from.into(),
            to: to.into(),
        },
        EventPayload::ModelStarted {
            request_id,
            purpose,
            profile,
        } => SchemaEventDto::ModelStarted {
            request_id: redaction::redact_str(&request_id, secrets),
            purpose: purpose.into(),
            profile: redaction::redact_str(&profile, secrets),
        },
        EventPayload::ModelTextDelta { request_id, text } => SchemaEventDto::ModelTextDelta {
            request_id: redaction::redact_str(&request_id, secrets),
            text: redaction::redact_str(&text, secrets),
        },
        EventPayload::ModelFinished {
            request_id,
            purpose,
            ok,
            usage,
            error,
        } => SchemaEventDto::ModelFinished {
            request_id: redaction::redact_str(&request_id, secrets),
            purpose: purpose.into(),
            ok,
            usage: usage.map(Into::into),
            error: error
                .as_ref()
                .map(|err| crate::types::ErrorInfoDto::from_redacted(err, secrets)),
        },
        EventPayload::ToolRequested {
            call_id,
            name,
            args,
        } => {
            let safe_args = redaction::redact_value(&args, secrets);
            let safe_name = redaction::redact_str(&name, secrets);
            let summary = tool_format::summarize_call(&safe_name, Some(&safe_args), None, None)
                .map(|s| redaction::redact_str(&s, secrets));
            let formatted =
                redaction::redact_str(&tool_format::call(&safe_name, &safe_args, None), secrets);
            tool_tracker.insert(
                call_id.clone(),
                (safe_name.clone(), Some(safe_args.clone()), Instant::now()),
            );
            SchemaEventDto::ToolRequested {
                call_id,
                name: safe_name,
                args: safe_args,
                summary,
                formatted,
            }
        }
        EventPayload::PermissionReviewed {
            call_id,
            decision,
            source,
            reason,
        } => SchemaEventDto::PermissionReviewed {
            call_id,
            decision: redaction::redact_str(&decision, secrets),
            source: source.into(),
            reason: redaction::redact_str(&reason, secrets),
        },
        EventPayload::PermissionRequested {
            approval_id,
            call_id,
            operation_summary,
            work_mode,
        } => SchemaEventDto::PermissionRequested {
            approval_id,
            call_id,
            operation_summary: redaction::redact_str(&operation_summary, secrets),
            work_mode: work_mode.into(),
        },
        EventPayload::PermissionResolved {
            approval_id,
            call_id,
            decision,
            source,
        } => SchemaEventDto::PermissionResolved {
            approval_id,
            call_id,
            decision: redaction::redact_str(&decision, secrets),
            source: source.into(),
        },
        EventPayload::ToolStarted { call_id, name } => {
            let safe_name = redaction::redact_str(&name, secrets);
            tool_tracker
                .entry(call_id.clone())
                .or_insert_with(|| (safe_name.clone(), None, Instant::now()));
            SchemaEventDto::ToolStarted {
                call_id,
                name: safe_name,
            }
        }
        EventPayload::ToolFinished {
            call_id,
            name,
            result,
        } => {
            let safe_name = redaction::redact_str(&name, secrets);
            let tracked = tool_tracker.remove(&call_id);
            let args = tracked.as_ref().and_then(|(_, a, _)| a.as_ref());
            let elapsed_ms = tracked
                .as_ref()
                .map(|(_, _, started)| started.elapsed().as_millis() as u64);
            let result_dto =
                ToolResultDto::from_redacted(&safe_name, args, &result, elapsed_ms, secrets);
            SchemaEventDto::ToolFinished {
                call_id,
                name: safe_name,
                result: result_dto,
            }
        }
        EventPayload::InputRequested {
            input_id,
            question,
            options,
        } => SchemaEventDto::InputRequested {
            input_id,
            question: redaction::redact_str(&question, secrets),
            options: options
                .into_iter()
                .map(|o| redaction::redact_str(&o, secrets))
                .collect(),
        },
        EventPayload::InputReceived { input_id, answer } => SchemaEventDto::InputReceived {
            input_id,
            answer: redaction::redact_str(&answer, secrets),
        },
        EventPayload::McpStatus { servers } => SchemaEventDto::McpStatus {
            servers: servers
                .into_iter()
                .map(|s| McpStatusBadgeDto {
                    server: redaction::redact_str(&s.server, secrets),
                    transport: format!("{:?}", s.transport).to_lowercase(),
                    state: format!("{:?}", s.state).to_lowercase(),
                    tools: s.tools as u32,
                })
                .collect(),
        },
        EventPayload::RunFinished(result) => {
            SchemaEventDto::RunFinished(RunResultDto::from_redacted(&result, secrets))
        }
    };

    SchemaEnvelopeDto {
        schema_version: event.schema_version,
        session_id: event.session_id.0.to_string(),
        run_id: event.run_id.map(|id| id.0.to_string()),
        seq: event.seq,
        timestamp: event.timestamp,
        payload,
    }
}

pub fn build_active_session_dto(
    session: &Session,
    last_usage: Option<Usage>,
    secrets: &[String],
) -> ActiveSessionDto {
    let title = session
        .history
        .iter()
        .find_map(|item| match item {
            HistoryItem::UserText(text) => Some(media::display(text)),
            _ => None,
        })
        .map(|text| redaction::safe_title(&text, secrets))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "New session".into());

    let todos = current_todos(&session.history)
        .into_iter()
        .map(|item| TodoItemDto::from_redacted(item, secrets))
        .collect();

    let messages = restore_messages(&session.history, &session.model, secrets);

    ActiveSessionDto {
        id: session.id.0.to_string(),
        title,
        workspace: redaction::redact_str(&session.workspace.to_string_lossy(), secrets),
        work_mode: session.work_mode.into(),
        profile: redaction::redact_str(&session.profile, secrets),
        model: redaction::redact_str(&session.model, secrets),
        messages,
        todos,
        history_omitted: session.history.len() > MAX_RESTORED_HISTORY,
        last_usage: last_usage.map(Into::into),
    }
}

fn restore_messages(
    history: &[HistoryItem],
    model: &str,
    secrets: &[String],
) -> Vec<RestoredMessageDto> {
    let mut out = Vec::new();
    let visible_start = history.len().saturating_sub(MAX_RESTORED_HISTORY);
    let mut index = 0usize;
    let mut seq = 0u64;
    let mut tools: HashMap<String, (usize, String, Value)> = HashMap::new();
    let safe_model = redaction::redact_str(model, secrets);

    while index < history.len() {
        if matches!(
            &history[index],
            HistoryItem::UserText(_) | HistoryItem::ToolResult { .. }
        ) {
            if index >= visible_start {
                push_restored_item(
                    &history[index],
                    &safe_model,
                    secrets,
                    &mut seq,
                    &mut tools,
                    &mut out,
                );
            }
            index += 1;
            continue;
        }
        let end = history[index..]
            .iter()
            .position(|item| {
                matches!(
                    item,
                    HistoryItem::UserText(_) | HistoryItem::ToolResult { .. }
                )
            })
            .map_or(history.len(), |offset| index + offset);
        if end > visible_start {
            let thought = extract_reasoning(&history[index..end]);
            if !thought.is_empty() {
                seq += 1;
                out.push(RestoredMessageDto {
                    id: format!("restored-{seq}"),
                    role: MessageRoleDto::Thinking,
                    label: "Provider Thought".into(),
                    text: redaction::redact_str(&thought, secrets),
                    streaming: false,
                    truncated: false,
                    tool_call_id: None,
                    tool_name: None,
                    tool_summary: None,
                    tool_ok: None,
                    media_refs: Vec::new(),
                });
            }
            for item in &history[index.max(visible_start)..end] {
                push_restored_item(item, &safe_model, secrets, &mut seq, &mut tools, &mut out);
            }
        }
        index = end;
    }

    out
}

fn push_restored_item(
    item: &HistoryItem,
    safe_model: &str,
    secrets: &[String],
    seq: &mut u64,
    tools: &mut HashMap<String, (usize, String, Value)>,
    out: &mut Vec<RestoredMessageDto>,
) {
    match item {
        HistoryItem::UserText(text) => {
            *seq += 1;
            out.push(RestoredMessageDto {
                id: format!("restored-{seq}"),
                role: MessageRoleDto::User,
                label: "You".into(),
                text: redaction::redact_str(&media::display(text), secrets),
                streaming: false,
                truncated: false,
                tool_call_id: None,
                tool_name: None,
                tool_summary: None,
                tool_ok: None,
                media_refs: Vec::new(),
            });
        }
        HistoryItem::AssistantText(text) if !text.is_empty() => {
            *seq += 1;
            out.push(RestoredMessageDto {
                id: format!("restored-{seq}"),
                role: MessageRoleDto::Assistant,
                label: safe_model.to_owned(),
                text: redaction::redact_str(text, secrets),
                streaming: false,
                truncated: false,
                tool_call_id: None,
                tool_name: None,
                tool_summary: None,
                tool_ok: None,
                media_refs: Vec::new(),
            });
        }
        HistoryItem::AssistantPartial(text) if !text.is_empty() => {
            *seq += 1;
            out.push(RestoredMessageDto {
                id: format!("restored-{seq}"),
                role: MessageRoleDto::Assistant,
                label: "Interrupted partial".into(),
                text: redaction::redact_str(text, secrets),
                streaming: false,
                truncated: true,
                tool_call_id: None,
                tool_name: None,
                tool_summary: None,
                tool_ok: None,
                media_refs: Vec::new(),
            });
        }
        HistoryItem::ToolCall(call) => {
            let args = serde_json::from_str::<Value>(&call.arguments)
                .unwrap_or_else(|_| Value::String(call.arguments.clone()));
            let safe_args = redaction::redact_value(&args, secrets);
            let safe_name = redaction::redact_str(&call.name, secrets);
            let formatted =
                redaction::redact_str(&tool_format::call(&safe_name, &safe_args, None), secrets);
            let summary = tool_format::summarize_call(&safe_name, Some(&safe_args), None, None)
                .map(|s| redaction::redact_str(&s, secrets));
            *seq += 1;
            let idx = out.len();
            out.push(RestoredMessageDto {
                id: format!("restored-{seq}"),
                role: MessageRoleDto::Tool,
                label: format!("{safe_name} · {}", call.call_id),
                text: formatted,
                streaming: false,
                truncated: false,
                tool_call_id: Some(call.call_id.clone()),
                tool_name: Some(safe_name.clone()),
                tool_summary: summary,
                tool_ok: None,
                media_refs: Vec::new(),
            });
            tools.insert(call.call_id.clone(), (idx, safe_name, safe_args));
        }
        HistoryItem::ToolResult { call_id, result } => {
            let _ = result_todos(result);
            if let Some((idx, name, args)) = tools.remove(call_id) {
                let dto = ToolResultDto::from_redacted(&name, Some(&args), result, None, secrets);
                if let Some(msg) = out.get_mut(idx) {
                    msg.label = format!(
                        "{name} · {call_id} · {}",
                        if dto.ok { "completed" } else { "failed" }
                    );
                    msg.text = dto.formatted;
                    msg.truncated = dto.truncated;
                    msg.tool_summary = dto.summary;
                    msg.tool_ok = Some(dto.ok);
                    msg.media_refs = dto.media_refs;
                }
            } else {
                let dto = ToolResultDto::from_redacted("tool", None, result, None, secrets);
                *seq += 1;
                out.push(RestoredMessageDto {
                    id: format!("restored-{seq}"),
                    role: MessageRoleDto::Tool,
                    label: format!(
                        "Result · {call_id} · {}",
                        if dto.ok { "completed" } else { "failed" }
                    ),
                    text: dto.formatted,
                    streaming: false,
                    truncated: dto.truncated,
                    tool_call_id: Some(call_id.clone()),
                    tool_name: None,
                    tool_summary: dto.summary,
                    tool_ok: Some(dto.ok),
                    media_refs: dto.media_refs,
                });
            }
        }
        _ => {}
    }
}

pub fn expand_attachments(text: &str, attachments: &[AttachmentTokenDto]) -> String {
    if attachments.is_empty() {
        return text.to_owned();
    }
    let mut out = text.to_owned();
    for item in attachments {
        if item.token.is_empty() || item.path.is_empty() {
            continue;
        }
        let replacement = format!("{}(<{}>)", item.token, item.path);
        out = out.replace(&item.token, &replacement);
    }
    out
}

pub fn register_config_secrets(cfg: &Config, registry: &SecretRegistry) {
    let values = cfg
        .secret_env_names()
        .iter()
        .filter_map(|name| cfg.secret(name).ok().map(str::to_owned))
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>();
    let _ = registry.register(&values);
}

fn redact_summary(summary: SessionSummary, secrets: &[String]) -> SessionItemDto {
    SessionItemDto {
        id: summary.id.0.to_string(),
        workspace: redaction::redact_str(&summary.workspace.to_string_lossy(), secrets),
        work_mode: summary.work_mode.into(),
        profile: redaction::redact_str(&summary.profile, secrets),
        title: summary.title.map(|t| redaction::redact_str(&t, secrets)),
        updated_at: summary.updated_at,
        last_status: summary
            .last_status
            .map(|status| format!("{status:?}").to_lowercase()),
    }
}

pub fn sandbox_label(backend: BackendKind) -> &'static str {
    match backend {
        BackendKind::Auto => "native",
        BackendKind::Seatbelt => "seatbelt",
        BackendKind::Bubblewrap => "bubblewrap",
        BackendKind::Host => "host",
    }
}

pub fn offload_blocking_sync<F, T>(f: F) -> Result<T, BoundaryError>
where
    F: FnOnce() -> Result<T, BoundaryError>,
{
    f()
}

pub async fn run_blocking_boundary<F, T>(f: F) -> Result<T, BoundaryError>
where
    F: FnOnce() -> Result<T, BoundaryError> + Send + 'static,
    T: Send + 'static,
{
    offload_boundary(f).await
}

pub async fn run_blocking<F, T>(f: F) -> Result<T, Box<dyn std::error::Error>>
where
    F: FnOnce() -> Result<T, Box<dyn std::error::Error>> + Send + 'static,
    T: Send + 'static,
{
    offload(f).await
}
