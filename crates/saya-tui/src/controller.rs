//! Interface orchestration using shared application services and durable events.
#[cfg(test)]
mod compact_tests;
#[cfg(test)]
mod control_tests;
mod controls;
mod memory;
#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod message_tests;
mod messages;
#[cfg(test)]
mod palette_tests;
mod picker;
#[cfg(test)]
mod picker_tests;
#[cfg(test)]
mod service_tests;
mod services;
mod turn_summary;
use crate::{
    attachments, clipboard,
    commands::COMMANDS,
    i18n::{common, controller as text, permissions as permission_text, view as ui_text},
    input::TerminalInput,
    terminal::TerminalGuard,
    view::{self, Role, State},
};
use crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
};
#[cfg(test)]
use crossterm::event::{MouseButton, MouseEventKind};
use saya_app::{
    context::{ContextModel, ContextSnapshot, ContextSources},
    events::{EventModel, EventStore, EventWriter},
    interaction::{Request, RunCancellation, input_channel},
    session::{self, Prepared},
    thinking::{ThinkingLog, ThinkingModel},
};
use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Store},
    config::Config,
    contract::{
        BoundaryError, Event, EventPayload, HistoryItem, ModelPurpose, RunId, RunResult, Session,
        Usage, WorkMode,
    },
    runtime::{PersonalContext, RuntimeHandle},
};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type RunOutcome = (std::result::Result<RunResult, BoundaryError>, Session);
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Execution,
    Compaction,
}
/// Launch options supplied by the executable, independent of terminal rendering.
pub struct Options {
    pub personal_context: PersonalContext,
    pub anthropic_prompt_cache: bool,
    pub cwd_explicit: bool,
    pub profile_explicit: bool,
    pub mode_override: Option<WorkMode>,
}
struct Active {
    kind: RunKind,
    cancel: Arc<RunCancellation>,
    join: tokio::task::JoinHandle<RunOutcome>,
    events: mpsc::Receiver<Event>,
    events_open: bool,
    context: watch::Receiver<Option<ContextSnapshot>>,
    thinking: watch::Receiver<ThinkingLog>,
    thinking_ids: HashMap<String, u64>,
    execution_request: Option<String>,
    assistant_id: Option<u64>,
    tool_cards: HashMap<String, messages::ToolCard>,
    handle: RuntimeHandle,
    requests: mpsc::Receiver<Request>,
    requests_open: bool,
    summary: turn_summary::TurnSummary,
}
impl Drop for Active {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
struct App {
    config: Config,
    options: Options,
    secrets: Vec<String>,
    secret_registry: Arc<saya_core::credentials::SecretRegistry>,
    prepared: Option<Prepared>,
    active: Option<Active>,
    state: State,
    exit_after_run: Option<u8>,
    pending: Option<Request>,
    ui_job: Option<services::UiJob>,
    deferred_mode: Option<WorkMode>,
    queued_prompt: Option<String>,
    queued_attachments: Vec<attachments::Attachment>,
    draft_attachments: Vec<attachments::Attachment>,
    queued_compaction: bool,
    memory_pending: Option<saya_app::memory_admin::PreparedMemoryAction>,
    memory_next: Option<String>,
    memory_last_operation: Option<String>,
    copy_notice_until: Option<Instant>,
}
impl App {
    fn new(mut config: Config, options: Options) -> Result<Self> {
        if config.profile.is_none() && config.profiles.len() == 1 {
            config.profile = config.profiles.keys().next().cloned();
        }
        let secrets = config
            .secret_env_names()
            .iter()
            .map(|name| config.secret(name).map(str::to_owned))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let secret_registry = Arc::new(saya_core::credentials::SecretRegistry::default());
        secret_registry.register(
            &secrets
                .iter()
                .filter(|value| !value.is_empty())
                .cloned()
                .collect::<Vec<_>>(),
        )?;
        let mut app = Self {
            config,
            options,
            secrets,
            secret_registry,
            prepared: None,
            active: None,
            state: State::default(),
            exit_after_run: None,
            pending: None,
            ui_job: None,
            deferred_mode: None,
            queued_prompt: None,
            queued_attachments: Vec::new(),
            draft_attachments: Vec::new(),
            queued_compaction: false,
            memory_pending: None,
            memory_next: None,
            memory_last_operation: None,
            copy_notice_until: None,
        };
        app.refresh_header();
        app.reset_mcp_statuses();
        if app.config.profile.is_none() {
            app.notice(text::PROFILE_FIRST_NOTICE);
        }
        Ok(app)
    }
    fn current_secrets(&self) -> std::result::Result<Vec<String>, BoundaryError> {
        let mut secrets = self.secret_registry.snapshot()?;
        secrets.extend(self.secrets.iter().cloned());
        Ok(secrets)
    }
    fn refresh_secret_inputs(&mut self) -> std::result::Result<(), BoundaryError> {
        let secrets = self
            .config
            .secret_env_names()
            .iter()
            .map(|name| self.config.secret(name).map(str::to_owned))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        self.secret_registry.register(&secrets)?;
        self.secrets = secrets;
        Ok(())
    }
    fn safe(&self, text: &str) -> String {
        let mut value = serde_json::Value::String(text.into());
        let Ok(secrets) = self.current_secrets() else {
            return common::SENSITIVE_WITHHELD.into();
        };
        if saya_core::redaction::redact(&mut value, &secrets).is_err() {
            return common::SENSITIVE_WITHHELD.into();
        }
        value.as_str().unwrap_or(common::SENSITIVE_WITHHELD).into()
    }
    fn insert_paste(&mut self, text: &str) {
        if let Some(insertion) =
            attachments::parse_paste(text, &self.config.cwd, self.state.editor.text())
        {
            self.state.editor.insert(&insertion.text);
            self.add_attachments(insertion.attachments);
        } else {
            self.state.editor.insert_paste(text);
        }
    }
    fn clear_editor(&mut self) {
        self.state.editor.clear();
        self.draft_attachments.clear();
    }
    fn expand_attachments(&self, text: &str) -> String {
        attachments::expand(text, &self.draft_attachments)
    }
    async fn paste_clipboard(&mut self) -> Result<()> {
        let Some(home) = self.config.home() else {
            self.notice(text::CLIPBOARD_EMPTY);
            return Ok(());
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::task::spawn_blocking(move || clipboard::read(&home)),
        )
        .await
        .map_err(|_| text::CLIPBOARD_READ_TIMEOUT)?
        .map_err(|_| text::CLIPBOARD_READ_FAILED)?;
        match result {
            Ok(clipboard::Paste::Image(path)) => {
                self.insert_image_attachment(&path);
            }
            Ok(clipboard::Paste::Text(text)) => self.insert_paste(&text),
            Ok(clipboard::Paste::Empty) => self.notice(text::CLIPBOARD_EMPTY),
            Err(error) => self.notice(&text::clipboard_read_failed(&error)),
        }
        Ok(())
    }
    fn insert_image_attachment(&mut self, path: &std::path::Path) {
        let number = attachments::next_number(self.state.editor.text(), attachments::Kind::Image);
        if let Some(attachment) = attachments::image_attachment(path, number) {
            self.state.editor.insert(&attachment.token);
            self.add_attachments(vec![attachment]);
        }
    }
    fn add_attachments(&mut self, attachments: Vec<attachments::Attachment>) {
        self.draft_attachments.retain(|current| {
            !attachments
                .iter()
                .any(|attachment| attachment.token == current.token)
        });
        self.draft_attachments.extend(attachments);
    }
    fn safe_picker(&self, mut panel: view::PickerPanel) -> view::PickerPanel {
        panel.title = self.safe(&panel.title);
        panel.hint = self.safe(&panel.hint);
        for item in &mut panel.items {
            item.label = self.safe(&item.label);
            item.description = self.safe(&item.description);
            item.details = item.details.iter().map(|line| self.safe(line)).collect();
        }
        panel
    }
    fn notice(&mut self, text: &str) {
        self.state
            .push_message(Role::System, ui_text::SAYA, self.safe(text));
    }
    fn report_error(&mut self, error: Box<dyn std::error::Error>) -> Result<()> {
        if error
            .downcast_ref::<BoundaryError>()
            .is_some_and(|error| error.0.code.starts_with("storage_") || error.2.is_some())
        {
            return Err(error);
        }
        self.notice(&error.to_string());
        Ok(())
    }
    fn safe_title(&self, text: &str) -> String {
        let title = self.safe(&crate::terminal::safe_title(text));
        if self.current_secrets().map_or(true, |secrets| {
            saya_core::redaction::contains_secret(&title, &secrets)
        }) {
            common::SENSITIVE_WITHHELD.into()
        } else {
            title
        }
    }
    fn refresh_header(&mut self) {
        self.state.plugins_enabled = Some(self.config.plugins.enabled);
        self.state.workspace = self.safe(&self.config.cwd.to_string_lossy());
        self.state.profile =
            self.safe(self.config.profile.as_deref().unwrap_or(common::NO_PROFILE));
        self.state.model = self
            .config
            .profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
            .map(|p| p.model.clone())
            .unwrap_or_else(|| common::UNCONFIGURED.into());
        self.state.model = self.safe(&self.state.model);
        self.state.context_window_tokens = self
            .config
            .profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
            .map(|profile| profile.context_window_tokens);
        self.state.thinking_effort = self
            .config
            .profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
            .and_then(|profile| profile.thinking_effort)
            .map(|effort| effort.as_str().into());
        self.state.session_title = self
            .prepared
            .as_ref()
            .and_then(|prepared| {
                prepared.session.history.iter().find_map(|item| match item {
                    HistoryItem::UserText(text) => Some(text.as_str()),
                    _ => None,
                })
            })
            .map(attachments::display)
            .map(|text| self.safe_title(&text).chars().take(80).collect::<String>())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| common::NEW_SESSION.into());
        self.state.mode = format!("{:?}", self.config.mode).to_uppercase();
        self.state.permissions = match self.config.permissions_mode {
            saya_core::contract::PermissionMode::Auto => permission_text::AUTO,
            saya_core::contract::PermissionMode::Manual => permission_text::MANUAL,
            saya_core::contract::PermissionMode::Bypass => permission_text::BYPASS,
        }
        .into();
    }
    fn record_context_input(&mut self, usage: Option<Usage>) {
        self.state.context_input_tokens = usage.as_ref().and_then(|usage| usage.input_tokens);
        self.state.context_estimated = usage.is_some_and(|usage| usage.estimated);
    }
    fn reset_mcp_statuses(&mut self) {
        use saya_core::{
            contract::{MAX_MCP_STATUS_SERVERS, McpServerState, McpServerStatus, McpTransport},
            mcp::config::McpTransportSettings,
        };
        self.state.mcp_servers = self
            .config
            .mcp
            .servers
            .iter()
            .filter(|(_, server)| server.enabled)
            .take(MAX_MCP_STATUS_SERVERS)
            .map(|(name, server)| McpServerStatus {
                server: self.safe(name),
                transport: match &server.transport {
                    McpTransportSettings::Stdio(_) => McpTransport::Stdio,
                    McpTransportSettings::Http(_) => McpTransport::Http,
                },
                state: McpServerState::Configured,
                tools: 0,
            })
            .collect();
    }
    fn reset_context(&mut self) {
        self.record_context_input(None);
        self.state.context_breakdown.clear();
        self.state.context_breakdown_estimated = true;
        self.state.skills_count = None;
    }
    fn record_context_snapshot(&mut self, snapshot: ContextSnapshot) {
        self.state.context_breakdown = snapshot
            .entries
            .into_iter()
            .map(|entry| view::ContextEntry {
                label: entry.label.into(),
                tokens: entry.tokens,
            })
            .collect();
        self.state.context_breakdown_estimated = true;
        self.state.skills_count = snapshot.skills_count;
    }
    fn refresh_context(&mut self) -> bool {
        let Some(active) = &mut self.active else {
            return false;
        };
        if !active.context.has_changed().unwrap_or(true) {
            return false;
        }
        let snapshot = active.context.borrow_and_update().clone();
        if let Some(snapshot) = snapshot {
            self.record_context_snapshot(snapshot);
            return true;
        }
        false
    }
    async fn start(&mut self, text: String) -> Result<()> {
        self.start_run(text, self.draft_attachments.clone(), RunKind::Execution)
            .await
    }

    async fn compact(&mut self) -> Result<()> {
        if let Some(active) = &self.active {
            if active.cancel.is_cancelled() {
                return Err(text::COMPACT_CLEANUP_WAIT.into());
            }
            if active.kind == RunKind::Compaction {
                self.notice(text::COMPACT_RUNNING);
                return Ok(());
            }
            active.handle.request_compaction()?;
            self.notice(text::COMPACT_REQUESTED);
            return Ok(());
        }
        if self.prepared.is_none() {
            return Err(text::COMPACT_SESSION_REQUIRED.into());
        }
        if self.ui_job.is_some() {
            self.queued_compaction = true;
            self.notice(text::COMPACT_UI_WAIT);
            return Ok(());
        }
        self.start_run(
            text::COMPACT_SUMMARY.into(),
            Vec::new(),
            RunKind::Compaction,
        )
        .await
    }

    async fn start_run(
        &mut self,
        display: String,
        attachments: Vec<attachments::Attachment>,
        kind: RunKind,
    ) -> Result<()> {
        if self.active.is_some() {
            self.notice(text::RUN_ACTIVE);
            return Ok(());
        }
        if self.ui_job.is_some() {
            return Err(text::UI_WAIT_MESSAGE.into());
        }
        if self.prepared.is_none() {
            if self.config.profile.is_none() {
                return Err(text::PROFILE_SELECT_MESSAGE.into());
            }
            self.service("/new", "").await?;
            self.queued_prompt = Some(display);
            self.queued_attachments = attachments;
            return Ok(());
        }
        let text = attachments::expand(&display, &attachments);
        let display = self.safe(&display);
        self.reset_mcp_statuses();
        let prepared = self.prepared.as_mut().ok_or(text::SESSION_UNAVAILABLE)?;
        let run_id = RunId::new();
        let (sender, events) = mpsc::channel(256);
        let (context_sender, context) = watch::channel(None);
        let (thinking_sender, thinking) = watch::channel(ThinkingLog::default());
        let thinking_secrets = self.secrets.clone();
        let dynamic_credentials = self
            .config
            .mcp
            .servers
            .values()
            .any(|server| server.enabled);
        let thinking_registry = dynamic_credentials.then(|| self.secret_registry.clone());
        let context_sources = ContextSources {
            project: true,
            personal: self.options.personal_context.preferences.is_some()
                || self.options.personal_context.memory.is_some(),
        };
        let mut summary = turn_summary::TurnSummary::new(self.state.model.clone());
        let writer = EventWriter::new(
            prepared.store.clone(),
            prepared.session.id,
            run_id,
            self.secrets.clone(),
            Arc::new(summary.sink(sender)),
        );
        let writer = Arc::new(if dynamic_credentials {
            writer.with_secret_registry(self.secret_registry.clone())?
        } else {
            writer
        });
        let store: Arc<dyn Store> = Arc::new(EventStore(writer.clone()));
        let cancel = Arc::new(RunCancellation::new());
        let (interaction, requests) = input_channel(16)?;
        let (mut runtime, _) = saya_app::build_runtime(
            &self.config,
            &prepared.session,
            &text,
            saya_app::Launch {
                permissions: self.config.permissions_mode,
                store,
                cancellation: cancel.clone(),
                confirmation: Some(interaction.clone()),
                input: Some(interaction),
                approvals: Preauthorizations::default(),
                personal_context: self.options.personal_context.clone(),
                anthropic_prompt_cache: self.options.anthropic_prompt_cache,
                run_id: Some(run_id),
                secret_registry: Some(self.secret_registry.clone()),
            },
            move |model, _| {
                let thinking = ThinkingModel::new(model, thinking_sender, thinking_secrets);
                let thinking = match thinking_registry {
                    Some(registry) => thinking.with_secret_registry(registry),
                    None => thinking,
                };
                Arc::new(
                    ContextModel::new(
                        Arc::new(EventModel(Arc::new(thinking), writer)),
                        context_sender,
                        context_sources,
                    )
                    .with_managed_sources(true, true),
                )
            },
        )?;
        if kind == RunKind::Execution {
            session::append(prepared, text.clone())
                .await
                .map_err(|failure| {
                    match failure.downcast::<saya_core::contract::BoundaryError>() {
                        Ok(failure) => *failure,
                        Err(_) => BoundaryError(
                            saya_core::contract::ErrorInfo {
                                code: "storage_error".into(),
                                message: text::CONVERSATION_SAVE_FAILED.into(),
                            },
                            None,
                            None,
                        ),
                    }
                })?;
        }
        let mut current = prepared.session.clone();
        self.refresh_header();
        if kind == RunKind::Execution {
            self.reset_context();
            self.state.push_message(Role::User, ui_text::YOU, display);
        }
        let handle = runtime.handle();
        summary.start(Instant::now());
        let join = tokio::spawn(async move {
            let result = match kind {
                RunKind::Execution => runtime.run(&mut current).await,
                RunKind::Compaction => runtime.compact(&mut current).await,
            };
            (result, current)
        });
        self.active = Some(Active {
            kind,
            cancel,
            join,
            events,
            events_open: true,
            context,
            thinking,
            thinking_ids: HashMap::new(),
            execution_request: None,
            assistant_id: None,
            tool_cards: HashMap::new(),
            handle,
            requests,
            requests_open: true,
            summary,
        });
        self.state.status = if kind == RunKind::Compaction {
            common::COMPACTING
        } else {
            common::RUNNING
        }
        .into();
        self.refresh_activity(Instant::now());
        self.state.scroll_to_bottom();
        Ok(())
    }

    fn refresh_activity(&mut self, now: Instant) -> bool {
        let memory_changed = self.scrub_memory();
        let was_active = self.state.activity_active;
        let previous_frame = self.state.activity_frame;
        self.state.activity_active = self.active.is_some();
        if self.state.activity_visible() {
            self.state.activity_frame = self.active.as_ref().map_or(0, |active| {
                u64::try_from(active.summary.elapsed_at(now).as_millis() / 120).unwrap_or(u64::MAX)
            });
        } else if !self.state.activity_active {
            self.state.activity_frame = 0;
        }
        memory_changed
            || was_active != self.state.activity_active
            || (self.state.activity_visible() && previous_frame != self.state.activity_frame)
    }

    fn show_copy_notice(&mut self, now: Instant) {
        self.copy_notice_until = Some(now + std::time::Duration::from_millis(1500));
        self.state.copied_notice = true;
    }

    fn refresh_copy_notice(&mut self, now: Instant) -> bool {
        if self.copy_notice_until.is_some_and(|until| now < until) {
            return false;
        }
        self.copy_notice_until = None;
        std::mem::replace(&mut self.state.copied_notice, false)
    }

    fn append_run_summary(&mut self, summary: &turn_summary::TurnSummary, usage: Option<&Usage>) {
        self.state.push_message(
            Role::RunSummary,
            self.state.mode.clone(),
            self.safe(&summary.text(usage)),
        );
    }
    fn cancel(&mut self) {
        self.queued_compaction = false;
        if let Some(job) = &self.ui_job {
            job.cancel.cancel();
        }
        self.deferred_mode = None;
        let restored_prompt = self.queued_prompt.is_some();
        if let Some(prompt) = self.queued_prompt.take() {
            let attachments = std::mem::take(&mut self.queued_attachments);
            self.restore_unsent_prompt(prompt, attachments);
        }
        if self.active.is_none() {
            self.state.pending_mode = None;
        }
        self.invalidate_pending();
        self.state.command_palette = None;
        if let Some(active) = &self.active {
            active.cancel.cancel();
            self.state.status = common::CANCELLING.into();
        } else if self.ui_job.is_none() && !restored_prompt {
            self.clear_editor();
        }
    }
    async fn shutdown(&mut self) -> Result<()> {
        let cleanup = if let Some(mut active) = self.active.take() {
            active.cancel.cancel();
            active.events.close();
            active.requests.close();
            let _ = (&mut active.join).await;
            active.handle.shutdown_background().await
        } else {
            Ok(())
        };
        if let Some(mut job) = self.ui_job.take() {
            job.cancel.cancel();
            let _ = (&mut job.join).await;
        }
        cleanup.map_err(Into::into)
    }
    async fn finish(
        &mut self,
        result: std::result::Result<RunOutcome, tokio::task::JoinError>,
    ) -> Result<()> {
        if self.active.is_none() {
            return Err(text::RUN_UNAVAILABLE.into());
        }
        self.invalidate_pending();
        self.state.pending_mode = None;
        loop {
            let event = self
                .active
                .as_mut()
                .and_then(|active| active.events.try_recv().ok());
            let Some(event) = event else { break };
            self.event(event);
        }
        self.sync_thinking();
        let snapshot = self
            .active
            .as_mut()
            .and_then(|active| active.context.borrow_and_update().clone());
        if let Some(snapshot) = snapshot {
            self.record_context_snapshot(snapshot);
        }
        let compact = self
            .active
            .as_ref()
            .is_some_and(|active| active.kind == RunKind::Compaction);
        let compact_after_run = self.active.as_ref().is_some_and(|active| {
            active.kind == RunKind::Execution
                && !active.cancel.is_cancelled()
                && active.handle.compaction_pending()
        });
        let mut completed = false;
        let mut failure = None;
        let mut summary_usage = None;
        match result {
            Ok((outcome, session)) => {
                self.config.mode = session.work_mode;
                self.install_todos(saya_core::tools::todo::current_todos(&session.history));
                if let Some(prepared) = &mut self.prepared {
                    prepared.session = session;
                }
                match outcome {
                    Ok(result) => {
                        completed = result.status == saya_core::contract::RunStatus::Completed;
                        summary_usage = result.usage.clone();
                        if let Some(text) = result.text.filter(|text| !text.is_empty()) {
                            if compact {
                                self.notice(&text);
                            } else {
                                let id = self.assistant_message();
                                self.state.replace_message(id, self.safe(&text));
                            }
                        }
                        self.state.status = format!("{:?}", result.status);
                        if let Some(error) = result.error {
                            self.notice(&format!("{} — {}", error.code, error.message));
                        }
                        self.state.usage = result
                            .usage
                            .map(|usage| {
                                self.safe(&serde_json::to_string(&usage).unwrap_or_default())
                            })
                            .unwrap_or_default();
                    }
                    Err(error) => {
                        self.state.status = common::FAILED.into();
                        self.notice(&error.to_string());
                        if error.0.code.starts_with("storage_") || error.2.is_some() {
                            failure = Some(Box::new(error) as Box<dyn std::error::Error>);
                        }
                    }
                }
            }
            Err(_) => {
                let mut active = self.active.take().ok_or(text::RUN_UNAVAILABLE)?;
                active.cancel.cancel();
                active.events.close();
                active.requests.close();
                let cleanup = active.handle.shutdown_background().await;
                self.close_run_messages(&active);
                self.state.status = common::FAILED.into();
                self.notice(text::RUN_TASK_FAILED_NOTICE);
                if let Ok(mode) = active.handle.mode() {
                    self.config.mode = mode.current;
                }
                self.refresh_header();
                self.refresh_activity(Instant::now());
                if let Err(error) = cleanup {
                    self.notice(&text::background_cleanup_failed(&error));
                    self.append_run_summary(&active.summary, None);
                    return Err(Box::new(error));
                }
                self.append_run_summary(&active.summary, None);
                return Err(text::RUN_TASK_FAILED.into());
            }
        }
        let active = self.active.take().ok_or(text::RUN_UNAVAILABLE)?;
        self.close_run_messages(&active);
        self.refresh_header();
        self.refresh_activity(Instant::now());
        self.append_run_summary(&active.summary, summary_usage.as_ref());
        if let Some(error) = failure {
            return Err(error);
        }
        if compact_after_run
            && completed
            && self.exit_after_run.is_none()
            && let Err(error) = self.compact().await
        {
            self.notice(&text::compact_start_failed(&error));
        }
        Ok(())
    }
    fn mouse(&mut self, event: MouseEvent) -> bool {
        self.state.mouse(event)
    }
    async fn key(&mut self, key: KeyEvent) -> Result<bool> {
        self.refresh_completion();
        let result = self.handle_key(key).await;
        self.refresh_completion();
        result
    }
    fn refresh_completion(&mut self) {
        self.state.completion.update(
            self.state.editor.text(),
            self.state.editor.cursor(),
            self.state.command_palette.is_none()
                && self.state.memory_panel.is_none()
                && self.state.picker_panel.is_none(),
        );
    }
    async fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind == KeyEventKind::Release
            || (matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                && key.kind != KeyEventKind::Press)
        {
            return Ok(false);
        }
        if self.state.picker_panel.is_some() {
            return self.picker_key(key).await;
        }
        if self.state.memory_panel.is_some() {
            return self.memory_key(key).await;
        }
        if matches!(self.pending, Some(Request::Approval(..)))
            && (key.kind != KeyEventKind::Press
                || !(key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT))
            && matches!(
                key.code,
                KeyCode::Char('y' | 'Y' | 'n' | 'N') | KeyCode::Enter | KeyCode::Esc
            )
        {
            return Ok(false);
        }
        if key.kind == KeyEventKind::Press
            && key.modifiers.contains(KeyModifiers::SUPER)
            && matches!(key.code, KeyCode::Char('v' | 'V'))
        {
            if self.state.command_palette.is_none() {
                self.paste_clipboard().await?;
            }
            return Ok(false);
        }
        if key.kind == KeyEventKind::Press
            && key.modifiers.contains(KeyModifiers::SUPER)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            self.state.copy_text_selection();
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    if key.kind != KeyEventKind::Press || self.state.copy_text_selection() {
                        return Ok(false);
                    }
                    self.cancel();
                    return Ok(false);
                }
                KeyCode::Char('o') => {
                    if key.kind == KeyEventKind::Press
                        && self.state.command_palette.is_none()
                        && !self.state.completion.visible()
                    {
                        self.state.toggle_latest();
                    }
                    return Ok(false);
                }
                KeyCode::Char('p') => {
                    if key.kind != KeyEventKind::Press {
                        return Ok(false);
                    }
                    self.state.command_palette = if self.state.command_palette.is_some() {
                        None
                    } else {
                        Some(0)
                    };
                    return Ok(false);
                }
                KeyCode::Char('v') => {
                    if key.kind == KeyEventKind::Press && self.state.command_palette.is_none() {
                        self.paste_clipboard().await?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('d') if self.state.editor.text().is_empty() => {
                    if self.active.is_some() || self.ui_job.is_some() {
                        self.exit_after_run = Some(0);
                        self.cancel();
                        return Ok(false);
                    }
                    return Ok(true);
                }
                _ => {}
            }
        }
        if key.code == KeyCode::Esc
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            && self.state.clear_text_selection()
        {
            return Ok(false);
        }
        if let Some(selected) = self.state.command_palette {
            let selected = selected.min(COMMANDS.len().saturating_sub(1));
            match key.code {
                KeyCode::Up if key.modifiers.is_empty() => {
                    self.state.command_palette = Some(selected.saturating_sub(1));
                    return Ok(false);
                }
                KeyCode::Down if key.modifiers.is_empty() => {
                    self.state.command_palette =
                        Some(selected.saturating_add(1).min(COMMANDS.len() - 1));
                    return Ok(false);
                }
                KeyCode::Esc
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    self.state.command_palette = None;
                    return Ok(false);
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    let command = COMMANDS[selected].text;
                    self.state.command_palette = None;
                    if command.ends_with(' ') {
                        if !self.state.editor.text().is_empty() {
                            self.notice(text::CLEAR_DRAFT_FIRST);
                        } else {
                            self.state.editor.set_text(command);
                        }
                        self.show_pending();
                        return Ok(false);
                    }
                    return self.command(command).await;
                }
                _ => {}
            }
            return Ok(false);
        }
        if key.code == KeyCode::Esc
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            && self.state.completion.visible()
        {
            self.state.completion.dismiss();
            return Ok(false);
        }
        if key.code == KeyCode::Enter
            && key.modifiers.is_empty()
            && self.state.completion.visible()
            && let Some(suggestion) = self.state.completion.current()
        {
            let command = suggestion.text;
            self.state.editor.set_text(command);
            if command.ends_with(' ') {
                return Ok(false);
            }
            let exit = self.command(command).await?;
            self.clear_editor();
            return Ok(exit);
        }
        if key.code == KeyCode::Tab
            && key.modifiers.is_empty()
            && self.state.command_palette.is_none()
            && self.state.editor.text().is_empty()
        {
            if key.kind == KeyEventKind::Press {
                self.cycle_mode().await?;
            }
            return Ok(false);
        }
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Tab if self.state.completion.eligible() => {
                    self.state.completion.reopen();
                    if let Some(suggestion) = self.state.completion.current() {
                        self.state.editor.set_text(suggestion.text);
                    }
                    return Ok(false);
                }
                KeyCode::Right if self.state.completion.visible() => {
                    if let Some(suggestion) = self.state.completion.current() {
                        self.state.editor.set_text(suggestion.text);
                    }
                    return Ok(false);
                }
                KeyCode::Up if self.state.completion.visible() => {
                    self.state.completion.previous();
                    return Ok(false);
                }
                KeyCode::Down if self.state.completion.visible() => {
                    self.state.completion.next();
                    return Ok(false);
                }
                _ => {}
            }
        }
        let approval_visible = self.state.command_palette.is_none()
            && self.deferred_mode.is_none()
            && self
                .state
                .details
                .as_ref()
                .is_some_and(|details| details.title == ui_text::APPROVAL_REQUIRED);
        if approval_visible && key.kind == KeyEventKind::Press {
            if key.code == KeyCode::Esc {
                self.reply_approval(false);
                return Ok(false);
            }
            if self.state.editor.text().is_empty() {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.reply_approval(true);
                        return Ok(false);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        self.reply_approval(false);
                        return Ok(false);
                    }
                    _ => {}
                }
            }
        }
        if key.modifiers.is_empty() && key.kind != KeyEventKind::Release {
            let text = self.state.editor.text();
            let cursor = self.state.editor.cursor();
            match key.code {
                KeyCode::Left => {
                    if let Some(target) = attachments::move_left_target(text, cursor) {
                        self.state.editor.set_cursor(target);
                        return Ok(false);
                    }
                }
                KeyCode::Right => {
                    if let Some(target) = attachments::move_right_target(text, cursor) {
                        self.state.editor.set_cursor(target);
                        return Ok(false);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(range) = attachments::backspace_range(text, cursor) {
                        self.state.editor.delete_range(range.start, range.end);
                        return Ok(false);
                    }
                }
                KeyCode::Delete => {
                    if let Some(range) = attachments::delete_range(text, cursor) {
                        self.state.editor.delete_range(range.start, range.end);
                        return Ok(false);
                    }
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => {
                self.state.details = None;
                self.show_pending();
            }
            KeyCode::PageUp if self.state.details.is_some() => {
                self.state.clear_text_selection();
                self.state.details_scroll = self
                    .state
                    .details_scroll
                    .saturating_sub(self.state.details_page_rows)
            }
            KeyCode::PageDown if self.state.details.is_some() => {
                self.state.clear_text_selection();
                self.state.details_scroll = self
                    .state
                    .details_scroll
                    .saturating_add(self.state.details_page_rows)
            }
            KeyCode::End if self.state.scroll > 0 => self.state.scroll_to_bottom(),
            KeyCode::PageUp => self.state.scroll_up(10),
            KeyCode::PageDown => self.state.scroll_down(10),
            KeyCode::Enter
                if !key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                return self.submit().await;
            }
            _ => {
                self.state.editor.handle_key(key);
            }
        }
        Ok(false)
    }
}
/// Run the terminal frontend. The executable validates TTY and output routing.
/// Cancellation completes owned runs before the terminal guard is released.
pub async fn run(config: Config, options: Options) -> Result<u8> {
    if config.output != saya_core::config::Output::Text {
        return Err(text::TUI_TEXT_OUTPUT.into());
    }
    let mut app = App::new(config, options)?;
    let mut terminal = TerminalGuard::enter()?;
    let outcome = run_loop(&mut app, &mut terminal).await;
    let cleanup = app.shutdown().await;
    outcome.and_then(|code| cleanup.map(|()| code))
}
const DRAW_BACKPRESSURE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
async fn run_loop(app: &mut App, terminal: &mut TerminalGuard) -> Result<u8> {
    let mut input = TerminalInput::new()?;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(33));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut dirty = true;
    let mut input_open = true;
    let mut backpressure_since: Option<Instant> = None;
    loop {
        for _ in 0..64 {
            let event = app
                .active
                .as_mut()
                .and_then(|active| active.events.try_recv().ok());
            let Some(event) = event else { break };
            app.event(event);
            dirty = true;
        }
        enum Action {
            Input(Option<std::io::Result<TerminalEvent>>),
            Tick,
            Signal,
            Event(Option<Event>),
            Request(Option<Request>),
            UiJob(std::result::Result<services::UiResult, BoundaryError>),
            Finished(std::result::Result<RunOutcome, tokio::task::JoinError>),
        }
        dirty |= app.refresh_context();
        dirty |= app.refresh_thinking();
        dirty |= app.refresh_mode()?;
        let pending_free = app.pending.is_none();
        let action = if let Some(active) = &mut app.active {
            tokio::select! {biased;
                _=interrupt.recv()=>Action::Signal,
                _=terminate.recv()=>Action::Signal,
                _=hangup.recv()=>Action::Signal,
                event=input.next(), if input_open=>Action::Input(event),
                result=services::next_job(&mut app.ui_job)=>Action::UiJob(result),
                request=active.requests.recv(), if active.requests_open && pending_free=>Action::Request(request),
                _=tick.tick()=>Action::Tick,
                event=active.events.recv(), if active.events_open=>Action::Event(event),
                result=&mut active.join=>Action::Finished(result),
            }
        } else {
            tokio::select! {biased; _=interrupt.recv()=>Action::Signal,_=terminate.recv()=>Action::Signal,
            _=hangup.recv()=>Action::Signal,result=services::next_job(&mut app.ui_job)=>Action::UiJob(result),event=input.next(), if input_open=>Action::Input(event),_=tick.tick()=>Action::Tick}
        };
        let redraw = !matches!(&action, Action::Event(_));
        dirty |= !matches!(&action, Action::Tick);
        match action {
            Action::Input(Some(Ok(TerminalEvent::Key(key)))) => match app.key(key).await {
                Ok(true) => return Ok(0),
                Ok(false) => {}
                Err(error) => app.report_error(error)?,
            },
            Action::Input(Some(Ok(TerminalEvent::Mouse(event)))) => {
                app.mouse(event);
            }
            Action::Input(Some(Ok(TerminalEvent::Paste(text)))) => {
                if app.state.picker_panel.is_none() {
                    if let Some(panel) = &mut app.state.memory_panel {
                        panel.input.insert_paste(&text);
                    } else if app.state.command_palette.is_none() {
                        app.insert_paste(&text);
                    }
                }
            }
            Action::Input(Some(Ok(_))) | Action::Tick => {}
            Action::Input(Some(Err(_))) | Action::Input(None) => {
                input_open = false;
                if app.active.is_some() || app.ui_job.is_some() {
                    app.exit_after_run.get_or_insert(1);
                    app.cancel();
                } else {
                    return Ok(app.exit_after_run.unwrap_or(1));
                }
            }
            Action::Signal => {
                if app.active.is_some() || app.ui_job.is_some() {
                    app.exit_after_run = Some(130);
                    app.cancel();
                } else {
                    return Ok(130);
                }
            }
            Action::UiJob(result) => {
                if let Err(error) = app.finish_ui(result).await {
                    app.report_error(error)?;
                }
            }
            Action::Request(Some(request)) => app.request(request),
            Action::Request(None) => {
                if let Some(active) = &mut app.active {
                    active.requests_open = false;
                }
            }
            Action::Event(Some(event)) => app.event(event),
            Action::Event(None) => {
                if let Some(active) = &mut app.active {
                    active.events_open = false;
                }
            }
            Action::Finished(result) => {
                app.finish(result).await?;
            }
        }
        app.refresh_completion();
        dirty |= app.refresh_activity(Instant::now());
        dirty |= app.refresh_copy_notice(Instant::now());
        if let Some(text) = app.state.take_clipboard_request() {
            match terminal.copy_text(&text) {
                Ok(()) => {
                    app.show_copy_notice(Instant::now());
                    dirty = true;
                }
                Err(error) => {
                    if error.kind() != std::io::ErrorKind::InvalidInput {
                        return Err(error.into());
                    }
                    app.notice(&text::clipboard_failed(&error));
                    dirty = true;
                }
            }
        }
        if redraw && dirty && app.exit_after_run.is_none() {
            let rendered = terminal
                .set_title(&app.safe_title(&common::window_title(&app.state.session_title)))
                .and_then(|()| terminal.draw(|frame| view::draw(frame, &mut app.state)));
            match rendered {
                Ok(()) => {
                    backpressure_since = None;
                    dirty = false;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let now = Instant::now();
                    let started = *backpressure_since.get_or_insert(now);
                    if now.duration_since(started) >= DRAW_BACKPRESSURE_WAIT {
                        let error = std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            text::TERMINAL_BLOCKED,
                        );
                        if app.active.is_some() || app.ui_job.is_some() {
                            app.exit_after_run.get_or_insert(1);
                            app.cancel();
                        } else {
                            return Err(error.into());
                        }
                    }
                }
                Err(error) => {
                    if app.active.is_some() || app.ui_job.is_some() {
                        // A disconnected terminal can fail before its pending HUP
                        // is consumed. Keep servicing signals and owned cleanup.
                        app.exit_after_run.get_or_insert(1);
                        app.cancel();
                    } else {
                        return Err(error.into());
                    }
                }
            }
        }
        if app.active.is_none()
            && app.ui_job.is_none()
            && let Some(code) = app.exit_after_run
        {
            return Ok(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn refused_send_preserves_draft_and_end_resumes_output_following() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.toml");
        std::fs::write(&path, "schema_version = 1\n").unwrap();
        let config = saya_core::config::load(
            saya_core::config::Overrides {
                config: Some(path),
                ..Default::default()
            },
            saya_core::config::Environment::from_pairs([(
                std::ffi::OsString::from("HOME"),
                root.path().as_os_str().to_owned(),
            )]),
            root.path(),
        )
        .unwrap();
        let mut app = App::new(
            config,
            Options {
                personal_context: PersonalContext::default(),
                anthropic_prompt_cache: false,
                cwd_explicit: false,
                profile_explicit: false,
                mode_override: None,
            },
        )
        .unwrap();
        app.state.editor.set_text("draft 🌸");
        assert!(
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .await
                .is_err()
        );
        assert_eq!(app.state.editor.text(), "draft 🌸");
        assert!(app.prepared.is_none());
        app.state.scroll = 10;
        app.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.state.scroll, 0);
    }
}
