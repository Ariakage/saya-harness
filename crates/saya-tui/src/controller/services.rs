//! Explicit user-side session browsing and publication on bounded background jobs.
use super::*;
use crate::{
    i18n::{
        common, messages as message_text, picker as picker_text, services as text, view as ui_text,
    },
    view::Details,
};
use saya_core::{
    boundary::Cancellation,
    contract::ErrorInfo,
    export::{ExportOptions, ExportScope, SessionExport},
    sqlite::{SnapshotLimits, SqliteStore},
};

pub(super) enum UiResult {
    Details(Details),
    Session {
        prepared: Prepared,
        resumed: bool,
        usage: Option<saya_core::contract::Usage>,
    },
    Export(serde_json::Value),
    Memory(super::memory::MemoryUiResult),
    Picker(crate::view::PickerPanel),
}
pub(super) struct UiJob {
    pub cancel: Arc<RunCancellation>,
    pub join: tokio::task::JoinHandle<std::result::Result<UiResult, BoundaryError>>,
}
impl Drop for UiJob {
    fn drop(&mut self) {
        self.cancel.cancel();
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
fn error(error: Box<dyn std::error::Error>) -> BoundaryError {
    match error.downcast::<BoundaryError>() {
        Ok(error) => *error,
        Err(error) => failure("session_error", error.to_string()),
    }
}
fn display_details(
    mut details: Details,
    secrets: &[String],
) -> std::result::Result<UiResult, BoundaryError> {
    for line in std::iter::once(&mut details.title).chain(details.lines.iter_mut()) {
        let mut value = serde_json::Value::String(std::mem::take(line));
        saya_core::redaction::redact(&mut value, secrets)?;
        *line = value.as_str().unwrap_or(common::SENSITIVE_WITHHELD).into();
    }
    view::bound_details(&mut details);
    Ok(UiResult::Details(details))
}

pub(super) async fn next_job(
    job: &mut Option<UiJob>,
) -> std::result::Result<UiResult, BoundaryError> {
    match job {
        Some(job) => (&mut job.join)
            .await
            .map_err(|_| failure("ui_worker_failed", text::UI_OPERATION_WORKER_FAILED))?,
        None => std::future::pending().await,
    }
}
impl App {
    pub(super) fn restore_unsent_prompt(
        &mut self,
        prompt: String,
        attachments: Vec<attachments::Attachment>,
    ) {
        if self.state.editor.text().is_empty() || self.state.editor.text() == prompt {
            self.state.editor.set_text(prompt);
            for attachment in attachments {
                if !self
                    .draft_attachments
                    .iter()
                    .any(|current| current.token == attachment.token)
                {
                    self.draft_attachments.push(attachment);
                }
            }
        } else {
            self.state
                .push_message(Role::System, text::UNSENT_DRAFT, self.safe(&prompt));
            self.notice(text::UNSENT_DRAFT_RETAINED);
        }
    }
    fn require_idle(&self) -> Result<()> {
        if self.active.is_some() {
            return Err(text::CANCEL_BEFORE_SESSION.into());
        }
        if self.ui_job.is_some() {
            return Err(text::UI_ACTIVE.into());
        }
        Ok(())
    }
    pub(super) fn begin_ui(
        &mut self,
        cancel: Arc<RunCancellation>,
        join: tokio::task::JoinHandle<std::result::Result<UiResult, BoundaryError>>,
    ) {
        self.ui_job = Some(UiJob { cancel, join });
        self.notice(text::LOADING_UI);
    }
    pub(super) async fn service(&mut self, name: &str, argument: &str) -> Result<()> {
        if self.ui_job.is_some() {
            return Err(text::UI_ACTIVE.into());
        }
        match name {
            "/new" => {
                self.require_idle()?;
                let config = self.config.clone();
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let runtime = tokio::runtime::Handle::current();
                let join = tokio::task::spawn_blocking(move || {
                    runtime.block_on(async move {
                        let session = saya_app::new_session(&config).map_err(error)?;
                        let prepared = session::create(&config, session).await.map_err(error)?;
                        let usage = prepared
                            .store
                            .run_metrics(prepared.session.id)
                            .await
                            .map_err(|failure| error(Box::new(failure)))?
                            .into_iter()
                            .rev()
                            .find_map(|record| record.metrics.execution_usage);
                        if checking.is_cancelled() {
                            return Err(failure("cancelled", text::SESSION_PREPARED_CANCELLED));
                        }
                        Ok(UiResult::Session {
                            prepared,
                            resumed: false,
                            usage,
                        })
                    })
                });
                self.begin_ui(cancel, join);
            }
            "/model" if argument.is_empty() => {
                self.state.set_details(Details {
                    title: text::CONFIGURED_MODELS_TITLE.into(),
                    lines: self
                        .config
                        .profiles
                        .iter()
                        .map(|(name, profile)| {
                            self.safe(&format!(
                                "{name} · {} · {:?}",
                                profile.model, profile.protocol
                            ))
                        })
                        .chain(std::iter::once(text::SELECT_MODEL.into()))
                        .collect(),
                });
                self.state.details_scroll = 0;
            }
            "/profile" | "/model" => {
                self.require_idle()?;
                if !self.config.profiles.contains_key(argument) {
                    return Err(text::UNKNOWN_PROFILE.into());
                }
                self.config.profile = Some(argument.into());
                self.prepared = None;
                self.state.clear_messages();
                self.state.details = None;
                self.state.usage.clear();
                self.reset_context();
                self.state.status = common::READY.into();
                self.refresh_header();
                self.notice(text::MODEL_SELECTED);
            }
            "/profiles" => {
                self.state.set_details(Details {
                    title: text::CONFIGURED_PROFILES_TITLE.into(),
                    lines: self
                        .config
                        .profiles
                        .iter()
                        .map(|(name, profile)| {
                            self.safe(&format!(
                                "{name} · {} · {:?}",
                                profile.model, profile.protocol
                            ))
                        })
                        .chain(std::iter::once(text::SELECT_PROFILE.into()))
                        .collect(),
                });
                self.state.details_scroll = 0;
            }
            "/resume" | "/resume-accept" => {
                self.require_idle()?;
                let parsed = session::parse_id(argument)?;
                if self
                    .prepared
                    .as_ref()
                    .is_some_and(|prepared| prepared.session.id == parsed)
                {
                    self.notice(text::SESSION_ALREADY_OPEN);
                    return Ok(());
                }
                let config = self.config.clone();
                let id = argument.to_owned();
                let options = session::ResumeOptions {
                    accept_config_change: name == "/resume-accept",
                    cwd_explicit: false,
                    profile_explicit: false,
                    mode_override: None,
                };
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let runtime = tokio::runtime::Handle::current();
                let join = tokio::task::spawn_blocking(move || {
                    runtime.block_on(async move {
                        let prepared = session::resume(&config, &id, options)
                            .await
                            .map_err(error)?;
                        let usage = prepared
                            .store
                            .run_metrics(prepared.session.id)
                            .await
                            .map_err(|failure| error(Box::new(failure)))?
                            .into_iter()
                            .rev()
                            .find_map(|record| record.metrics.execution_usage);
                        if checking.is_cancelled() {
                            return Err(failure("cancelled", text::SESSION_PREPARED_CANCELLED));
                        }
                        Ok(UiResult::Session {
                            prepared,
                            resumed: true,
                            usage,
                        })
                    })
                });
                self.begin_ui(cancel, join);
            }
            "/sessions" => {
                session::check_roots(&self.config, &self.config.cwd, self.current_mode()?)?;
                let page = if argument.is_empty() {
                    1
                } else {
                    argument.parse::<u64>()?
                };
                let offset = page
                    .checked_sub(1)
                    .and_then(|page| page.checked_mul(20))
                    .ok_or(text::SESSION_PAGE_POSITIVE)?;
                let config = self.config.clone();
                let current_session = self.prepared.as_ref().map(|prepared| prepared.session.id);
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let join = tokio::spawn(async move {
                    let store =
                        tokio::task::spawn_blocking(move || session::open(&config).map_err(error))
                            .await
                            .map_err(|_| {
                                failure("ui_worker_failed", text::SESSION_LIST_WORKER_FAILED)
                            })??;
                    let sessions = store.list_sessions(offset, 20).await?;
                    if checking.is_cancelled() {
                        return Err(failure("cancelled", text::SESSION_LIST_CANCELLED));
                    }
                    let items = sessions
                        .into_iter()
                        .map(|session| {
                            let title = session
                                .title
                                .unwrap_or_else(|| text::UNTITLED_SESSION.into());
                            let status = session
                                .last_status
                                .map(|status| format!("{status:?}"))
                                .unwrap_or_else(|| text::NO_COMPLETED_RUN.into());
                            let current = current_session == Some(session.id);
                            crate::view::PickerItem {
                                id: session.id.0.to_string(),
                                label: title,
                                description: format!(
                                    "{} · {:?} · {} · {}",
                                    session.id.0, session.work_mode, session.profile, status
                                ),
                                details: vec![
                                    format!("{} {}", text::SESSION_LABEL, session.id.0),
                                    format!(
                                        "{} {}",
                                        text::WORKSPACE_LABEL,
                                        session.workspace.display()
                                    ),
                                    format!("{} {:?}", ui_text::MODE, session.work_mode),
                                    format!("{} {}", ui_text::PROFILE, session.profile),
                                    format!("{} {status}", text::LAST_STATUS_LABEL),
                                    format!("{} {}", text::UPDATED_AT_LABEL, session.updated_at),
                                    if current {
                                        picker_text::SESSION_CURRENT.into()
                                    } else {
                                        picker_text::SESSION_RESUME.into()
                                    },
                                ],
                                active: current,
                            }
                        })
                        .collect::<Vec<_>>();
                    Ok(UiResult::Picker(crate::view::PickerPanel {
                        kind: crate::view::PickerKind::Sessions,
                        title: text::sessions_title(page),
                        items,
                        selected: 0,
                        scroll: 0,
                        page_rows: 0,
                        hint: picker_text::SESSION_EMPTY.into(),
                    }))
                });
                self.begin_ui(cancel, join);
            }
            "/tools" | "/jobs" | "/export" | "/export-full" => {
                if matches!(name, "/export" | "/export-full")
                    && let Some(active) = &self.active
                    && active.handle.mode()?.pending.is_some()
                {
                    return Err(text::WAIT_MODE_EXPORT.into());
                }
                let export = if matches!(name, "/export" | "/export-full") {
                    let (full, argument) = if name == "/export-full" {
                        (true, argument)
                    } else if let Some(argument) = argument.strip_prefix("full ") {
                        (true, argument)
                    } else {
                        (false, argument)
                    };
                    let (format, path) = argument
                        .split_once(' ')
                        .ok_or_else(|| failure("invalid_input", text::EXPORT_USAGE))?;
                    if !matches!(format, "json" | "markdown") || path.trim().is_empty() {
                        return Err(failure("invalid_input", text::EXPORT_USAGE).into());
                    }
                    Some((
                        format.to_owned(),
                        std::path::PathBuf::from(path.trim()),
                        full,
                    ))
                } else {
                    None
                };
                let prepared = self.prepared.as_ref().ok_or(text::SESSION_REQUIRED)?;
                let id = prepared.session.id;
                let root = self.config.data_dir.clone();
                let secrets = self.secrets.clone();
                let workspace = prepared.session.workspace.clone();
                let mode = self.current_mode()?;
                let operation = name.to_owned();
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let join = tokio::spawn(async move {
                    let snapshot = SqliteStore::read_snapshot(
                        &root,
                        id,
                        SnapshotLimits::default(),
                        checking.clone(),
                    )
                    .await?;
                    if operation == "/tools" || operation == "/jobs" {
                        return tokio::task::spawn_blocking(move || {
                            let records = if operation == "/tools" {
                                &snapshot.tool_calls
                            } else {
                                &snapshot.background_jobs
                            };
                            let mut lines = vec![
                                text::stored_records_header(
                                    &id.0.to_string(),
                                    snapshot.last_change_seq,
                                    snapshot.last_event_seq,
                                ),
                                message_text::STORED_RECORDS_HINT.into(),
                            ];
                            if operation == "/tools" {
                                lines.extend(crate::tool_render::record_lines(records));
                            } else {
                                let mut value = serde_json::to_value(records).map_err(|_| {
                                    failure("ui_encode", text::CANNOT_ENCODE_BACKGROUND)
                                })?;
                                saya_core::redaction::redact(&mut value, &secrets)?;
                                lines.extend(
                                    serde_json::to_string_pretty(&value)
                                        .map_err(|_| {
                                            failure("ui_encode", text::CANNOT_DISPLAY_BACKGROUND)
                                        })?
                                        .lines()
                                        .map(str::to_owned),
                                );
                            }
                            let title = if operation == "/tools" {
                                text::TOOLS_TITLE
                            } else {
                                text::RUNS_TITLE
                            };
                            let details = Details {
                                title: title.into(),
                                lines,
                            };
                            display_details(details, &secrets)
                        })
                        .await
                        .map_err(|_| {
                            failure("ui_worker_failed", text::TOOL_DISPLAY_WORKER_FAILED)
                        })?;
                    }
                    let (format, target, full) = export.ok_or_else(|| {
                        failure("invalid_input", text::EXPORT_ARGUMENTS_UNAVAILABLE)
                    })?;
                    let target = if target.is_absolute() {
                        target
                    } else {
                        workspace.join(target)
                    };
                    tokio::task::spawn_blocking(move || {
                        let export = SessionExport::from_snapshot(
                            &snapshot,
                            ExportOptions {
                                scope: if full {
                                    ExportScope::Full
                                } else {
                                    ExportScope::Transcript
                                },
                                include_tools: full,
                                ..Default::default()
                            },
                            &secrets,
                        )?;
                        let bytes = if format == "json" {
                            export.to_json()?
                        } else {
                            export.to_markdown()?
                        };
                        let mut receipt = saya_app::export::publish_session(
                            &target,
                            &bytes,
                            &workspace,
                            &snapshot.session.workspace,
                            mode,
                            checking.as_ref(),
                        )?;
                        receipt["last_change_seq"] = snapshot.last_change_seq.into();
                        receipt["last_event_seq"] = snapshot.last_event_seq.into();
                        Ok(UiResult::Export(receipt))
                    })
                    .await
                    .map_err(|_| failure("ui_worker_failed", text::EXPORT_WORKER_FAILED))?
                });
                self.begin_ui(cancel, join);
            }
            "/cancel-job" => {
                let active = self.active.as_ref().ok_or(text::NO_ACTIVE_BACKGROUND)?;
                let cancelled = active.handle.cancel_background(argument)?;
                self.notice(if cancelled {
                    text::BACKGROUND_CANCEL_REQUESTED
                } else {
                    text::BACKGROUND_NOT_FOUND
                });
            }
            _ => return Err(text::UNKNOWN_UI.into()),
        }
        Ok(())
    }
    pub(super) fn current_mode(&self) -> Result<WorkMode> {
        Ok(if let Some(active) = &self.active {
            active.handle.mode()?.current
        } else {
            self.config.mode
        })
    }
    pub(super) async fn finish_ui(
        &mut self,
        result: std::result::Result<UiResult, BoundaryError>,
    ) -> Result<()> {
        let cancelled = self
            .ui_job
            .as_ref()
            .is_some_and(|job| job.cancel.is_cancelled());
        self.ui_job = None;
        let deferred_mode = self.deferred_mode.take();
        if deferred_mode.is_some() {
            self.state.pending_mode = None;
        }
        match result {
            Ok(UiResult::Memory(result)) => self.finish_memory(result, cancelled)?,
            Ok(UiResult::Picker(panel)) => {
                if cancelled {
                    self.notice(text::SELECTOR_CANCELLED);
                } else {
                    self.state.picker_panel = Some(self.safe_picker(panel));
                    self.state.details = None;
                }
            }
            Ok(UiResult::Details(_)) if cancelled => self.notice(text::UI_QUERY_CANCELLED),
            Ok(UiResult::Session { .. }) if cancelled => {
                self.notice(text::SESSION_PREPARED_CANCELLED)
            }
            Ok(UiResult::Details(details)) => {
                self.state.set_details(details);
                self.state.details_scroll = 0;
            }
            Ok(UiResult::Session {
                prepared,
                resumed,
                usage,
            }) => {
                self.config.cwd = prepared.session.workspace.clone();
                self.config.profile = Some(prepared.session.profile.clone());
                self.config.mode = prepared.session.work_mode;
                self.state.clear_messages();
                self.state.usage.clear();
                self.reset_context();
                if let Some(usage) = usage {
                    self.record_context_input(Some(usage.clone()));
                    if let Some(tokens) = usage.input_tokens {
                        self.state.context_breakdown = vec![crate::view::ContextEntry {
                            label: text::LAST_RUN.into(),
                            tokens,
                        }];
                        self.state.context_breakdown_estimated = usage.estimated;
                    }
                }
                self.state.details = None;
                self.state.pending_mode = None;
                self.prepared = None;
                self.refresh_header();
                let history = &prepared.session.history;
                self.restore_messages(history);
                if history.len() > 512 {
                    self.notice(text::HISTORY_OMITTED);
                }
                self.prepared = Some(prepared);
                self.refresh_header();
                self.state.status = common::READY.into();
                self.notice(if resumed {
                    text::SESSION_RESTORED
                } else {
                    text::NEW_SESSION_READY
                });
            }
            Ok(UiResult::Export(receipt)) => {
                self.notice(&text::exported(&self.safe(&receipt.to_string())))
            }
            Err(error) => {
                let message = self.safe(&format!("{} — {}", error.0.code, error.0.message));
                let operation = self
                    .memory_last_operation
                    .as_ref()
                    .map(|id| text::last_memory_operation(&self.safe(id)));
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.busy = false;
                    panel.lines = vec![
                        text::memory_failed(&message),
                        text::MEMORY_RECOVERY_HINT.into(),
                    ];
                    panel.scroll = 0;
                    if let Some(operation) = operation {
                        panel.lines.push(operation);
                    }
                }
                self.notice(&message);
            }
        }
        if let Some(prompt) = self.queued_prompt.take() {
            let attachments = std::mem::take(&mut self.queued_attachments);
            if self.prepared.is_some() && !cancelled && self.active.is_none() {
                if let Err(error) = self
                    .start_run(prompt.clone(), attachments.clone(), RunKind::Execution)
                    .await
                {
                    self.restore_unsent_prompt(prompt, attachments);
                    self.state.pending_mode = None;
                    return Err(error);
                }
            } else {
                self.restore_unsent_prompt(prompt, attachments);
            }
        }
        if let Some(mode) = deferred_mode {
            self.command(&format!("/mode {mode:?}")).await?;
        }
        if self.queued_compaction
            && self.active.is_none()
            && self.prepared.is_some()
            && !cancelled
            && self.exit_after_run.is_none()
        {
            self.queued_compaction = false;
            self.compact().await?;
        }
        Ok(())
    }
}
