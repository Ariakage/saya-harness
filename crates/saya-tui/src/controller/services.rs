//! Explicit user-side session browsing and publication on bounded background jobs.
use super::*;
use crate::view::Details;
use saya_core::{
    boundary::Cancellation,
    contract::ErrorInfo,
    export::{ExportOptions, ExportScope, SessionExport},
    sqlite::{SnapshotLimits, SqliteStore},
};

pub(super) enum UiResult {
    Details(Details),
    Session { prepared: Prepared, resumed: bool },
    Export(serde_json::Value),
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
        *line = value
            .as_str()
            .unwrap_or("Sensitive details withheld")
            .into();
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
            .map_err(|_| failure("ui_worker_failed", "UI operation worker failed"))?,
        None => std::future::pending().await,
    }
}
impl App {
    pub(super) fn restore_unsent_prompt(&mut self, prompt: String) {
        if self.state.editor.text().is_empty() || self.state.editor.text() == prompt {
            self.state.editor.set_text(prompt);
        } else {
            self.state
                .push_message(Role::System, "Unsent draft", self.safe(&prompt));
            self.notice(
                "The earlier unsent prompt is retained above. Your current draft is preserved.",
            );
        }
    }
    fn require_idle(&self) -> Result<()> {
        if self.active.is_some() {
            return Err("Cancel the current run before changing sessions".into());
        }
        if self.ui_job.is_some() {
            return Err("A UI operation is active; Ctrl+C cancels it".into());
        }
        Ok(())
    }
    fn begin_ui(
        &mut self,
        cancel: Arc<RunCancellation>,
        join: tokio::task::JoinHandle<std::result::Result<UiResult, BoundaryError>>,
    ) {
        self.ui_job = Some(UiJob { cancel, join });
        self.notice("Loading UI data. Ctrl+C cancels the operation.");
    }
    pub(super) async fn service(&mut self, name: &str, argument: &str) -> Result<()> {
        if self.ui_job.is_some() {
            return Err("A UI operation is active; Ctrl+C cancels it".into());
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
                        if checking.is_cancelled() {
                            return Err(failure(
                                "cancelled",
                                "Session metadata prepared; view activation cancelled",
                            ));
                        }
                        Ok(UiResult::Session {
                            prepared,
                            resumed: false,
                        })
                    })
                });
                self.begin_ui(cancel, join);
            }
            "/profile" => {
                self.require_idle()?;
                if !self.config.profiles.contains_key(argument) {
                    return Err("Unknown configured profile; use /profiles".into());
                }
                self.config.profile = Some(argument.into());
                self.prepared = None;
                self.state.clear_messages();
                self.state.details = None;
                self.state.usage.clear();
                self.reset_context();
                self.state.status = "Ready".into();
                self.refresh_header();
                self.notice("Profile selected. New session is waiting for your message.");
            }
            "/profiles" => {
                self.state.set_details(Details {
                    title: "Configured profiles · UI".into(),
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
                        .chain(std::iter::once(
                            "Select with /profile NAME while idle".into(),
                        ))
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
                    self.notice("Session is already open and waiting for input.");
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
                        if checking.is_cancelled() {
                            return Err(failure(
                                "cancelled",
                                "Session metadata prepared; view activation cancelled",
                            ));
                        }
                        Ok(UiResult::Session {
                            prepared,
                            resumed: true,
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
                    .ok_or("Session page must be positive")?;
                let config = self.config.clone();
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let secrets = self.secrets.clone();
                let join = tokio::spawn(async move {
                    let store =
                        tokio::task::spawn_blocking(move || session::open(&config).map_err(error))
                            .await
                            .map_err(|_| {
                                failure("ui_worker_failed", "Session list worker failed")
                            })??;
                    let sessions = store.list_sessions(offset, 20).await?;
                    if checking.is_cancelled() {
                        return Err(failure("cancelled", "Session list cancelled"));
                    }
                    tokio::task::spawn_blocking(move || {
                        let mut value = serde_json::to_value(&sessions)
                            .map_err(|_| failure("ui_encode", "Cannot encode session records"))?;
                        saya_core::redaction::redact(&mut value, &secrets)?;
                        let mut lines =
                            vec![format!("Page {page} · /resume ID opens a saved session")];
                        if let Some(sessions) = value.as_array() {
                            lines.extend(sessions.iter().map(serde_json::Value::to_string));
                        }
                        let details = Details {
                            title: "Sessions · UI".into(),
                            lines,
                        };
                        display_details(details, &secrets)
                    })
                    .await
                    .map_err(|_| failure("ui_worker_failed", "Session display worker failed"))?
                });
                self.begin_ui(cancel, join);
            }
            "/tools" | "/jobs" | "/export" | "/export-full" => {
                if matches!(name, "/export" | "/export-full")
                    && let Some(active) = &self.active
                    && active.handle.mode()?.pending.is_some()
                {
                    return Err("Wait for the pending mode change before exporting".into());
                }
                let export = if matches!(name, "/export" | "/export-full") {
                    let (format, path) = argument.split_once(' ').ok_or_else(|| {
                        failure("invalid_input", "Use /export json|markdown PATH")
                    })?;
                    if !matches!(format, "json" | "markdown") || path.trim().is_empty() {
                        return Err(
                            failure("invalid_input", "Use /export json|markdown PATH").into()
                        );
                    }
                    Some((format.to_owned(), std::path::PathBuf::from(path.trim())))
                } else {
                    None
                };
                let prepared = self
                    .prepared
                    .as_ref()
                    .ok_or("Create or resume a session first")?;
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
                        let mut value = serde_json::to_value(records)
                            .map_err(|_| failure("ui_encode", "Cannot encode tool records"))?;
                        saya_core::redaction::redact(&mut value, &secrets)?;
                        let mut lines=vec![format!("Session {} · committed change {} · event {}",id.0,snapshot.last_change_seq,snapshot.last_event_seq),"Read by the UI from stored records. Tool calls are identified by run_id and call_id.".into()];
                        lines.extend(
                            serde_json::to_string_pretty(&value)
                                .map_err(|_| failure("ui_encode", "Cannot display tool records"))?
                                .lines()
                                .map(str::to_owned),
                        );
                        let title = if operation == "/tools" {
                            "Tools · UI snapshot"
                        } else {
                            "Background jobs · UI snapshot"
                        };
                            let details = Details {
                            title: title.into(),
                            lines,
                        };
                            display_details(details, &secrets)
                        })
                        .await
                        .map_err(|_| failure("ui_worker_failed", "Tool display worker failed"))?;
                    }
                    let (format, target) = export
                        .ok_or_else(|| failure("invalid_input", "Export arguments unavailable"))?;
                    let target = if target.is_absolute() {
                        target
                    } else {
                        workspace.join(target)
                    };
                    let full = operation == "/export-full";
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
                        let mut receipt = saya_app::export::publish(
                            &target,
                            &bytes,
                            &workspace,
                            mode,
                            checking.as_ref(),
                        )?;
                        receipt["last_change_seq"] = snapshot.last_change_seq.into();
                        receipt["last_event_seq"] = snapshot.last_event_seq.into();
                        Ok(UiResult::Export(receipt))
                    })
                    .await
                    .map_err(|_| failure("ui_worker_failed", "Export worker failed"))?
                });
                self.begin_ui(cancel, join);
            }
            "/cancel-job" => {
                let active = self
                    .active
                    .as_ref()
                    .ok_or("No active run owns a background job")?;
                let cancelled = active.handle.cancel_background(argument)?;
                self.notice(if cancelled {
                    "Background cancellation requested"
                } else {
                    "Background job was not found"
                });
            }
            _ => return Err("Unknown UI command".into()),
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
            Ok(UiResult::Details(_)) if cancelled => self.notice("UI query cancelled"),
            Ok(UiResult::Session { .. }) if cancelled => {
                self.notice("Session metadata prepared; view activation cancelled")
            }
            Ok(UiResult::Details(details)) => {
                self.state.set_details(details);
                self.state.details_scroll = 0;
            }
            Ok(UiResult::Session { prepared, resumed }) => {
                self.config.cwd = prepared.session.workspace.clone();
                self.config.profile = Some(prepared.session.profile.clone());
                self.config.mode = prepared.session.work_mode;
                self.state.clear_messages();
                self.state.usage.clear();
                self.reset_context();
                self.state.details = None;
                self.state.pending_mode = None;
                self.prepared = None;
                self.refresh_header();
                let history = &prepared.session.history;
                self.restore_messages(history);
                if history.len() > 512 {
                    self.notice("Earlier stored history is omitted from this view. Use session export to inspect the full history.");
                }
                self.prepared = Some(prepared);
                self.refresh_header();
                self.state.status = "Ready".into();
                self.notice(if resumed {
                    "Session restored. Waiting for new input."
                } else {
                    "New session ready. Waiting for your message."
                });
            }
            Ok(UiResult::Export(receipt)) => {
                self.notice(&format!("Exported {}", self.safe(&receipt.to_string())))
            }
            Err(error) => self.notice(&format!("{} — {}", error.0.code, error.0.message)),
        }
        if let Some(prompt) = self.queued_prompt.take() {
            if self.prepared.is_some() && !cancelled && self.active.is_none() {
                if let Err(error) = self.start(prompt.clone()).await {
                    self.restore_unsent_prompt(prompt);
                    self.state.pending_mode = None;
                    return Err(error);
                }
            } else {
                self.restore_unsent_prompt(prompt);
            }
        }
        if let Some(mode) = deferred_mode {
            self.command(&format!("/mode {mode:?}")).await?;
        }
        Ok(())
    }
}
