//! Runtime service bridging helper-server requests and `soyo-app` / `soyo-core`.

use crate::attachment::{ActiveAttachmentScope, StagedDraft};
use serde_json::Value;
use soyo_app::{
    context::{ContextModel, ContextSnapshot, ContextSources},
    events::{ChannelSink, EventModel, EventSink, EventStore, EventWriter},
    interaction::{Request, RunCancellation, input_channel},
    memory_admin::PreparedMemoryAction,
    offload, offload_boundary,
    session::{self, Prepared},
    thinking::{ThinkingLog, ThinkingModel, extract_reasoning},
};
use soyo_core::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, StoreChange,
    },
    config::{self, Config, Environment, Output, Overrides},
    contract::{
        BoundaryError, ErrorInfo, Event, EventPayload, HistoryItem, ModelPurpose, PermissionMode,
        Persistence, RunId, RunResult, RunStatus, SCHEMA_VERSION, Session, SessionId, Usage,
        WorkMode,
    },
    credentials::SecretRegistry,
    execution::BackendKind,
    logging::{JsonLog, LogLevel, diagnostics::Diagnostics},
    media,
    runtime::{PersonalContext, RuntimeHandle},
    sqlite::SessionSummary,
    tools::todo::{current_todos, result_todos},
};
use soyo_ipc::{
    protocol::PROTOCOL_VERSION,
    redaction,
    socket::HelperTarget,
    tool_format,
    types::{
        ActiveSessionDto, ApprovalRequestDto, AttachmentTokenDto, BootstrapStateDto,
        ContextEntryDto, DesktopError, DesktopStreamEnvelope, InputRequestDto, McpStatusBadgeDto,
        MessageRoleDto, ModeStateDto, PermissionModeDto, ProfileOptionDto, RestoredMessageDto,
        ResumeSessionDto, ResumeSessionResultDto, RunPromptParams, RunResultDto, RunStateDto,
        SchemaEnvelopeDto, SchemaEventDto, SessionItemDto, SessionPageDto, SubmitPromptParams,
        SubmitPromptResultDto, ThinkingSnapshotDto, TodoItemDto, ToolResultDto, UsageDayDto,
        UsagePurposeDto, UsageStatsDto, UsageTotalsDto, WorkModeDto,
    },
};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};

const SESSIONS_PAGE_SIZE: u64 = 20;
const MAX_RESTORED_HISTORY: usize = 512;
pub const MAX_CONCURRENT_RUNS: usize = 4;
const MAX_IDLE_SESSION_SLOTS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Execution,
    Compaction,
}

/// Per-run knobs carried by `start_run_ex`. Desktop submit/compact paths keep
/// the interactive defaults; `run_prompt` (the socket-backed `soyo run`)
/// supplies overrides, headless interaction, and raw event passthrough.
pub struct StartRunRequest {
    pub expanded_prompt: Option<String>,
    pub kind: RunKind,
    /// Wire approval/input handlers. Headless runs leave them `None` so the
    /// runtime ends `needs_confirmation`/`needs_input` like the in-process CLI.
    pub interactive: bool,
    pub approvals: Preauthorizations,
    pub personal_context: PersonalContext,
    pub anthropic_prompt_cache: bool,
    /// Emit `RawSchemaEvent` passthrough instead of `SchemaEvent` DTOs.
    pub raw_events: bool,
    /// Run media capture on the prompt before appending it (`soyo chat`
    /// parity); the runtime summary keeps the original text.
    pub capture_media: bool,
    /// Emit the synthetic `session.created`/`session.resumed` line the CLI
    /// event writer emits before the run starts.
    pub emit_session_event: bool,
    /// Per-run config with client overrides applied; the shared daemon config
    /// is left untouched.
    pub config: Option<Config>,
    /// Whether the prepared session came from a resume (`session.resumed`
    /// vs `session.created` for `emit_session_event`).
    pub resumed: bool,
    /// Reconcile unstreamed suffix / reordered text before `Finished`
    /// (`PrintStream` parity for `--output text`).
    pub reconcile_stream_text: bool,
    /// Optional per-run diagnostic log threshold (`run_prompt` CLI parity).
    pub log_level: Option<LogLevel>,
}

impl StartRunRequest {
    fn desktop(expanded_prompt: Option<String>, kind: RunKind) -> Self {
        Self {
            expanded_prompt,
            kind,
            interactive: true,
            approvals: Preauthorizations::default(),
            personal_context: PersonalContext::default(),
            anthropic_prompt_cache: false,
            raw_events: false,
            capture_media: false,
            emit_session_event: false,
            config: None,
            resumed: false,
            reconcile_stream_text: false,
            log_level: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StreamSendError;

#[derive(Clone)]
pub enum StreamSink {
    Mpsc(mpsc::UnboundedSender<DesktopStreamEnvelope>),
    Fn(Arc<dyn Fn(DesktopStreamEnvelope) -> Result<(), StreamSendError> + Send + Sync>),
}

impl StreamSink {
    pub fn send(&self, envelope: DesktopStreamEnvelope) -> Result<(), StreamSendError> {
        match self {
            Self::Mpsc(tx) => tx.send(envelope).map_err(|_| StreamSendError),
            Self::Fn(f) => f(envelope),
        }
    }
}

pub type TestModelFactory = Arc<dyn Fn() -> Arc<dyn Model> + Send + Sync>;

/// Connection and frame identity of the client owning an interactive run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClientOwner {
    pub conn_id: u64,
    pub request_id: u64,
}

pub struct ActiveControl {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub owner_conn_id: u64,
    pub owner_request_id: u64,
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

pub struct SessionSlot {
    pub prepared: Prepared,
    pub active_run_id: Option<RunId>,
    pub last_usage: Option<Usage>,
    pub owner_conn_id: Option<u64>,
    pub permissions_mode: PermissionMode,
    /// Ephemeral one-shot slots (`run_prompt` / `soyo run`) drop their prepared
    /// SQLite handle and lock as soon as the run finishes so CLI resume / crash
    /// recovery commands don't observe `session_busy`.
    pub release_after_run: bool,
}

pub struct DesktopInner {
    pub config: Option<Config>,
    pub config_error: Option<String>,
    pub target: HelperTarget,
    pub sessions: HashMap<SessionId, SessionSlot>,
    pub connection_sessions: HashMap<u64, SessionId>,
}

impl DesktopInner {
    pub fn release_ephemeral_slot(&mut self, sid: SessionId) {
        if self
            .sessions
            .get(&sid)
            .is_some_and(|slot| slot.release_after_run && slot.active_run_id.is_none())
        {
            self.sessions.remove(&sid);
            self.connection_sessions
                .retain(|_, &mut bound| bound != sid);
        }
    }
    pub fn resolve_session_id(&self, explicit: Option<&str>, conn_id: u64) -> Option<SessionId> {
        if let Some(raw) = explicit {
            return session::parse_id(raw).ok();
        }
        if let Some(&sid) = self.connection_sessions.get(&conn_id)
            && self.sessions.contains_key(&sid)
        {
            return Some(sid);
        }
        if conn_id == 0 && self.sessions.len() == 1 {
            return self.sessions.keys().next().copied();
        }
        None
    }

    pub fn evict_idle_sessions(&mut self) {
        if self.sessions.len() <= MAX_IDLE_SESSION_SLOTS {
            return;
        }
        let pinned: std::collections::HashSet<SessionId> =
            self.connection_sessions.values().copied().collect();
        let evict_ids: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(sid, slot)| slot.active_run_id.is_none() && !pinned.contains(sid))
            .map(|(sid, _)| *sid)
            .take(self.sessions.len() - MAX_IDLE_SESSION_SLOTS)
            .collect();
        for sid in evict_ids {
            self.sessions.remove(&sid);
        }
    }
}

pub struct HostService {
    pub inner: Arc<AsyncMutex<DesktopInner>>,
    pub active_controls: Arc<StdMutex<HashMap<RunId, ActiveControl>>>,
    pub active_clients: Arc<AtomicUsize>,
    pub attachment_scope: Arc<StdMutex<ActiveAttachmentScope>>,
    pub staged_drafts: Arc<StdMutex<HashMap<String, StagedDraft>>>,
    pub pending_memory_actions: Arc<StdMutex<HashMap<String, PreparedMemoryAction>>>,
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

fn fold_usage(totals: &mut UsageTotalsDto, usage: &Usage) {
    totals.input_tokens = totals
        .input_tokens
        .saturating_add(usage.input_tokens.unwrap_or(0));
    totals.output_tokens = totals
        .output_tokens
        .saturating_add(usage.output_tokens.unwrap_or(0));
    totals.cached_input_tokens = totals
        .cached_input_tokens
        .saturating_add(usage.cached_input_tokens.unwrap_or(0));
    totals.estimated |= usage.estimated;
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

impl HostService {
    pub fn from_environment() -> Self {
        Self::from_overrides(None, None)
    }

    pub fn from_overrides(config_path: Option<&Path>, data_dir: Option<&Path>) -> Self {
        let raw_env = Environment::capture();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let target = HelperTarget::from_overrides(config_path, data_dir, &raw_env, &cwd)
            .unwrap_or_else(|_| {
                HelperTarget::from_paths(
                    PathBuf::from("config.toml"),
                    PathBuf::from("data"),
                    PathBuf::from("state"),
                )
            });
        crate::envfile::migrate_legacy_env(&target.state_dir);
        let overlay = crate::envfile::load_env_pairs_for(&target.state_dir);
        let secret_values = crate::envfile::load_env_secret_values_for(&target.state_dir);
        let env = Environment::capture_with(overlay);
        let overrides = Overrides {
            config: config_path.map(Path::to_path_buf),
            data_dir: data_dir.map(Path::to_path_buf),
            ..Default::default()
        };
        let resolved_config_path = config::config_path(&overrides, &env)
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        match config::load(overrides, env, &cwd) {
            Ok(cfg) => {
                let svc = Self::with_config(cfg);
                let _ = svc.secrets.register(&secret_values);
                svc
            }
            Err(err) => {
                let secrets = Arc::new(SecretRegistry::default());
                let _ = secrets.register(&secret_values);
                Self {
                    inner: Arc::new(AsyncMutex::new(DesktopInner {
                        config: None,
                        config_error: Some(format!(
                            "{}{}",
                            err.message(),
                            resolved_config_path
                                .map(|p| format!(" ({p})"))
                                .unwrap_or_default()
                        )),
                        target,
                        sessions: HashMap::new(),
                        connection_sessions: HashMap::new(),
                    })),
                    active_controls: Arc::new(StdMutex::new(HashMap::new())),
                    active_clients: Arc::new(AtomicUsize::new(0)),
                    attachment_scope: Arc::new(StdMutex::new(HashMap::new())),
                    staged_drafts: Arc::new(StdMutex::new(HashMap::new())),
                    pending_memory_actions: Arc::new(StdMutex::new(HashMap::new())),
                    secrets,
                    test_model_factory: None,
                }
            }
        }
    }

    pub fn with_config(mut cfg: Config) -> Self {
        if cfg.profile.is_none() && cfg.profiles.len() == 1 {
            cfg.profile = cfg.profiles.keys().next().cloned();
        }
        let target = HelperTarget::from_config(&cfg);
        let registry = Arc::new(SecretRegistry::default());
        let _ = registry.register(&crate::envfile::load_env_secret_values_for(&cfg.state_dir));
        register_config_secrets(&cfg, &registry);
        Self {
            inner: Arc::new(AsyncMutex::new(DesktopInner {
                config: Some(cfg),
                config_error: None,
                target,
                sessions: HashMap::new(),
                connection_sessions: HashMap::new(),
            })),
            active_controls: Arc::new(StdMutex::new(HashMap::new())),
            active_clients: Arc::new(AtomicUsize::new(0)),
            attachment_scope: Arc::new(StdMutex::new(HashMap::new())),
            staged_drafts: Arc::new(StdMutex::new(HashMap::new())),
            pending_memory_actions: Arc::new(StdMutex::new(HashMap::new())),
            secrets: registry,
            test_model_factory: None,
        }
    }

    pub fn sync_attachment_scope(&self, prepared: Option<&Prepared>) {
        if let Some(p) = prepared
            && let Ok(mut scope) = self.attachment_scope.lock()
        {
            scope.insert(p.session.id, p.media.clone());
        }
    }

    pub fn remove_attachment_scope(&self, session_id: SessionId) {
        if let Ok(mut scope) = self.attachment_scope.lock() {
            scope.remove(&session_id);
        }
    }

    pub async fn helper_target(&self) -> HelperTarget {
        self.inner.lock().await.target.clone()
    }

    pub fn with_test_model(mut self, factory: TestModelFactory) -> Self {
        self.test_model_factory = Some(factory);
        self
    }

    pub fn current_secrets(&self) -> Vec<String> {
        self.secrets.snapshot().unwrap_or_default()
    }

    /// Identity and compatibility metadata for the running helper daemon.
    pub async fn helper_info(&self) -> soyo_ipc::types::HelperInfoDto {
        let active_runs = self
            .active_controls
            .lock()
            .map(|controls| controls.len())
            .unwrap_or(0);
        let active_clients = self.active_clients.load(Ordering::Acquire);
        let fingerprint = self.inner.lock().await.target.fingerprint.clone();
        soyo_ipc::types::HelperInfoDto {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            exe_path: std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            pid: std::process::id(),
            run_active: active_runs > 0,
            active_runs: active_runs as u32,
            active_clients: active_clients as u32,
            fingerprint,
        }
    }

    pub async fn get_bootstrap_state(&self) -> BootstrapStateDto {
        self.get_bootstrap_state_for(0, None).await
    }

    pub async fn get_bootstrap_state_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
    ) -> BootstrapStateDto {
        let guard = self.inner.lock().await;
        let secrets = self.current_secrets();
        let target_sid = guard.resolve_session_id(session_id, conn_id);
        let slot = target_sid.and_then(|sid| guard.sessions.get(&sid));
        let active_run = slot.and_then(|s| s.active_run_id);
        let active_run_id = active_run.map(|id| id.0.to_string());
        let (current_mode, pending_mode) = self.active_mode_snapshot_for(active_run);

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

        let effective_profile = slot
            .map(|s| s.prepared.session.profile.clone())
            .or_else(|| cfg.profile.clone());
        let effective_workspace = slot
            .map(|s| s.prepared.session.workspace.clone())
            .unwrap_or_else(|| cfg.cwd.clone());
        let effective_perm = slot
            .map(|s| s.permissions_mode)
            .unwrap_or(cfg.permissions_mode);

        let selected = effective_profile
            .as_ref()
            .and_then(|name| cfg.profiles.get(name));
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

        let session = slot
            .map(|s| build_active_session_dto(&s.prepared.session, s.last_usage.clone(), &secrets));

        let work_mode = current_mode
            .or_else(|| slot.map(|s| s.prepared.session.work_mode))
            .unwrap_or(cfg.mode)
            .into();

        BootstrapStateDto {
            version: env!("CARGO_PKG_VERSION").into(),
            config_ready: true,
            config_path: redaction::redact_str(&cfg.config_path.to_string_lossy(), &secrets),
            config_error: None,
            workspace: redaction::redact_str(&effective_workspace.to_string_lossy(), &secrets),
            selected_profile: effective_profile
                .as_deref()
                .map(|p| redaction::redact_str(p, &secrets)),
            selected_model: selected.map(|p| redaction::redact_str(&p.model, &secrets)),
            context_window_tokens: selected.map(|p| p.context_window_tokens),
            thinking_effort: selected
                .and_then(|p| p.thinking_effort)
                .map(|e| e.as_str().to_owned()),
            work_mode,
            pending_mode,
            permission_mode: effective_perm.into(),
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

    fn active_mode_snapshot_for(
        &self,
        run_id: Option<RunId>,
    ) -> (Option<WorkMode>, Option<WorkModeDto>) {
        let Some(run_id) = run_id else {
            return (None, None);
        };
        let Ok(guard) = self.active_controls.lock() else {
            return (None, None);
        };
        let Some(control) = guard.get(&run_id) else {
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

    /// Cross-session token usage aggregated over `days` local-timezone days.
    /// `utc_offset_minutes` is positive east of UTC (e.g. `+480` for UTC+8, matching
    /// `-new Date().getTimezoneOffset()` in JavaScript). Defaults to `0` (UTC) when omitted.
    pub async fn usage_stats(
        &self,
        days: Option<u32>,
        utc_offset_minutes: Option<i32>,
    ) -> Result<UsageStatsDto, DesktopError> {
        let range_days = days.unwrap_or(30).clamp(1, 365);
        let offset_secs = i64::from(utc_offset_minutes.unwrap_or(0).clamp(-840, 840)) * 60;
        let cfg = self.require_config().await?;
        let secrets = self.current_secrets();
        let store = offload_boundary(move || {
            session::open(&cfg).map_err(|err| BoundaryError::new("storage_error", err.to_string()))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        const DAY: i64 = 86_400;
        let local_day_start = |ts: i64| -> i64 {
            let shifted = ts.saturating_add(offset_secs);
            (shifted - shifted.rem_euclid(DAY)) - offset_secs
        };
        let today_start = local_day_start(now);
        let since = today_start - i64::from(range_days.saturating_sub(1)) * DAY;
        let records = store
            .usage_metrics_since(since)
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        let mut totals = UsageTotalsDto::default();
        let mut purposes: Vec<(&'static str, UsageTotalsDto)> = vec![
            ("execution", UsageTotalsDto::default()),
            ("review", UsageTotalsDto::default()),
            ("compaction", UsageTotalsDto::default()),
        ];
        let mut days: std::collections::BTreeMap<i64, UsageDayDto> =
            std::collections::BTreeMap::new();

        for record in records {
            totals.runs = totals.runs.saturating_add(1);
            totals.requests = totals.requests.saturating_add(
                record
                    .metrics
                    .execution_requests
                    .saturating_add(record.metrics.review_requests)
                    .saturating_add(record.metrics.compaction_requests),
            );
            let day_key = local_day_start(record.started_at);
            let usages = [
                (
                    0usize,
                    record.metrics.execution_requests,
                    &record.metrics.execution_usage,
                ),
                (
                    1,
                    record.metrics.review_requests,
                    &record.metrics.review_usage,
                ),
                (
                    2,
                    record.metrics.compaction_requests,
                    &record.metrics.compaction_usage,
                ),
            ];
            for (idx, requests, usage) in usages {
                if requests > 0 {
                    purposes[idx].1.requests = purposes[idx].1.requests.saturating_add(requests);
                    purposes[idx].1.runs = purposes[idx].1.runs.saturating_add(1);
                }
                let Some(usage) = usage else { continue };
                fold_usage(&mut purposes[idx].1, usage);
                fold_usage(&mut totals, usage);
                let day = days.entry(day_key).or_insert(UsageDayDto {
                    day_start: day_key,
                    input_tokens: 0,
                    output_tokens: 0,
                    cached_input_tokens: 0,
                    estimated: false,
                });
                day.input_tokens = day
                    .input_tokens
                    .saturating_add(usage.input_tokens.unwrap_or(0));
                day.output_tokens = day
                    .output_tokens
                    .saturating_add(usage.output_tokens.unwrap_or(0));
                day.cached_input_tokens = day
                    .cached_input_tokens
                    .saturating_add(usage.cached_input_tokens.unwrap_or(0));
                day.estimated |= usage.estimated;
            }
        }

        Ok(UsageStatsDto {
            range_days,
            generated_at: now,
            totals,
            by_purpose: purposes
                .into_iter()
                .map(|(purpose, totals)| UsagePurposeDto {
                    purpose: purpose.to_string(),
                    totals,
                })
                .collect(),
            days: days.into_values().collect(),
        })
    }

    /// Create a new session (`new_session`). `cwd`/`profile` let interactive
    /// clients pin the session's workspace and profile to what they resolved
    /// locally — the helper may have been spawned under a different cwd or
    /// default profile.
    pub async fn new_session(
        &self,
        cwd: Option<String>,
        profile: Option<String>,
    ) -> Result<ActiveSessionDto, DesktopError> {
        self.new_session_for(0, cwd, profile).await
    }

    pub async fn new_session_for(
        &self,
        conn_id: u64,
        cwd: Option<String>,
        profile: Option<String>,
    ) -> Result<ActiveSessionDto, DesktopError> {
        let secrets = self.current_secrets();
        let base_cfg = self.require_config().await?;
        let mut session_cfg = base_cfg.clone();
        // Per-session overrides must never overwrite the daemon default config.
        if let Some(cwd) = cwd {
            session_cfg.cwd = PathBuf::from(cwd);
        }
        if let Some(profile) = profile {
            session_cfg.profile = Some(profile);
        }
        let new_sess = soyo_app::new_session(&session_cfg).map_err(|err| {
            DesktopError::new(
                "invalid_config",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;
        let prepared = session::create(&session_cfg, new_sess)
            .await
            .map_err(|err| {
                DesktopError::new(
                    "storage_error",
                    redaction::redact_str(&err.to_string(), &secrets),
                )
            })?;
        let sid = prepared.session.id;
        let dto = build_active_session_dto(&prepared.session, None, &secrets);
        self.sync_attachment_scope(Some(&prepared));
        let mut guard = self.inner.lock().await;
        guard.sessions.insert(
            sid,
            SessionSlot {
                prepared,
                active_run_id: None,
                last_usage: None,
                owner_conn_id: Some(conn_id),
                permissions_mode: session_cfg.permissions_mode,
                release_after_run: false,
            },
        );
        guard.connection_sessions.insert(conn_id, sid);
        guard.evict_idle_sessions();
        Ok(dto)
    }

    fn resolve_run_config(
        &self,
        base_cfg: &Config,
        caller_env: Option<BTreeMap<String, String>>,
    ) -> Result<Config, DesktopError> {
        let secrets = self.current_secrets();
        let Some(env_map) = caller_env else {
            return Ok(base_cfg.clone());
        };
        let mut env = Environment::from_pairs(
            env_map
                .into_iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v))),
        );
        let overlay = crate::envfile::load_env_pairs_for(&base_cfg.state_dir);
        let overlay_secrets = crate::envfile::load_env_secret_values_for(&base_cfg.state_dir);
        let _ = self.secrets.register(&overlay_secrets);
        env.merge_missing(overlay);
        let overrides = Overrides {
            config: Some(base_cfg.config_path.clone()),
            data_dir: Some(base_cfg.data_dir.clone()),
            profile: base_cfg.profile.clone(),
            mode: Some(base_cfg.mode),
            permissions: Some(base_cfg.permissions_mode),
            output: Some(base_cfg.output),
            cwd: Some(base_cfg.cwd.clone()),
        };
        let mut cfg = config::load(overrides, env, &base_cfg.cwd).map_err(|err| {
            DesktopError::new(
                "invalid_config",
                redaction::redact_str(err.message(), &secrets),
            )
        })?;
        cfg.sandbox.backend = base_cfg.sandbox.backend;
        cfg.memory = base_cfg.memory.clone();
        cfg.context.auto_compact = base_cfg.context.auto_compact;
        cfg.context.trigger_tokens = base_cfg.context.trigger_tokens;
        for (name, server) in &base_cfg.mcp.servers {
            if let Some(target) = cfg.mcp.servers.get_mut(name) {
                target.enabled = server.enabled;
            }
        }
        register_config_secrets(&cfg, &self.secrets);
        Ok(cfg)
    }

    pub async fn submit_prompt(
        &self,
        params: SubmitPromptParams,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        self.submit_prompt_for(ClientOwner::default(), params, sink)
            .await
    }

    pub async fn submit_prompt_for(
        &self,
        owner: ClientOwner,
        params: SubmitPromptParams,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let trimmed = params.prompt.trim();
        if trimmed.is_empty() {
            return Err(DesktopError::new("invalid_input", "Prompt cannot be empty"));
        }
        let expanded = expand_attachments(&params.prompt, &params.attachments);
        let secrets = self.current_secrets();

        let target_sid: Option<SessionId>;
        let run_cfg: Option<Config>;
        {
            let mut guard = self.inner.lock().await;
            let bound_sid = guard.connection_sessions.get(&owner.conn_id).copied();

            if let Some(wanted_raw) = &params.session_id {
                let wanted_sid = session::parse_id(wanted_raw).map_err(|err| {
                    DesktopError::new(
                        "invalid_session_id",
                        redaction::redact_str(&err.to_string(), &secrets),
                    )
                })?;

                // Check whether another connection already owns an active run for this session.
                if let Some(slot) = guard.sessions.get(&wanted_sid)
                    && let Some(active_run) = slot.active_run_id
                {
                    let control_owner = self
                        .active_controls
                        .lock()
                        .ok()
                        .and_then(|c| c.get(&active_run).map(|a| a.owner_conn_id));
                    if control_owner != Some(owner.conn_id) {
                        return Err(DesktopError::new(
                            "session_changed",
                            "Target session has an active run owned by another client",
                        ));
                    }
                }

                // If this connection is already bound to a different session, fail closed
                // with `session_changed` instead of silently cross-steering or jumping.
                if let Some(bound) = bound_sid
                    && bound != wanted_sid
                    && guard.sessions.contains_key(&bound)
                {
                    return Err(DesktopError::new(
                        "session_changed",
                        "Another session is active on this connection; reopen yours and retry",
                    ));
                }

                if !guard.sessions.contains_key(&wanted_sid) {
                    let cfg = guard.config.clone().ok_or_else(|| {
                        DesktopError::new(
                            "invalid_config",
                            guard
                                .config_error
                                .as_deref()
                                .unwrap_or("Configuration is not loaded"),
                        )
                    })?;
                    let prepared = session::resume(
                        &cfg,
                        wanted_raw,
                        session::ResumeOptions {
                            accept_config_change: false,
                            cwd_explicit: false,
                            profile_explicit: false,
                            mode_override: None,
                        },
                    )
                    .await
                    .map_err(|err| {
                        DesktopError::new(
                            "resume_failed",
                            redaction::redact_str(&err.to_string(), &secrets),
                        )
                    })?;
                    self.sync_attachment_scope(Some(&prepared));
                    guard.sessions.insert(
                        wanted_sid,
                        SessionSlot {
                            prepared,
                            active_run_id: None,
                            last_usage: None,
                            owner_conn_id: Some(owner.conn_id),
                            permissions_mode: cfg.permissions_mode,
                            release_after_run: false,
                        },
                    );
                }
                guard.connection_sessions.insert(owner.conn_id, wanted_sid);
                target_sid = Some(wanted_sid);
            } else {
                target_sid = guard.resolve_session_id(None, owner.conn_id);
            }

            // Fast-path: steer only the active run of `target_sid` owned by `owner.conn_id`.
            if let Some(sid) = target_sid
                && let Some(slot) = guard.sessions.get(&sid)
                && let Some(run_id) = slot.active_run_id
            {
                let control_guard = self
                    .active_controls
                    .lock()
                    .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
                if let Some(control) = control_guard.get(&run_id) {
                    if control.owner_conn_id != owner.conn_id {
                        return Err(DesktopError::new(
                            "session_changed",
                            "Cannot steer a run owned by another client connection",
                        ));
                    }
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
                    handle
                        .steer(expanded.clone())
                        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                    return Ok(SubmitPromptResultDto {
                        action: "steered".into(),
                        session_id: sid.0.to_string(),
                        run_id: control.run_id.0.to_string(),
                        recovered_runs: None,
                        recovered_tools: None,
                    });
                }
            }

            run_cfg = if let Some(env_snapshot) = params.env {
                let base_cfg = guard.config.clone().ok_or_else(|| {
                    DesktopError::new(
                        "invalid_config",
                        guard
                            .config_error
                            .as_deref()
                            .unwrap_or("Configuration is not loaded"),
                    )
                })?;
                Some(self.resolve_run_config(&base_cfg, Some(env_snapshot))?)
            } else {
                None
            };
        }

        let mut req = StartRunRequest::desktop(Some(expanded), RunKind::Execution);
        req.raw_events = params.raw_events;
        req.capture_media = params.capture_media;
        req.anthropic_prompt_cache = params.anthropic_prompt_cache;
        req.config = run_cfg;
        req.personal_context = params
            .personal_context
            .map(|p| PersonalContext {
                preferences: p.preferences,
                memory: p.memory,
            })
            .unwrap_or_default();
        self.start_run_ex(owner, target_sid, req, sink).await
    }

    /// Socket-backed `soyo run`: headless-or-interactive execution that keeps
    /// the CLI's session prep, resume options, approval files, personal
    /// context, caller environment, and raw schema-event output on the shared runtime.
    pub async fn run_prompt(
        &self,
        params: RunPromptParams,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        self.run_prompt_for(ClientOwner::default(), params, sink)
            .await
    }

    pub async fn run_prompt_for(
        &self,
        owner: ClientOwner,
        params: RunPromptParams,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let secrets = self.current_secrets();
        if params.prompt.trim().is_empty() {
            return Err(DesktopError::new("invalid_input", "Prompt cannot be empty"));
        }
        let log_level = match params.log_level.as_deref() {
            Some(raw) => Some(raw.parse::<LogLevel>().map_err(|_| {
                DesktopError::new("invalid_params", format!("Invalid log_level: {raw}"))
            })?),
            None => None,
        };
        let expanded = expand_attachments(&params.prompt, &params.attachments);

        let (cfg, workspace, sid, recovered_runs, recovered_tools) = {
            let active_count = self.active_controls.lock().map(|c| c.len()).unwrap_or(0);
            if active_count >= MAX_CONCURRENT_RUNS {
                return Err(DesktopError::new(
                    "run_active",
                    "Maximum concurrent run limit (4) reached",
                ));
            }
            let mut guard = self.inner.lock().await;
            let base_cfg = guard.config.clone().ok_or_else(|| {
                DesktopError::new(
                    "invalid_config",
                    guard
                        .config_error
                        .as_deref()
                        .unwrap_or("Configuration is not loaded"),
                )
            })?;
            let mut cfg = self.resolve_run_config(&base_cfg, params.env)?;
            if cfg.profile.is_none() && params.profile.is_none() {
                return Err(DesktopError::new(
                    "invalid_config",
                    "Select a configured model profile first",
                ));
            }
            if let Some(cwd) = &params.cwd {
                cfg.cwd = PathBuf::from(cwd);
            }
            if let Some(profile) = &params.profile {
                cfg.profile = Some(profile.clone());
            }
            if let Some(mode) = params.mode {
                cfg.mode = mode.into();
            }
            if let Some(permissions) = params.permissions {
                cfg.permissions_mode = permissions.into();
            }
            if params.reconcile_stream_text {
                cfg.output = Output::Text;
            }
            if cfg.profile.is_none() {
                return Err(DesktopError::new(
                    "invalid_config",
                    "Select a configured model profile first",
                ));
            }

            let prepared = if let Some(session_id) = &params.session_id {
                let parsed_id = session::parse_id(session_id).map_err(|err| {
                    DesktopError::new(
                        "invalid_session_id",
                        redaction::redact_str(&err.to_string(), &secrets),
                    )
                })?;
                if guard
                    .sessions
                    .get(&parsed_id)
                    .is_some_and(|slot| slot.active_run_id.is_some())
                {
                    return Err(DesktopError::new(
                        "run_active",
                        "This session already has an active run",
                    ));
                }
                // Release an idle prepared slot for this same session so
                // `session::resume` can acquire its SQLite session lock cleanly.
                guard.sessions.remove(&parsed_id);
                session::resume(
                    &cfg,
                    session_id,
                    session::ResumeOptions {
                        accept_config_change: params.accept_config_change,
                        cwd_explicit: params.cwd.is_some(),
                        profile_explicit: params.profile.is_some(),
                        mode_override: params.mode.map(Into::into),
                    },
                )
                .await
                .map_err(|err| {
                    if let Some(boundary) = err.downcast_ref::<BoundaryError>() {
                        DesktopError::from_boundary(boundary, &secrets)
                    } else {
                        let msg = redaction::redact_str(&err.to_string(), &secrets);
                        let code = if msg.contains("configuration changed") {
                            "config_change_requires_confirmation"
                        } else {
                            "resume_failed"
                        };
                        DesktopError::new(code, msg)
                    }
                })?
            } else {
                let new_sess = soyo_app::new_session(&cfg).map_err(|err| {
                    DesktopError::new(
                        "invalid_config",
                        redaction::redact_str(&err.to_string(), &secrets),
                    )
                })?;
                session::create(&cfg, new_sess).await.map_err(|err| {
                    DesktopError::new(
                        "storage_error",
                        redaction::redact_str(&err.to_string(), &secrets),
                    )
                })?
            };
            let recovered_runs = prepared.recovery.interrupted_runs as u32;
            let recovered_tools = prepared.recovery.unknown_tools as u32;
            let workspace = prepared.session.workspace.clone();
            let sid = prepared.session.id;
            self.sync_attachment_scope(Some(&prepared));
            guard.sessions.insert(
                sid,
                SessionSlot {
                    prepared,
                    active_run_id: None,
                    last_usage: None,
                    owner_conn_id: Some(owner.conn_id),
                    permissions_mode: cfg.permissions_mode,
                    release_after_run: true,
                },
            );
            guard.connection_sessions.insert(owner.conn_id, sid);
            guard.evict_idle_sessions();
            (cfg, workspace, sid, recovered_runs, recovered_tools)
        };

        let approvals = if let Some(path) = &params.approval_file {
            Preauthorizations::from_file(std::path::Path::new(path), &workspace).map_err(|err| {
                DesktopError::new(
                    "invalid_input",
                    redaction::redact_str(&err.to_string(), &secrets),
                )
            })?
        } else {
            Preauthorizations::default()
        };

        self.start_run_ex(
            owner,
            Some(sid),
            StartRunRequest {
                expanded_prompt: Some(expanded),
                kind: RunKind::Execution,
                interactive: params.interactive,
                approvals,
                personal_context: params
                    .personal_context
                    .map(|p| PersonalContext {
                        preferences: p.preferences,
                        memory: p.memory,
                    })
                    .unwrap_or_default(),
                anthropic_prompt_cache: params.anthropic_prompt_cache,
                raw_events: true,
                capture_media: false,
                emit_session_event: true,
                config: Some(cfg),
                resumed: params.session_id.is_some(),
                reconcile_stream_text: params.reconcile_stream_text,
                log_level,
            },
            sink,
        )
        .await
        .map(|mut result| {
            result.recovered_runs = Some(recovered_runs);
            result.recovered_tools = Some(recovered_tools);
            result
        })
    }

    pub async fn compact_session(
        &self,
        raw_events: bool,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        self.compact_session_for(ClientOwner::default(), None, raw_events, sink)
            .await
    }

    pub async fn compact_session_for(
        &self,
        owner: ClientOwner,
        session_id: Option<&str>,
        raw_events: bool,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let target_sid = {
            let guard = self.inner.lock().await;
            let resolved = guard.resolve_session_id(session_id, owner.conn_id);
            if let Some(sid) = resolved
                && let Some(slot) = guard.sessions.get(&sid)
                && let Some(active_run) = slot.active_run_id
            {
                let control_guard = self
                    .active_controls
                    .lock()
                    .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
                if let Some(control) = control_guard.get(&active_run) {
                    if control.owner_conn_id != owner.conn_id {
                        return Err(DesktopError::new(
                            "run_active",
                            "Session run is owned by another client connection",
                        ));
                    }
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
                    let handle = control.handle.as_ref().ok_or_else(|| {
                        DesktopError::new("run_starting", "Run is still initializing")
                    })?;
                    let secrets = self.current_secrets();
                    handle
                        .request_compaction()
                        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
                    return Ok(SubmitPromptResultDto {
                        action: "compaction_queued".into(),
                        session_id: sid.0.to_string(),
                        run_id: control.run_id.0.to_string(),
                        recovered_runs: None,
                        recovered_tools: None,
                    });
                }
            }
            resolved
        };

        let mut req = StartRunRequest::desktop(None, RunKind::Compaction);
        req.raw_events = raw_events;
        self.start_run_ex(owner, target_sid, req, sink).await
    }

    async fn start_run_ex(
        &self,
        owner: ClientOwner,
        target_sid: Option<SessionId>,
        req: StartRunRequest,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let StartRunRequest {
            expanded_prompt,
            kind,
            interactive,
            approvals,
            personal_context,
            anthropic_prompt_cache,
            raw_events,
            capture_media,
            emit_session_event,
            config: config_override,
            resumed,
            reconcile_stream_text,
            log_level,
        } = req;
        let secrets = self.current_secrets();
        let mut guard = self.inner.lock().await;
        let mut cfg = config_override
            .or_else(|| guard.config.clone())
            .ok_or_else(|| {
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

        let session_id = match target_sid {
            Some(sid) => {
                if !guard.sessions.contains_key(&sid) {
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
                    let new_sid = prepared.session.id;
                    self.sync_attachment_scope(Some(&prepared));
                    guard.sessions.insert(
                        new_sid,
                        SessionSlot {
                            prepared,
                            active_run_id: None,
                            last_usage: None,
                            owner_conn_id: Some(owner.conn_id),
                            permissions_mode: cfg.permissions_mode,
                            release_after_run: false,
                        },
                    );
                    guard.connection_sessions.insert(owner.conn_id, new_sid);
                    new_sid
                } else {
                    sid
                }
            }
            None => {
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
                let new_sid = prepared.session.id;
                self.sync_attachment_scope(Some(&prepared));
                guard.sessions.insert(
                    new_sid,
                    SessionSlot {
                        prepared,
                        active_run_id: None,
                        last_usage: None,
                        owner_conn_id: Some(owner.conn_id),
                        permissions_mode: cfg.permissions_mode,
                        release_after_run: false,
                    },
                );
                guard.connection_sessions.insert(owner.conn_id, new_sid);
                new_sid
            }
        };

        if guard
            .sessions
            .get(&session_id)
            .is_some_and(|slot| slot.active_run_id.is_some())
        {
            return Err(DesktopError::new(
                "run_active",
                "Another run is already active in this session",
            ));
        }

        let effective_permissions = guard
            .sessions
            .get(&session_id)
            .map(|slot| slot.permissions_mode)
            .unwrap_or(cfg.permissions_mode);
        if cfg.permissions_mode != effective_permissions {
            cfg.permissions_mode = effective_permissions;
        }

        let run_id = RunId::new();
        let cancel = Arc::new(RunCancellation::new());
        {
            let mut controls = self
                .active_controls
                .lock()
                .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
            if controls.len() >= MAX_CONCURRENT_RUNS {
                return Err(DesktopError::new(
                    "run_active",
                    "Maximum concurrent run limit (4) reached",
                ));
            }
            controls.insert(
                run_id,
                ActiveControl {
                    run_id,
                    session_id,
                    owner_conn_id: owner.conn_id,
                    owner_request_id: owner.request_id,
                    kind,
                    cancel: cancel.clone(),
                    handle: None,
                    pending: None,
                },
            );
        }
        let prepared = guard.sessions.get_mut(&session_id).ok_or_else(|| {
            DesktopError::new("session_unavailable", "Prepared session is unavailable")
        })?;
        prepared.active_run_id = Some(run_id);
        prepared.owner_conn_id = Some(owner.conn_id);
        let prepared = &mut prepared.prepared;
        let session_snapshot = prepared.session.clone();
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
        // The channel is created unconditionally so the driver can poll it;
        // headless runs never wire the handlers, so no request can arrive.
        let (interaction, request_rx) =
            input_channel(16).map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let event_writer = writer.clone();

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
                confirmation: if interactive {
                    Some(interaction.clone())
                } else {
                    None
                },
                input: if interactive { Some(interaction) } else { None },
                approvals,
                personal_context,
                anthropic_prompt_cache,
                run_id: Some(run_id),
                secret_registry: Some(self.secrets.clone()),
            },
            move |model, streamed| {
                let mut inner_model = match &test_model_factory {
                    Some(factory) => factory(),
                    None => model,
                };
                if streamed && reconcile_stream_text {
                    inner_model = Arc::new(ReconcileTextModel(inner_model));
                }
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
                self.clear_active_run(&mut guard, session_id, run_id);
                return Err(DesktopError::new(
                    "runtime_build_failed",
                    redaction::redact_str(&err.to_string(), &secrets),
                ));
            }
        };

        if kind == RunKind::Execution
            && let Some(text) = &expanded_prompt
        {
            // `soyo chat` parity: pasted-media markers are captured into the
            // session media store and the rewritten text is what lands in
            // history; the runtime summary keeps the original prompt.
            let append_text = if capture_media {
                match soyo_core::media::capture_user(
                    text,
                    prepared.media_capabilities,
                    &prepared.media,
                    prepared.session.id,
                ) {
                    Ok(capture) => capture.text,
                    Err(err) => {
                        self.clear_active_run(&mut guard, session_id, run_id);
                        return Err(DesktopError::from_boundary(&err, &secrets));
                    }
                }
            } else {
                text.clone()
            };
            if let Err(err) = session::append(prepared, append_text).await {
                self.clear_active_run(&mut guard, session_id, run_id);
                return Err(DesktopError::new(
                    "storage_error",
                    redaction::redact_str(&err.to_string(), &secrets),
                ));
            }
        }

        // Mirror the CLI's pre-run line so `--output jsonl` streams start with
        // session.created / session.resumed exactly like the in-process path.
        if emit_session_event {
            let (work_mode, workspace, profile) = (
                session_snapshot.work_mode,
                session_snapshot.workspace.clone(),
                session_snapshot.profile.clone(),
            );
            let payload = if resumed {
                EventPayload::SessionResumed {
                    work_mode,
                    permissions: cfg.permissions_mode,
                    workspace,
                    profile,
                }
            } else {
                EventPayload::SessionCreated {
                    work_mode,
                    permissions: cfg.permissions_mode,
                    workspace,
                    profile,
                }
            };
            if let Err(err) = event_writer.emit(payload, None, true).await {
                self.clear_active_run(&mut guard, session_id, run_id);
                return Err(DesktopError::from_boundary(&err, &secrets));
            }
        }

        let diagnostics = if let Some(level) = log_level {
            let mut diag_secrets = Vec::new();
            for name in cfg.secret_env_names() {
                if let Ok(value) = cfg.secret(&name)
                    && !value.is_empty()
                {
                    diag_secrets.push(value.to_owned());
                }
            }
            match Diagnostics::start(JsonLog::new(&cfg.state_dir, diag_secrets).with_level(level)) {
                Ok(diag) => Some(diag),
                Err(err) => {
                    self.clear_active_run(&mut guard, session_id, run_id);
                    return Err(DesktopError::from_boundary(&err, &secrets));
                }
            }
        } else {
            None
        };

        let mut current = prepared.session.clone();
        let handle = runtime.handle();
        if let Ok(mut controls) = self.active_controls.lock()
            && let Some(active) = controls.get_mut(&run_id)
        {
            active.handle = Some(handle.clone());
        }

        let started_at = Instant::now();
        let runtime_fut = async move {
            let outcome = match kind {
                RunKind::Execution => runtime.run(&mut current).await,
                RunKind::Compaction => runtime.compact(&mut current).await,
            };
            (outcome, current)
        };
        let runtime_join = match &diagnostics {
            Some(diag) => tokio::spawn(diag.instrument(runtime_fut)),
            None => tokio::spawn(runtime_fut),
        };

        let inner_arc = self.inner.clone();
        let active_controls = self.active_controls.clone();
        let registry = self.secrets.clone();
        let model_display = redaction::redact_str(&model_name, &secrets);

        tokio::spawn(async move {
            drive_active_run(DriverParams {
                run_id,
                session_id,
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
                active_controls,
                secrets: registry,
                event_writer,
                raw_events,
                diagnostics,
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
            recovered_runs: None,
            recovered_tools: None,
        })
    }

    fn clear_active_run(&self, guard: &mut DesktopInner, session_id: SessionId, run_id: RunId) {
        if let Some(slot) = guard.sessions.get_mut(&session_id)
            && slot.active_run_id == Some(run_id)
        {
            slot.active_run_id = None;
        }
        guard.release_ephemeral_slot(session_id);
        if let Ok(mut controls) = self.active_controls.lock()
            && let Some(removed) = controls.remove(&run_id)
        {
            reject_pending_interaction(removed.pending);
        }
    }

    /// Close a pending interaction when a connection drops or a run cancels:
    /// cancel first so biased handlers observe cancellation rather than an
    /// explicit user denial, then drop any pending reply sender.
    pub(crate) fn close_pending_control(control: &mut ActiveControl) {
        let _ = control.cancel.cancel();
        drop(control.pending.take());
    }

    pub fn cancel_run(&self) -> bool {
        self.cancel_run_for(0, None)
    }

    pub fn cancel_run_for(&self, conn_id: u64, explicit_run_id: Option<&str>) -> bool {
        let Ok(mut controls) = self.active_controls.lock() else {
            return false;
        };
        if let Some(rid_raw) = explicit_run_id {
            let Some(control) = controls
                .values_mut()
                .find(|control| control.run_id.0.to_string() == rid_raw)
            else {
                return false;
            };
            Self::close_pending_control(control);
            return true;
        }
        let mut matched = false;
        for control in controls.values_mut() {
            if control.owner_conn_id == conn_id {
                Self::close_pending_control(control);
                matched = true;
            }
        }
        matched
    }

    pub fn cancel_request(&self, conn_id: u64, request_id: u64) -> bool {
        let Ok(mut controls) = self.active_controls.lock() else {
            return false;
        };
        let Some(control) = controls.values_mut().find(|control| {
            control.owner_conn_id == conn_id && control.owner_request_id == request_id
        }) else {
            return false;
        };
        Self::close_pending_control(control);
        true
    }

    pub async fn on_client_disconnected(&self, conn_id: u64) {
        if let Ok(mut controls) = self.active_controls.lock() {
            for control in controls.values_mut() {
                if control.owner_conn_id == conn_id {
                    Self::close_pending_control(control);
                }
            }
        }
        let mut guard = self.inner.lock().await;
        guard.connection_sessions.remove(&conn_id);
        for slot in guard.sessions.values_mut() {
            if slot.owner_conn_id == Some(conn_id) && slot.active_run_id.is_none() {
                slot.owner_conn_id = None;
            }
        }
    }

    pub fn reply_approval(
        &self,
        approval_id: &str,
        call_id: &str,
        approved: bool,
    ) -> Result<(), DesktopError> {
        self.reply_approval_for(0, approval_id, call_id, approved)
    }

    pub fn reply_approval_for(
        &self,
        conn_id: u64,
        approval_id: &str,
        call_id: &str,
        approved: bool,
    ) -> Result<(), DesktopError> {
        let mut controls = self
            .active_controls
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
        // Find matching approval across active controls first so we enforce ownership explicitly.
        let matching_run = controls
            .iter()
            .find_map(|(rid, control)| match &control.pending {
                Some(PendingInteraction::Approval {
                    approval_id: cur_id,
                    call_id: cur_call,
                    ..
                }) if cur_id == approval_id && cur_call == call_id => Some(*rid),
                _ => None,
            });
        let target_run = match matching_run {
            Some(rid) => rid,
            None => {
                // Fall back to connection's own pending run so mismatch errors remain informative.
                if let Some((&rid, _)) = controls.iter().find(|(_, control)| {
                    control.owner_conn_id == conn_id && control.pending.is_some()
                }) {
                    rid
                } else if controls.values().any(|c| c.owner_conn_id == conn_id) {
                    return Err(DesktopError::new(
                        "stale_approval",
                        "No pending approval request is awaiting reply",
                    ));
                } else {
                    return Err(DesktopError::new(
                        "stale_approval",
                        "No active run owns this approval request",
                    ));
                }
            }
        };

        let control = controls.get_mut(&target_run).ok_or_else(|| {
            DesktopError::new("stale_approval", "No active run owns this approval request")
        })?;
        if control.owner_conn_id != conn_id {
            return Err(DesktopError::new(
                "unauthorized_client",
                "Approval request belongs to another client connection",
            ));
        }
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

    /// `answer: None` declines the prompt — the runtime sees the same `Ok(None)`
    /// the in-process handler produces when a user dismisses the question.
    pub fn reply_input(&self, input_id: &str, answer: Option<String>) -> Result<(), DesktopError> {
        self.reply_input_for(0, input_id, answer)
    }

    pub fn reply_input_for(
        &self,
        conn_id: u64,
        input_id: &str,
        answer: Option<String>,
    ) -> Result<(), DesktopError> {
        let mut controls = self
            .active_controls
            .lock()
            .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
        let matching_run = controls
            .iter()
            .find_map(|(rid, control)| match &control.pending {
                Some(PendingInteraction::Input {
                    input_id: cur_id, ..
                }) if cur_id == input_id => Some(*rid),
                _ => None,
            });
        let target_run = match matching_run {
            Some(rid) => rid,
            None => {
                if let Some((&rid, _)) = controls.iter().find(|(_, control)| {
                    control.owner_conn_id == conn_id && control.pending.is_some()
                }) {
                    rid
                } else if controls.values().any(|c| c.owner_conn_id == conn_id) {
                    return Err(DesktopError::new(
                        "stale_input",
                        "No pending input request is awaiting reply",
                    ));
                } else {
                    return Err(DesktopError::new(
                        "stale_input",
                        "No active run owns this input request",
                    ));
                }
            }
        };

        let control = controls.get_mut(&target_run).ok_or_else(|| {
            DesktopError::new("stale_input", "No active run owns this input request")
        })?;
        if control.owner_conn_id != conn_id {
            return Err(DesktopError::new(
                "unauthorized_client",
                "Input request belongs to another client connection",
            ));
        }
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
                        if reply.send(Ok(answer)).is_err() {
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
        self.set_work_mode_for(0, None, mode).await
    }

    pub async fn set_work_mode_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
        mode: WorkModeDto,
    ) -> Result<ModeStateDto, DesktopError> {
        let target: WorkMode = mode.into();
        let secrets = self.current_secrets();
        let (cfg, target_sid, workspace, active_run) = {
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
            let sid = guard.resolve_session_id(session_id, conn_id);
            let slot = sid.and_then(|id| guard.sessions.get(&id));
            let workspace = slot
                .map(|s| s.prepared.session.workspace.clone())
                .unwrap_or_else(|| cfg.cwd.clone());
            let active_run = slot.and_then(|s| s.active_run_id);
            (cfg, sid, workspace, active_run)
        };

        offload_boundary(move || {
            session::check_roots(&cfg, &workspace, target)
                .map_err(|err| BoundaryError::new("invalid_mode_roots", err.to_string()))
        })
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;

        if let Some(run_id) = active_run {
            let mut controls = self
                .active_controls
                .lock()
                .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
            if let Some(control) = controls.get_mut(&run_id) {
                if control.owner_conn_id != conn_id {
                    return Err(DesktopError::new(
                        "unauthorized_client",
                        "Session run belongs to another client connection",
                    ));
                }
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
        if let Some(sid) = target_sid
            && let Some(slot) = guard.sessions.get_mut(&sid)
        {
            if slot.active_run_id.is_some() {
                return Err(DesktopError::new(
                    "run_active",
                    "Run became active while switching mode; retry",
                ));
            }
            let mut updated = slot.prepared.session.clone();
            updated.work_mode = target;
            slot.prepared
                .store
                .commit(&[StoreChange::Session(updated.clone())])
                .await
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
            slot.prepared.session = updated;
        } else if let Some(cfg) = &mut guard.config {
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
        self.set_permission_mode_for(0, None, permissions).await
    }

    pub async fn set_permission_mode_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
        permissions: PermissionModeDto,
    ) -> Result<PermissionModeDto, DesktopError> {
        let mut guard = self.inner.lock().await;
        let target_sid = guard.resolve_session_id(session_id, conn_id);
        let active_run = target_sid
            .and_then(|sid| guard.sessions.get(&sid))
            .and_then(|slot| slot.active_run_id);
        if let Some(run_id) = active_run {
            let controls = self
                .active_controls
                .lock()
                .map_err(|_| DesktopError::new("state_lock", "active control lock failed"))?;
            if let Some(active) = controls.get(&run_id) {
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
        if let Some(sid) = target_sid
            && let Some(slot) = guard.sessions.get_mut(&sid)
        {
            if slot.active_run_id.is_some() {
                return Err(DesktopError::new(
                    "run_active",
                    "Permission mode can only be changed while idle",
                ));
            }
            if slot.owner_conn_id.is_some_and(|o| o != conn_id) {
                return Err(DesktopError::new(
                    "unauthorized_client",
                    "Session belongs to another client connection",
                ));
            }
            slot.permissions_mode = permissions.into();
            if conn_id == 0
                && guard.sessions.len() <= 1
                && let Some(cfg) = &mut guard.config
            {
                cfg.permissions_mode = permissions.into();
            }
            return Ok(permissions);
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
        params: &ResumeSessionDto,
    ) -> Result<ResumeSessionResultDto, DesktopError> {
        self.resume_session_for(0, params).await
    }

    pub async fn resume_session_for(
        &self,
        conn_id: u64,
        params: &ResumeSessionDto,
    ) -> Result<ResumeSessionResultDto, DesktopError> {
        let secrets = self.current_secrets();
        let session_id = params.session_id.as_str();
        let parsed_id = session::parse_id(session_id).map_err(|err| {
            DesktopError::new(
                "invalid_session_id",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;

        let cfg = {
            let mut guard = self.inner.lock().await;
            if let Some(slot) = guard.sessions.get(&parsed_id) {
                if slot.active_run_id.is_some() {
                    if slot.owner_conn_id != Some(conn_id) {
                        return Err(DesktopError::new(
                            "run_active",
                            "Target session has an active run owned by another client",
                        ));
                    }
                    guard.connection_sessions.insert(conn_id, parsed_id);
                    drop(guard);
                    let bootstrap = self
                        .get_bootstrap_state_for(conn_id, Some(session_id))
                        .await;
                    return Ok(ResumeSessionResultDto {
                        bootstrap,
                        recovered_runs: 0,
                        recovered_tools: 0,
                    });
                }
                if !params.accept_config_change
                    && !params.cwd_explicit
                    && !params.profile_explicit
                    && params.mode_override.is_none()
                {
                    guard.connection_sessions.insert(conn_id, parsed_id);
                    drop(guard);
                    let bootstrap = self
                        .get_bootstrap_state_for(conn_id, Some(session_id))
                        .await;
                    return Ok(ResumeSessionResultDto {
                        bootstrap,
                        recovered_runs: 0,
                        recovered_tools: 0,
                    });
                }
                // Release idle slot before re-resuming with explicit overrides so SQLite lock is free.
                guard.sessions.remove(&parsed_id);
            }
            guard.config.clone().ok_or_else(|| {
                DesktopError::new(
                    "invalid_config",
                    guard
                        .config_error
                        .as_deref()
                        .unwrap_or("Configuration is not loaded"),
                )
            })?
        };

        let prepared = session::resume(
            &cfg,
            session_id,
            session::ResumeOptions {
                accept_config_change: params.accept_config_change,
                cwd_explicit: params.cwd_explicit,
                profile_explicit: params.profile_explicit,
                mode_override: params.mode_override.map(Into::into),
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
            self.sync_attachment_scope(Some(&prepared));
            let sid = prepared.session.id;
            guard.sessions.insert(
                sid,
                SessionSlot {
                    prepared,
                    active_run_id: None,
                    last_usage: None,
                    owner_conn_id: Some(conn_id),
                    permissions_mode: cfg.permissions_mode,
                    release_after_run: false,
                },
            );
            guard.connection_sessions.insert(conn_id, sid);
            guard.evict_idle_sessions();
        }

        let bootstrap = self
            .get_bootstrap_state_for(conn_id, Some(session_id))
            .await;
        Ok(ResumeSessionResultDto {
            bootstrap,
            recovered_runs,
            recovered_tools,
        })
    }

    /// Drop this connection's prepared session handle (`release_session`).
    /// Never fails when another session is running.
    pub async fn release_session(&self) -> Result<(), DesktopError> {
        self.release_session_for(0, None).await
    }

    pub async fn release_session_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
    ) -> Result<(), DesktopError> {
        let mut guard = self.inner.lock().await;
        let Some(sid) = guard.resolve_session_id(session_id, conn_id) else {
            guard.connection_sessions.remove(&conn_id);
            return Ok(());
        };
        if let Some(slot) = guard.sessions.get(&sid)
            && slot.active_run_id.is_some()
        {
            if slot.owner_conn_id == Some(conn_id) {
                return Err(DesktopError::new(
                    "run_active",
                    "Cancel or wait for the active run before closing the session",
                ));
            }
            guard.connection_sessions.remove(&conn_id);
            return Ok(());
        }
        guard.connection_sessions.remove(&conn_id);
        let still_referenced = guard.connection_sessions.values().any(|&id| id == sid);
        if !still_referenced {
            guard.sessions.remove(&sid);
            self.remove_attachment_scope(sid);
        }
        Ok(())
    }

    /// Live mode/compaction state for interactive clients.
    pub async fn get_run_state(&self) -> Result<RunStateDto, DesktopError> {
        self.get_run_state_for(0, None).await
    }

    pub async fn get_run_state_for(
        &self,
        conn_id: u64,
        session_id: Option<&str>,
    ) -> Result<RunStateDto, DesktopError> {
        let secrets = self.current_secrets();
        let guard = self.inner.lock().await;
        let target_sid = guard.resolve_session_id(session_id, conn_id);
        let slot = target_sid.and_then(|sid| guard.sessions.get(&sid));
        let active_run = slot.and_then(|s| s.active_run_id);
        if let Some(run_id) = active_run
            && let Ok(controls) = self.active_controls.lock()
            && let Some(control) = controls.get(&run_id)
            && let Some(handle) = &control.handle
        {
            let snapshot = handle
                .mode()
                .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
            return Ok(RunStateDto {
                current: snapshot.current.into(),
                pending: snapshot.pending.map(Into::into),
                active_run: true,
                compaction_pending: handle.compaction_pending(),
                revision: snapshot.revision,
            });
        }
        let current = slot
            .map(|s| s.prepared.session.work_mode)
            .or_else(|| guard.config.as_ref().map(|cfg| cfg.mode))
            .ok_or_else(|| {
                DesktopError::new(
                    "invalid_config",
                    guard
                        .config_error
                        .as_deref()
                        .unwrap_or("Configuration is not loaded"),
                )
            })?;
        Ok(RunStateDto {
            current: current.into(),
            pending: None,
            active_run: false,
            compaction_pending: false,
            revision: 0,
        })
    }

    /// Load one session's durable record by id (`get_session`). Returns the
    /// `Session` JSON with registered secrets redacted; null when absent.
    pub async fn get_session(&self, session_id: &str) -> Result<Value, DesktopError> {
        let secrets = self.current_secrets();
        let parsed_id = session::parse_id(session_id).map_err(|err| {
            DesktopError::new(
                "invalid_session_id",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;
        let cfg = self.require_config().await?;
        let store = session::open(&cfg).map_err(|err| {
            DesktopError::new(
                "storage_error",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;
        let session = store
            .load_session(parsed_id)
            .await
            .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let value = serde_json::to_value(&session).map_err(|err| {
            DesktopError::new(
                "storage_error",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;
        Ok(redaction::redact_value(&value, &secrets))
    }

    /// Bounded committed snapshot of a session (`read_session_snapshot`).
    /// Read-only: never creates, migrates, recovers or locks the source
    /// session. Registered secrets are redacted before returning.
    pub async fn read_session_snapshot(&self, session_id: &str) -> Result<Value, DesktopError> {
        let secrets = self.current_secrets();
        let parsed_id = session::parse_id(session_id).map_err(|err| {
            DesktopError::new(
                "invalid_session_id",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;
        let cfg = self.require_config().await?;
        let snapshot = soyo_core::sqlite::SqliteStore::read_snapshot(
            &cfg.data_dir,
            parsed_id,
            soyo_core::sqlite::SnapshotLimits::default(),
            Arc::new(RunCancellation::new()),
        )
        .await
        .map_err(|err| DesktopError::from_boundary(&err, &secrets))?;
        let value = serde_json::to_value(&snapshot).map_err(|err| {
            DesktopError::new(
                "storage_error",
                redaction::redact_str(&err.to_string(), &secrets),
            )
        })?;
        Ok(redaction::redact_value(&value, &secrets))
    }

    pub async fn shutdown(&self) {
        if let Ok(mut controls) = self.active_controls.lock() {
            for control in controls.values_mut() {
                Self::close_pending_control(control);
            }
        }
        for _ in 0..50 {
            let is_active = self
                .active_controls
                .lock()
                .map(|controls| !controls.is_empty())
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

fn reject_pending_interaction(pending: Option<PendingInteraction>) {
    match pending {
        Some(PendingInteraction::Approval {
            request: Request::Approval(_, reply),
            ..
        }) => {
            let _ = reply.send(Ok(Some(false)));
        }
        Some(PendingInteraction::Input {
            request: Request::Input(_, reply),
            ..
        }) => {
            let _ = reply.send(Ok(None));
        }
        Some(_) | None => {}
    }
}

struct DriverParams {
    run_id: RunId,
    session_id: SessionId,
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
    active_controls: Arc<StdMutex<HashMap<RunId, ActiveControl>>>,
    secrets: Arc<SecretRegistry>,
    event_writer: Arc<EventWriter>,
    /// `run_prompt` consumers get raw schema-v1 envelopes instead of DTOs.
    raw_events: bool,
    diagnostics: Option<Diagnostics>,
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
                        if let Ok(mut controls) = params.active_controls.lock()
                            && let Some(active) = controls.get_mut(&params.run_id)
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
                        let envelope = envelope_for_event(
                            params.raw_events,
                            &params.secrets,
                            event,
                            &mut tool_tracker,
                        );
                        if params.sink.send(envelope).is_err() {
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

    // On boundary failure emit the terminal `run.finished` line the in-process
    // CLI produces via `EventWriter::emit_failure`, so raw/JSONL consumers see
    // exactly one final event either way.
    match &run_outcome {
        Ok((Err(err), _)) => {
            let _ = params
                .event_writer
                .emit_failure(failed_run_result(
                    params.session_id,
                    params.run_id,
                    err.0.clone(),
                ))
                .await;
        }
        Err(join_err) => {
            let _ = params
                .event_writer
                .emit_failure(failed_run_result(
                    params.session_id,
                    params.run_id,
                    ErrorInfo {
                        code: "run_task_failed".into(),
                        message: join_err.to_string(),
                    },
                ))
                .await;
        }
        _ => {}
    }

    // Drain any events/thinking/context produced right before completion.
    while let Ok(event) = params.event_rx.try_recv() {
        if !consumer_disconnected {
            let envelope =
                envelope_for_event(params.raw_events, &params.secrets, event, &mut tool_tracker);
            if params.sink.send(envelope).is_err() {
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
    let (status_str, usage_opt, todos, final_error, needs_input_question) = match run_outcome {
        Ok((outcome, updated_session)) => {
            let todos = current_todos(&updated_session.history)
                .into_iter()
                .map(|item| TodoItemDto::from_redacted(item, &secrets))
                .collect::<Vec<_>>();
            let (status_str, is_needs_input, usage_opt, mut final_error) = match outcome {
                Ok(res) => (
                    format!("{:?}", res.status).to_lowercase(),
                    res.status == RunStatus::NeedsInput,
                    res.usage.clone(),
                    res.error.as_ref().map(|e| {
                        DesktopError::new(&e.code, redaction::redact_str(&e.message, &secrets))
                    }),
                ),
                Err(err) => (
                    "failed".into(),
                    false,
                    None,
                    Some(DesktopError::from_boundary(&err, &secrets)),
                ),
            };
            if let Err(cleanup_err) = cleanup_res
                && final_error.is_none()
            {
                final_error = Some(DesktopError::from_boundary(&cleanup_err, &secrets));
            }
            // Same clarification lookup the CLI's report_result performs on
            // session history for `needs_input` text output.
            let needs_input_question = if is_needs_input {
                updated_session
                    .history
                    .iter()
                    .rev()
                    .find_map(|item| match item {
                        HistoryItem::ToolResult { result, .. }
                            if result
                                .error
                                .as_ref()
                                .is_some_and(|e| e.code == "needs_input") =>
                        {
                            result
                                .data
                                .as_ref()
                                .map(|data| redaction::redact_str(&data.to_string(), &secrets))
                        }
                        _ => None,
                    })
            } else {
                None
            };
            let mut inner = params.inner.lock().await;
            if let Some(slot) = inner.sessions.get_mut(&params.session_id) {
                slot.prepared.session = updated_session;
                if let Some(usage) = &usage_opt {
                    slot.last_usage = Some(usage.clone());
                }
                if slot.active_run_id == Some(params.run_id) {
                    slot.active_run_id = None;
                }
            }
            inner.release_ephemeral_slot(params.session_id);
            (
                status_str,
                usage_opt,
                todos,
                final_error,
                needs_input_question,
            )
        }
        Err(_) => {
            let mut inner = params.inner.lock().await;
            if let Some(slot) = inner.sessions.get_mut(&params.session_id)
                && slot.active_run_id == Some(params.run_id)
            {
                slot.active_run_id = None;
            }
            inner.release_ephemeral_slot(params.session_id);
            (
                "failed".into(),
                None,
                Vec::new(),
                Some(DesktopError::new(
                    "run_task_failed",
                    "Runtime task terminated unexpectedly",
                )),
                None,
            )
        }
    };

    if let Some(diag) = params.diagnostics.take() {
        let _ = run_blocking_boundary(move || diag.flush(Duration::from_secs(2))).await;
    }

    let mut compaction_pending = None;
    if let Ok(mut controls) = params.active_controls.lock()
        && let Some(removed) = controls.remove(&params.run_id)
    {
        compaction_pending = removed
            .handle
            .as_ref()
            .map(|handle| handle.compaction_pending());
        reject_pending_interaction(removed.pending);
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
            needs_input_question,
            compaction_pending,
        });
    }
}

fn envelope_for_event(
    raw_events: bool,
    registry: &SecretRegistry,
    event: Event,
    tool_tracker: &mut HashMap<String, (String, Option<Value>, Instant)>,
) -> DesktopStreamEnvelope {
    if raw_events {
        DesktopStreamEnvelope::RawSchemaEvent {
            event: serde_json::to_value(&event).unwrap_or(Value::Null),
        }
    } else {
        let secrets = registry.snapshot().unwrap_or_default();
        DesktopStreamEnvelope::SchemaEvent(convert_event(event, tool_tracker, &secrets))
    }
}

/// Terminal result emitted on boundary failure; mirrors the CLI's
/// `failed_run_result` so `run.finished` carries the same shape.
fn failed_run_result(session_id: SessionId, run_id: RunId, error: ErrorInfo) -> RunResult {
    RunResult {
        schema_version: SCHEMA_VERSION,
        session_id: Some(session_id),
        run_id: Some(run_id),
        status: RunStatus::Failed,
        stop_reason: Some(error.code.clone()),
        text: None,
        error: Some(error.clone()),
        usage: None,
        artifacts: vec![],
        persistence: Some(Persistence {
            saved: false,
            error: Some(error),
        }),
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
                .map(|err| soyo_ipc::types::ErrorInfoDto::from_redacted(err, secrets)),
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

/// Reconciles execution text deltas against the finalized `AssistantText` items
/// on `Finished` / `OutputLimitTruncated`, matching `PrintStream` in the CLI.
struct ReconcileTextModel(Arc<dyn Model>);

struct ReconcileTextStream {
    inner: Box<dyn ModelStream>,
    displayed: String,
    pending_finished: Option<ModelUpdate>,
}

impl ModelStream for ReconcileTextStream {
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            if let Some(finished) = self.pending_finished.take() {
                return Ok(Some(finished));
            }
            let item = self.inner.next(cancel).await?;
            match &item {
                Some(ModelUpdate::TextDelta(text)) => {
                    self.displayed.push_str(text);
                }
                Some(
                    ModelUpdate::Finished { items, .. }
                    | ModelUpdate::OutputLimitTruncated { items, .. },
                ) => {
                    let completed: String = items
                        .iter()
                        .filter_map(|item| match item {
                            HistoryItem::AssistantText(text) => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    let extra = if let Some(suffix) = completed.strip_prefix(&self.displayed) {
                        (!suffix.is_empty()).then(|| suffix.to_owned())
                    } else if !completed.is_empty() {
                        Some(format!("\n{completed}"))
                    } else {
                        None
                    };
                    if let Some(delta) = extra {
                        self.pending_finished = item;
                        return Ok(Some(ModelUpdate::TextDelta(delta)));
                    }
                }
                _ => {}
            }
            Ok(item)
        })
    }
}

impl Model for ReconcileTextModel {
    fn check_start(&self, request: &ModelRequest) -> Result<(), BoundaryError> {
        self.0.check_start(request)
    }

    fn request_counter(&self) -> Option<&dyn soyo_core::context_builder::RequestCounter> {
        self.0.request_counter()
    }

    fn validate_session(&self, session: &Session) -> Result<(), BoundaryError> {
        self.0.validate_session(session)
    }

    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let stream = self.0.start(request, cancel).await?;
            if request.purpose == ModelPurpose::Execution {
                Ok(Box::new(ReconcileTextStream {
                    inner: stream,
                    displayed: String::new(),
                    pending_finished: None,
                }) as Box<dyn ModelStream>)
            } else {
                Ok(stream)
            }
        })
    }
}
